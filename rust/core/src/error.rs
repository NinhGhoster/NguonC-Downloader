//! Typed errors for the engine.
//!
//! Previously every failure was an `anyhow` string, so callers (and users)
//! could not tell a Cloudflare block from a rate limit from a changed page
//! layout — and `scrape` collapsed all of them into "Could not find episode
//! data on page". Each variant carries the facts needed to act on it, and
//! its `Display` is the message shown to the user.
use std::fmt;

#[derive(Debug)]
pub enum NguoncError {
    /// Non-2xx HTTP response. `detail` is a short snippet of the body.
    Http {
        url: String,
        status: u16,
        detail: String,
    },
    /// Cloudflare block page (HTTP 403 with a Cloudflare challenge body).
    /// This is an IP/reputation block, not a bug in the resolver.
    CloudflareBlocked {
        url: String,
    },
    /// `curl` itself failed (DNS, connection reset, …).
    Curl(String),
    /// `curl -m` timed out.
    Timeout {
        url: String,
        timeout_secs: u64,
    },
    /// Embed host rate limit (HTTP 429/503 with `Retry-After`).
    RateLimited {
        retry_after_secs: u64,
    },
    /// Embed host wants a Turnstile check and issued no preissued grant.
    Turnstile,
    /// Bootstrap API host does not match the embed host.
    WrongOrigin,
    /// A required field / pattern was missing, or a payload did not parse.
    Parse(String),
    /// The playlist uses a feature this engine does not support.
    Unsupported(String),
    Io(std::io::Error),
    Other(String),
}

impl NguoncError {
    /// Build the right error for a non-2xx response, detecting the
    /// Cloudflare block page so it is never reported as a parse failure.
    pub fn from_response(url: &str, status: u16, body: &[u8]) -> Self {
        let text = String::from_utf8_lossy(body);
        if status == 403
            && (text.contains("Attention Required")
                || text.contains("Cloudflare")
                || text.contains("cf-error"))
        {
            return Self::CloudflareBlocked {
                url: url.to_string(),
            };
        }
        let detail: String = text.chars().filter(|c| !c.is_control()).take(200).collect();
        Self::Http {
            url: url.to_string(),
            status,
            detail,
        }
    }

    pub fn parse(msg: impl Into<String>) -> Self {
        Self::Parse(msg.into())
    }

    /// Replace the body snippet of an `Http` error with a parsed error
    /// detail (e.g. the embed host's JSON `"error"` field), when present.
    pub fn map_detail(mut self, detail: String) -> Self {
        if let Self::Http { detail: d, .. } = &mut self
            && !detail.is_empty()
        {
            *d = detail;
        }
        self
    }
}

impl fmt::Display for NguoncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http {
                url,
                status,
                detail,
            } => {
                if detail.is_empty() {
                    write!(f, "HTTP {status} from {url}")
                } else {
                    write!(f, "HTTP {status} from {url}: {detail}")
                }
            }
            Self::CloudflareBlocked { url } => write!(
                f,
                "Blocked by Cloudflare on {url} (HTTP 403) — this IP is blocked, not the movie; try a different network/VPN"
            ),
            Self::Curl(e) => write!(f, "curl failed: {e}"),
            Self::Timeout { url, timeout_secs } => write!(
                f,
                "Request timed out after {timeout_secs}s ({url}) — the site may be slow or your VPN connection dropped; check VPN Vietnam and retry"
            ),
            Self::RateLimited { retry_after_secs } => write!(
                f,
                "Rate limited by embed host (retry after {retry_after_secs}s)"
            ),
            Self::Turnstile => write!(f, "Turnstile verification required (no preissued grant)"),
            Self::WrongOrigin => write!(f, "Invalid bootstrap api URL"),
            Self::Parse(msg) => write!(f, "{msg}"),
            Self::Unsupported(msg) => write!(f, "{msg}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for NguoncError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for NguoncError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for NguoncError {
    fn from(e: serde_json::Error) -> Self {
        Self::Parse(e.to_string())
    }
}

impl From<url::ParseError> for NguoncError {
    fn from(e: url::ParseError) -> Self {
        Self::Parse(e.to_string())
    }
}

impl From<regex::Error> for NguoncError {
    fn from(e: regex::Error) -> Self {
        Self::Parse(format!("invalid regex: {e}"))
    }
}

impl From<base64::DecodeError> for NguoncError {
    fn from(e: base64::DecodeError) -> Self {
        Self::Parse(format!("invalid base64: {e}"))
    }
}

impl From<std::string::FromUtf8Error> for NguoncError {
    fn from(e: std::string::FromUtf8Error) -> Self {
        Self::Parse(e.to_string())
    }
}

impl From<std::time::SystemTimeError> for NguoncError {
    fn from(e: std::time::SystemTimeError) -> Self {
        Self::Other(e.to_string())
    }
}

impl From<hmac::digest::InvalidLength> for NguoncError {
    fn from(e: hmac::digest::InvalidLength) -> Self {
        Self::Parse(e.to_string())
    }
}

impl From<aes_gcm::Error> for NguoncError {
    fn from(_: aes_gcm::Error) -> Self {
        Self::Parse("decrypt failed".to_string())
    }
}

/// Convenience alias used by every public core function.
pub type Result<T> = std::result::Result<T, NguoncError>;
