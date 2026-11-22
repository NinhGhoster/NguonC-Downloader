//! HTTP transport. The embed host hard-blocks HTTP/1.1, so embed traffic
//! goes through system `curl --http2` (same approach as the Python engine —
//! measured: Python clients get 403 from non-VN IPs while curl with the same
//! headers gets 200).
//!
//! Design notes:
//! - One transport for every host, but HTTP/2 is only *required* for the
//!   embed host. Public pages (movie page, episodes JSON, GitHub API) fall
//!   back to HTTP/1.1 when curl has no h2 support, instead of failing
//!   outright — the old code bailed for every host when h2 was missing.
//! - The response body comes back on curl's stdout, not via a temp file.
//!   Only the response headers still use a temp file (curl needs a place
//!   to write them, and the status line is parsed from it). One temp file
//!   per request instead of two, and no body file to leak on a crash.
//! - `get` checks the status code. The old `get` returned the body of a
//!   Cloudflare 403 page as if it were the movie page, and `scrape` then
//!   reported the misleading "Could not find episode data on page".
use crate::error::{NguoncError, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
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

    /// The fetch succeeded at the HTTP level but the status was not 200.
    pub fn check_status(&self, url: &str) -> Result<()> {
        if self.status == 200 {
            Ok(())
        } else {
            Err(NguoncError::from_response(url, self.status, &self.body))
        }
    }
}

fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(bin))
            .find(|p| p.is_file())
    })
}

/// System curl, resolved once per process.
fn curl_path() -> Option<&'static PathBuf> {
    static CURL: OnceLock<Option<PathBuf>> = OnceLock::new();
    CURL.get_or_init(|| which("curl")).as_ref()
}

/// Probe once per process whether system curl supports HTTP/2.
fn curl_supports_http2() -> bool {
    static H2: OnceLock<bool> = OnceLock::new();
    *H2.get_or_init(|| {
        let Some(curl) = curl_path() else {
            return false;
        };
        if Command::new(curl)
            .args(["-sS", "--http2", "--version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return true;
        }
        Command::new(curl)
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
    curl_path().is_some() && curl_supports_http2()
}

/// Parse a curl `-D` header dump into (status, headers). There can be more
/// than one status line when the connection goes through a proxy
/// (`HTTP/1.1 200 Connection Established` for the CONNECT, then the real
/// `HTTP/2 403`) — the last one is the response, and headers from the
/// proxy handshake are discarded with it.
fn parse_headers(head: &str) -> (u16, HashMap<String, String>) {
    let mut status = 0u16;
    let mut headers = HashMap::new();
    for line in head.lines() {
        if line.starts_with("HTTP/") {
            status = line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            headers.clear();
        } else if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    (status, headers)
}

/// Simple GET used for public pages (movie page, JSON episodes endpoint).
/// Fails on any non-200 status instead of returning an error page as text.
pub async fn get(url: &str, referer: Option<&str>) -> Result<String> {
    let resp = fetch(url, referer, "GET", &[], None, 30).await?;
    resp.check_status(url)?;
    Ok(resp.text())
}

/// Full fetch via system curl. The body is captured on stdout; headers go
/// to one temp file whose first line carries the status. HTTP/2 is used
/// when curl supports it, otherwise HTTP/1.1 — callers that *require* h2
/// (the embed host) must check `can_h2()` first, as `resolve` does.
pub async fn fetch(
    url: &str,
    referer: Option<&str>,
    method: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
    timeout: u64,
) -> Result<Response> {
    let Some(curl) = curl_path() else {
        return Err(NguoncError::Curl("curl is not available on PATH".into()));
    };
    let tag = format!(
        "nguonc-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let head_path = std::env::temp_dir().join(format!("{tag}.head"));

    let mut cmd = Command::new(curl);
    cmd.args(["-sS", "--compressed"])
        .arg("-m")
        .arg(timeout.to_string())
        .arg("-D")
        .arg(&head_path)
        .args(["-H", &format!("User-Agent: {USER_AGENT}")]);
    if curl_supports_http2() {
        cmd.arg("--http2");
    }
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
        .map_err(|e| NguoncError::Curl(format!("failed to run curl for {url}: {e}")))?;
    // The header file is read (and deleted) on every path out, so a failed
    // curl cannot leave temp files behind.
    let head = fs::read_to_string(&head_path).await.unwrap_or_default();
    let _ = fs::remove_file(&head_path).await;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        // curl exit 28 = timeout (-m).
        if out.status.code() == Some(28) {
            return Err(NguoncError::Timeout {
                url: url.to_string(),
                timeout_secs: timeout,
            });
        }
        return Err(NguoncError::Curl(format!("curl failed for {url}: {err}")));
    }
    let (status, resp_headers) = parse_headers(&head);
    Ok(Response {
        status,
        body: out.stdout,
        headers: resp_headers,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_headers;

    #[test]
    fn parse_headers_uses_last_status_line_behind_a_proxy() {
        let head = "HTTP/1.1 200 Connection Established\r\n\r\nHTTP/2 403 \r\ndate: Sat, 21 Nov 2026 15:41:48 GMT\r\nserver: cloudflare\r\n";
        let (status, headers) = parse_headers(head);
        assert_eq!(status, 403);
        assert_eq!(
            headers.get("server").map(String::as_str),
            Some("cloudflare")
        );
    }

    #[test]
    fn parse_headers_plain_response() {
        let (status, headers) = parse_headers("HTTP/2 200\r\ncontent-type: text/html\r\n");
        assert_eq!(status, 200);
        assert_eq!(
            headers.get("content-type").map(String::as_str),
            Some("text/html")
        );
    }
}
