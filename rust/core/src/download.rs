//! HLS download: parse decrypted playlist, fetch segments concurrently via
//! curl (HTTP/2, browser headers), append in order to a temp file, atomic
//! rename. No yt-dlp, no ffmpeg — avoids the entire issue #2 failure class.
use crate::http::{self, USER_AGENT};
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::Path;

pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub bytes: u64,
}

pub async fn download(
    playlist_path: &str,
    output_path: &str,
    referer: &str,
    parallelism: usize,
    on_progress: impl Fn(Progress) + Send + Sync + 'static,
) -> Result<()> {
    let text = tokio::fs::read_to_string(playlist_path).await?;
    if text.contains("#EXT-X-STREAM-INF") {
        bail!("master playlists are not supported");
    }
    if text.contains("#EXT-X-KEY:") && !text.contains("METHOD=NONE") {
        bail!("encrypted HLS segments are not supported");
    }
    if text.contains("#EXT-X-MAP:") || text.contains("#EXT-X-BYTERANGE:") {
        bail!("fMP4 / byte-range playlists are not supported");
    }

    let base = url::Url::parse(&format!("file://{playlist_path}"))?;
    let mut segments = Vec::new();
    for line in text.lines() {
        let item = line.trim();
        if item.is_empty() || item.starts_with('#') {
            continue;
        }
        let u = base.join(item)?;
        if u.scheme() != "http" && u.scheme() != "https" {
            bail!("invalid segment URL: {u}");
        }
        segments.push(u.to_string());
    }
    if segments.is_empty() {
        bail!("playlist has no segments");
    }

    let out = Path::new(output_path);
    if let Some(parent) = out.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let part = out.with_extension("part");
    let mut file = std::fs::File::create(&part)?;
    let total = segments.len();
    let mut bytes_total = 0u64;
    let mut completed = 0usize;

    // Bounded-concurrency fetch with ordered writes. We keep it simple and
    // robust: one curl per segment, 3 attempts, write in playlist order.
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(parallelism.max(1)));
    let on_progress = std::sync::Arc::new(on_progress);
    let mut results = tokio::task::JoinSet::new();
    for (idx, seg) in segments.iter().enumerate() {
        let sem = semaphore.clone();
        let seg = seg.clone();
        let referer = referer.to_string();
        results.spawn(async move {
            let _permit = sem.acquire().await?;
            let mut last_err = String::new();
            for attempt in 1..=3 {
                match http::fetch(&seg, Some(&referer), "GET", &[("User-Agent", USER_AGENT)], None, 60).await {
                    Ok(r) if r.status == 200 && r.body.len() >= 188 => return Ok((idx, r.body)),
                    Ok(r) => last_err = format!("HTTP {} ({} bytes)", r.status, r.body.len()),
                    Err(e) => last_err = e.to_string(),
                }
                if attempt < 3 {
                    tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64)).await;
                }
            }
            bail!("segment {}/{} failed after 3 attempts: {last_err}", idx + 1, total)
        });
    }

    let mut ordered = vec![None; total];
    while let Some(res) = results.join_next().await {
        let (idx, body) = res.context("segment task panicked")??;
        bytes_total += body.len() as u64;
        completed += 1;
        ordered[idx] = Some(body);
        // Report as segments land, not after the ordered write pass, so
        // progress callbacks are live during the fetch.
        on_progress(Progress { done: completed, total, bytes: bytes_total });
    }
    for body in ordered.into_iter() {
        let body = body.context("missing segment")?;
        file.write_all(&body)?;
    }
    file.flush()?;
    drop(file);
    if bytes_total == 0 {
        bail!("no data downloaded; file would be empty");
    }
    tokio::fs::rename(&part, out).await?;
    Ok(())
}
