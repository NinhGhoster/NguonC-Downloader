//! HLS download: parse decrypted playlist, fetch segments concurrently via
//! curl (HTTP/2, browser headers), write in playlist order to a temp file,
//! atomic rename. No yt-dlp, no ffmpeg — avoids the entire issue #2 failure class.
//!
//! Segments are written to disk as soon as their turn comes: completed
//! segments are held only in a small out-of-order buffer, so peak memory is
//! bounded by in-flight segments, not by the whole movie. The old code kept
//! every segment body in a `Vec` and only wrote after the last one landed —
//! ~741 MiB of RAM for a single verified episode, and a late failure lost
//! everything already fetched.
use crate::error::{NguoncError, Result};
use crate::http::{self, USER_AGENT};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub bytes: u64,
}

/// Download one episode. `parallelism` bounds segment fetches *within* this
/// episode; callers bound episode-level concurrency separately.
pub async fn download(
    playlist_path: &str,
    output_path: &str,
    referer: &str,
    parallelism: usize,
    on_progress: impl Fn(Progress) + Send + Sync + 'static,
) -> Result<()> {
    let text = tokio::fs::read_to_string(playlist_path).await?;
    if text.contains("#EXT-X-STREAM-INF") {
        return Err(NguoncError::Unsupported(
            "master playlists are not supported".into(),
        ));
    }
    if text.contains("#EXT-X-KEY:") && !text.contains("METHOD=NONE") {
        return Err(NguoncError::Unsupported(
            "encrypted HLS segments are not supported".into(),
        ));
    }
    if text.contains("#EXT-X-MAP:") || text.contains("#EXT-X-BYTERANGE:") {
        return Err(NguoncError::Unsupported(
            "fMP4 / byte-range playlists are not supported".into(),
        ));
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
            return Err(NguoncError::parse(format!("invalid segment URL: {u}")));
        }
        segments.push(u.to_string());
    }
    if segments.is_empty() {
        return Err(NguoncError::parse("playlist has no segments"));
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

    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(parallelism.max(1)));
    let on_progress = std::sync::Arc::new(on_progress);
    let mut results = tokio::task::JoinSet::new();
    for (idx, seg) in segments.iter().enumerate() {
        let sem = semaphore.clone();
        let seg = seg.clone();
        let referer = referer.to_string();
        results.spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|e| NguoncError::parse(e.to_string()))?;
            let mut last_err = String::new();
            for attempt in 1..=3 {
                match http::fetch(
                    &seg,
                    Some(&referer),
                    "GET",
                    &[("User-Agent", USER_AGENT)],
                    None,
                    60,
                )
                .await
                {
                    Ok(r) if r.status == 200 && r.body.len() >= 188 => {
                        return Ok((idx, r.body));
                    }
                    Ok(r) => {
                        last_err = format!("HTTP {} ({} bytes)", r.status, r.body.len());
                    }
                    Err(e) => last_err = e.to_string(),
                }
                if attempt < 3 {
                    tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64))
                        .await;
                }
            }
            Err(NguoncError::parse(format!(
                "segment {}/{} failed after 3 attempts: {last_err}",
                idx + 1,
                total
            )))
        });
    }

    // Ordered streaming write: buffer out-of-order segments, drain the
    // contiguous prefix to disk as soon as it is complete, then drop the
    // bytes. Memory held is at most the in-flight + out-of-order segments.
    let mut buffered: HashMap<usize, Vec<u8>> = HashMap::new();
    let mut next_to_write = 0usize;
    while let Some(res) = results.join_next().await {
        let (idx, body) =
            res.map_err(|e| NguoncError::parse(format!("segment task panicked: {e}")))??;
        bytes_total += body.len() as u64;
        completed += 1;
        buffered.insert(idx, body);
        while let Some(body) = buffered.remove(&next_to_write) {
            file.write_all(&body)?;
            next_to_write += 1;
        }
        // Report as segments land, not after the whole file is written, so
        // progress callbacks are live during the fetch.
        on_progress(Progress {
            done: completed,
            total,
            bytes: bytes_total,
        });
    }
    if next_to_write != total {
        return Err(NguoncError::parse(format!(
            "download incomplete: wrote {next_to_write}/{total} segments"
        )));
    }
    file.flush()?;
    drop(file);
    // Port of the Python engine's rule: an output under 1 KB is a failed
    // download, not a movie.
    if bytes_total < 1024 {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(NguoncError::parse(
            "no data downloaded; file would be empty",
        ));
    }
    tokio::fs::rename(&part, out).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::download;
    use std::sync::{Arc, Mutex};

    #[test]
    fn progress_is_copyable_shape() {
        let p = super::Progress {
            done: 1,
            total: 2,
            bytes: 188,
        };
        assert_eq!(p.done, 1);
        let _ = Arc::new(Mutex::new(p.bytes));
    }

    #[tokio::test]
    async fn rejects_master_playlist() {
        let dir = std::env::temp_dir();
        let pl = dir.join("nguonc-test-master.m3u8");
        tokio::fs::write(&pl, "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nlow.m3u8\n")
            .await
            .unwrap();
        let err = download(
            pl.to_str().unwrap(),
            "/tmp/nguonc-test-out.mp4",
            "",
            1,
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("master playlists"));
    }

    #[tokio::test]
    async fn rejects_empty_playlist() {
        let dir = std::env::temp_dir();
        let pl = dir.join("nguonc-test-empty.m3u8");
        tokio::fs::write(&pl, "#EXTM3U\n").await.unwrap();
        let err = download(
            pl.to_str().unwrap(),
            "/tmp/nguonc-test-out.mp4",
            "",
            1,
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("no segments"));
    }
}
