//! HTTP transport. The embed host hard-blocks HTTP/1.1, so all embed traffic
//! goes through system `curl --http2` (same approach as the Python engine —
//! measured: Python clients get 403 from non-VN IPs while curl with the same
//! headers gets 200).
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use tokio::fs;

pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// Result of an HTTP fetch: status code, body and response headers (lowercase keys).
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    pub headers: HashMap<String, String>,
}

impl Response {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn curl_available() -> bool {
    which("curl").is_some()
}

fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(bin))
            .find(|p| p.is_file())
    })
}

/// Probe once per process whether system curl supports HTTP/2.
fn curl_supports_http2() -> bool {
    use std::sync::OnceLock;
    static H2: OnceLock<bool> = OnceLock::new();
    *H2.get_or_init(|| {
        let curl = match which("curl") {
            Some(p) => p,
            None => return false,
        };
        if Command::new(&curl)
            .args(["-sS", "--http2", "--version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return true;
        }
        Command::new(&curl)
            .arg("--version")
            .output()
            .map(|o| {
                let s = format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                );
                s.contains("HTTP2") || s.contains("HTTP/2")
            })
            .unwrap_or(false)
    })
}

pub fn can_h2() -> bool {
    curl_available() && curl_supports_http2()
}

/// Simple GET used for public pages (movie page, JSON episodes endpoint).
/// Uses curl as well so one transport covers every host.
pub async fn get(url: &str, referer: Option<&str>) -> Result<String> {
    let resp = fetch(url, referer, "GET", &[], None, 30).await?;
    Ok(resp.text())
}

/// Full fetch via `curl --http2 -D <headerfile> -o <bodyfile>`.
pub async fn fetch(
    url: &str,
    referer: Option<&str>,
    method: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
    timeout: u64,
) -> Result<Response> {
    if !curl_available() {
        bail!("curl is not available on PATH");
    }
    if !curl_supports_http2() {
        bail!("system curl has no HTTP/2 support; embed host requires HTTP/2");
    }
    let dir = std::env::temp_dir();
    let tag = format!(
        "nguonc-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let body_path = dir.join(format!("{tag}.body"));
    let head_path = dir.join(format!("{tag}.head"));

    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "--http2", "--compressed"])
        .arg("-m")
        .arg(timeout.to_string())
        .arg("-o")
        .arg(&body_path)
        .arg("-D")
        .arg(&head_path)
        .args(["-w", "%{http_code}"])
        .args(["-H", &format!("User-Agent: {USER_AGENT}")]);
    if let Some(r) = referer {
        cmd.args(["-H", &format!("Referer: {r}")]);
    }
    for (k, v) in headers {
        cmd.args(["-H", &format!("{k}: {v}")]);
    }
    if method != "GET" {
        cmd.args(["-X", method]);
    }
    if let Some(b) = body {
        cmd.args(["--data-binary", b]);
    }
    cmd.arg(url);

    let out = tokio::process::Command::from(cmd)
        .output()
        .await
        .with_context(|| format!("failed to run curl for {url}"))?;
    let status: u16 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    let mut resp_headers = HashMap::new();
    if let Ok(head) = fs::read_to_string(&head_path).await {
        for line in head.lines() {
            if let Some((k, v)) = line.split_once(':') {
                resp_headers.insert(k.trim().to_lowercase(), v.trim().to_string());
            }
        }
    }
    let bytes = fs::read(&body_path).await.unwrap_or_default();
    let _ = fs::remove_file(&body_path).await;
    let _ = fs::remove_file(&head_path).await;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        // curl exit 28 = timeout (-m). Make it actionable instead of the raw
        // "Operation timed out" string.
        if out.status.code() == Some(28) {
            bail!(
                "Request timed out after {timeout}s ({url}) — the site may be slow or your VPN connection dropped; check VPN Vietnam and retry"
            );
        }
        bail!("curl failed for {url}: {err}");
    }
    Ok(Response {
        status,
        body: bytes,
        headers: resp_headers,
    })
}
