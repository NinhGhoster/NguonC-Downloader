//! Embed resolution: HTTP/2 bootstrap POST -> AES-GCM envelope -> preissued
//! playlist -> decrypt `#ENC-AESGCM` m3u8 -> write temp file.
//! Constants are fixed in the player's public JS (see root AGENTS.md).
use crate::http;
use crate::model::{Episode, Resolved};
use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use anyhow::{bail, Context, Result};
use regex::Regex;
use sha2::{Digest, Sha256};
use hmac::{Hmac, Mac};

const BOOTSTRAP_AAD_PREFIX: &str = "stream-bootstrap-v1\n";

pub fn generate_filename(english_title: &str, title: &str, episode_num: &str, season: u32) -> String {
    let name = if english_title.is_empty() { title } else { english_title };
    let safe: String = name.chars().filter(|c| !matches!(c, '\\' | '/' | '*' | '?' | ':' | '"' | '<' | '>' | '|')).collect();
    let dotted = safe.trim().split_whitespace().collect::<Vec<_>>().join(".");
    let ep: u32 = episode_num.parse().unwrap_or(0);
    format!("{dotted}.S{season:02}E{ep:02}.mp4")
}

pub async fn resolve_one(ep: &Episode, movie_url: &str, season: u32) -> Resolved {
    let filename = generate_filename("", "", &ep.name, season); // filled by caller if needed
    match resolve_inner(ep, movie_url).await {
        Ok(path) => Resolved { num: ep.name.clone(), embed: ep.embed.clone(), m3u8: Some(path), error: None, filename },
        Err(e) => Resolved { num: ep.name.clone(), embed: ep.embed.clone(), m3u8: None, error: Some(format!("{e}")), filename },
    }
}

async fn resolve_inner(ep: &Episode, movie_url: &str) -> Result<String> {
    if !http::can_h2() {
        bail!("embed host requires HTTP/2 but no curl with --http2 is available");
    }
    let embed = &ep.embed;
    let parsed = url::Url::parse(embed)?;
    let origin = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap_or(""));

    let shell = http::fetch(embed, Some(movie_url), "GET", &[], None, 30).await?;
    if shell.status != 200 {
        bail!("HTTP {} from embed host", shell.status);
    }
    let shell_text = shell.text();
    let boot_re = Regex::new(r#"<script id="stream-bootstrap"[^>]*>(.*?)</script>"#)?;
    let boot_json = match boot_re.captures(&shell_text) {
        Some(c) => c[1].to_string(),
        None => bail!("stream-bootstrap script missing (legacy data-obf pages not yet ported)"),
    };
    let boot: serde_json::Value = serde_json::from_str(&boot_json)?;
    let api = boot["api"].as_str().context("bootstrap api missing")?;
    let api_parsed = url::Url::parse(api)?;
    if api_parsed.host_str() != parsed.host_str() {
        bail!("Invalid bootstrap api URL");
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
        &[
            ("Content-Type", "application/json"),
            ("Origin", &origin),
        ],
        Some(&body),
        30,
    )
    .await?;
    if resp.status == 429 || resp.status == 503 {
        let retry = resp.headers.get("retry-after").map(String::as_str).unwrap_or("60");
        bail!("Rate limited by embed host (retry after {retry}s)");
    }
    if resp.status != 200 {
        let detail = serde_json::from_slice::<serde_json::Value>(&resp.body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_default();
        bail!("HTTP {} from embed host (bootstrap{})", resp.status, if detail.is_empty() { String::new() } else { format!(": {detail}") });
    }

    let envelope: serde_json::Value = serde_json::from_slice(&resp.body)?;
    let opened = if envelope["format"].as_str() == Some("aesgcm-v1") {
        open_bootstrap(&envelope, api)?
    } else {
        envelope.clone()
    };

    if opened["turnstileEnabled"].as_bool() == Some(true) && opened.get("preissued").is_none() {
        bail!("Turnstile verification required (no preissued grant)");
    }
    let playlist_url = opened["preissued"]["playlist"]
        .as_str()
        .context("Bootstrap response has no preissued playlist")?;
    let pl = http::fetch(playlist_url, Some(embed), "GET", &[], None, 30).await?;
    if pl.status != 200 {
        bail!("HTTP {} fetching playlist", pl.status);
    }
    let mut text = pl.text();
    if text.contains("#ENC-AESGCM") {
        let hash_match = Regex::new(r"[?&]hash=([0-9a-f]+)")?
            .captures(embed)
            .map(|c| c[1].to_string())
            .unwrap_or_default();
        let video_hash = opened["video"].as_str().unwrap_or(&hash_match).to_string();
        text = decrypt_playlist(&text, &video_hash)?;
    }
    write_playlist(&text)
}

fn open_bootstrap(envelope: &serde_json::Value, api_href: &str) -> Result<serde_json::Value> {
    let iv_hex = envelope["iv"].as_str().context("malformed bootstrap envelope")?;
    let data_b64 = envelope["data"].as_str().context("malformed bootstrap envelope")?;
    if iv_hex.len() != 24 || !iv_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("malformed bootstrap envelope iv");
    }
    let aad = format!("{BOOTSTRAP_AAD_PREFIX}{api_href}");
    let key = Sha256::digest(aad.as_bytes());
    let key = Key::<Aes256Gcm>::from_slice(key.as_slice());
    let cipher = Aes256Gcm::new(key);
    let iv = hex_decode(iv_hex)?;
    let ct = base64_decode(data_b64)?;
    let pt = cipher
        .decrypt(Nonce::from_slice(&iv), Payload { msg: &ct, aad: aad.as_bytes() })
        .map_err(|_| anyhow::anyhow!("bootstrap decrypt failed"))?;
    Ok(serde_json::from_slice(&pt)?)
}

/// Playlist key: HMAC-SHA256(key=b"stream-derive-v1", msg=videoHash); no AAD.
fn decrypt_playlist(text: &str, video_hash: &str) -> Result<String> {
    let mut iv_hex = None;
    let mut data_line = None;
    for line in text.lines() {
        if line.contains("#ENC-AESGCM") {
            if let Some(c) = Regex::new(r"iv=([0-9a-fA-F]+)")?.captures(line) {
                iv_hex = Some(c[1].to_string());
            }
        } else if !line.starts_with('#') && !line.trim().is_empty() {
            data_line = Some(line.trim().to_string());
        }
    }
    let iv_hex = iv_hex.context("Could not parse encrypted m3u8")?;
    let data_line = data_line.context("Could not parse encrypted m3u8")?;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(b"stream-derive-v1")?;
    mac.update(video_hash.as_bytes());
    let key = mac.finalize().into_bytes();
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let pt = cipher
        .decrypt(Nonce::from_slice(&hex_decode(&iv_hex)?), base64_decode(&data_line)?.as_slice())
        .map_err(|_| anyhow::anyhow!("playlist decrypt failed"))?;
    Ok(String::from_utf8(pt)?)
}

fn write_playlist(text: &str) -> Result<String> {
    let path = std::env::temp_dir().join(format!(
        "nguonc-{}-{}.m3u8",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos()
    ));
    std::fs::write(&path, text)?;
    Ok(path.to_string_lossy().into_owned())
}

fn hex_decode(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 { bail!("odd hex length") }
    Ok((0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16)).collect::<Result<Vec<_>, _>>()?)
}

fn base64_decode(s: &str) -> Result<Vec<u8>> {
    Ok(base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s)?)
}
