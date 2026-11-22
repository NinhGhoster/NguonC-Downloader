//! Embed resolution: HTTP/2 bootstrap POST -> AES-GCM envelope -> preissued
//! playlist -> decrypt `#ENC-AESGCM` m3u8 -> write temp file.
//! Constants are fixed in the player's public JS (see root AGENTS.md).
use crate::error::{NguoncError, Result};
use crate::http;
use crate::model::{Episode, Resolved, generate_filename};
use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use hmac::{Hmac, Mac};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

const BOOTSTRAP_AAD_PREFIX: &str = "stream-bootstrap-v1\n";

fn boot_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"<script id="stream-bootstrap"[^>]*>(.*?)</script>"#)
            .expect("bootstrap regex must compile")
    })
}

fn hash_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[?&]hash=([0-9a-f]+)").expect("hash regex must compile"))
}

fn iv_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"iv=([0-9a-fA-F]+)").expect("iv regex must compile"))
}

/// Resolve one episode to a decrypted local playlist. The filename is
/// built from the movie's real titles here, not left for the caller.
pub async fn resolve_one(
    ep: &Episode,
    movie_url: &str,
    english_title: &str,
    title: &str,
    season: u32,
) -> Result<Resolved> {
    let path = resolve_inner(ep, movie_url).await?;
    Ok(Resolved {
        num: ep.name.clone(),
        embed: ep.embed.clone(),
        m3u8: path,
        filename: generate_filename(english_title, title, &ep.name, season),
    })
}

async fn resolve_inner(ep: &Episode, movie_url: &str) -> Result<String> {
    if !http::can_h2() {
        return Err(NguoncError::Other(
            "embed host requires HTTP/2 but no curl with --http2 is available".into(),
        ));
    }
    let embed = &ep.embed;
    let parsed = url::Url::parse(embed)?;
    let origin = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap_or(""));

    // VPN round-trips to the VN-only embed host can be slow; 45s leaves
    // headroom over curl's default while still failing fast on a dead link.
    let shell = http::fetch(embed, Some(movie_url), "GET", &[], None, 45).await?;
    shell.check_status(embed)?;
    let shell_text = shell.text();
    let boot_json = match boot_re().captures(&shell_text) {
        Some(c) => c[1].to_string(),
        None => {
            return Err(NguoncError::parse(
                "stream-bootstrap script missing (legacy data-obf pages not yet ported)",
            ));
        }
    };
    let boot: serde_json::Value = serde_json::from_str(&boot_json)?;
    let api = boot["api"]
        .as_str()
        .ok_or_else(|| NguoncError::parse("bootstrap api missing"))?;
    let api_parsed = url::Url::parse(api)?;
    if api_parsed.host_str() != parsed.host_str() {
        return Err(NguoncError::WrongOrigin);
    }

    let movie_origin = url::Url::parse(movie_url)
        .map(|u| format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")))
        .unwrap_or_default();
    let body = serde_json::json!({
        "action": "bootstrap",
        "referrer": movie_url.chars().take(4096).collect::<String>(),
        "frame_origins": [movie_origin],
        "request_grant": true,
        "playlist_format": "aesgcm-v2",
        "pretty_url": true,
        "path_chunks": true,
        "bootstrap_format": "aesgcm-v1",
    })
    .to_string();

    let resp = http::fetch(
        api,
        Some(embed),
        "POST",
        &[("Content-Type", "application/json"), ("Origin", &origin)],
        Some(&body),
        45,
    )
    .await?;
    if resp.status == 429 || resp.status == 503 {
        let retry = resp
            .headers
            .get("retry-after")
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);
        return Err(NguoncError::RateLimited {
            retry_after_secs: retry,
        });
    }
    if resp.status != 200 {
        let detail = serde_json::from_slice::<serde_json::Value>(&resp.body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_default();
        return Err(NguoncError::from_response(api, resp.status, &resp.body).map_detail(detail));
    }

    let envelope: serde_json::Value = serde_json::from_slice(&resp.body)?;
    let opened = if envelope["format"].as_str() == Some("aesgcm-v1") {
        open_bootstrap(&envelope, api)?
    } else {
        envelope.clone()
    };

    if opened["turnstileEnabled"].as_bool() == Some(true) && opened.get("preissued").is_none() {
        return Err(NguoncError::Turnstile);
    }
    let playlist_url = opened["preissued"]["playlist"]
        .as_str()
        .ok_or_else(|| NguoncError::parse("Bootstrap response has no preissued playlist"))?;
    let pl = http::fetch(playlist_url, Some(embed), "GET", &[], None, 45).await?;
    pl.check_status(playlist_url)?;
    let mut text = pl.text();
    if text.contains("#ENC-AESGCM") {
        let hash_match = hash_re()
            .captures(embed)
            .map(|c| c[1].to_string())
            .unwrap_or_default();
        let video_hash = opened["video"].as_str().unwrap_or(&hash_match).to_string();
        text = decrypt_playlist(&text, &video_hash)?;
    }
    write_playlist(&text)
}

fn open_bootstrap(envelope: &serde_json::Value, api_href: &str) -> Result<serde_json::Value> {
    let iv_hex = envelope["iv"]
        .as_str()
        .ok_or_else(|| NguoncError::parse("malformed bootstrap envelope"))?;
    let data_b64 = envelope["data"]
        .as_str()
        .ok_or_else(|| NguoncError::parse("malformed bootstrap envelope"))?;
    if iv_hex.len() != 24 || !iv_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(NguoncError::parse("malformed bootstrap envelope iv"));
    }
    let aad = format!("{BOOTSTRAP_AAD_PREFIX}{api_href}");
    let key = Sha256::digest(aad.as_bytes());
    let key = Key::<Aes256Gcm>::from_slice(key.as_slice());
    let cipher = Aes256Gcm::new(key);
    let iv = hex_decode(iv_hex)?;
    let ct = base64_decode(data_b64)?;
    let pt = cipher
        .decrypt(
            Nonce::from_slice(&iv),
            Payload {
                msg: &ct,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| NguoncError::parse("bootstrap decrypt failed"))?;
    Ok(serde_json::from_slice(&pt)?)
}

/// Playlist key: HMAC-SHA256(key=b"stream-derive-v1", msg=videoHash); no AAD.
fn decrypt_playlist(text: &str, video_hash: &str) -> Result<String> {
    let mut iv_hex = None;
    let mut data_line = None;
    for line in text.lines() {
        if line.contains("#ENC-AESGCM") {
            if let Some(c) = iv_re().captures(line) {
                iv_hex = Some(c[1].to_string());
            }
        } else if !line.starts_with('#') && !line.trim().is_empty() {
            data_line = Some(line.trim().to_string());
        }
    }
    let iv_hex = iv_hex.ok_or_else(|| NguoncError::parse("Could not parse encrypted m3u8"))?;
    let data_line =
        data_line.ok_or_else(|| NguoncError::parse("Could not parse encrypted m3u8"))?;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(b"stream-derive-v1")?;
    mac.update(video_hash.as_bytes());
    let key = mac.finalize().into_bytes();
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let iv = hex_decode(&iv_hex)?;
    if iv.len() != 12 {
        return Err(NguoncError::parse("Could not parse encrypted m3u8"));
    }
    let pt = cipher
        .decrypt(
            Nonce::from_slice(&iv),
            base64_decode(&data_line)?.as_slice(),
        )
        .map_err(|_| NguoncError::parse("playlist decrypt failed"))?;
    Ok(String::from_utf8(pt)?)
}

fn write_playlist(text: &str) -> Result<String> {
    let path = std::env::temp_dir().join(format!(
        "nguonc-{}-{}.m3u8",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::write(&path, text)?;
    Ok(path.to_string_lossy().into_owned())
}

fn hex_decode(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return Err(NguoncError::parse("odd hex length"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| NguoncError::parse(format!("invalid hex: {e}")))
}

fn base64_decode(s: &str) -> Result<Vec<u8>> {
    Ok(base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        s,
    )?)
}
