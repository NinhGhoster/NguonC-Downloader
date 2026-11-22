//! End-to-end test for the streaming download writer: segments are served
//! by a tiny local HTTP server (one curl fetch each, as in production) and
//! the output must be the segments concatenated in playlist order —
//! regardless of the order the fetches complete in.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Serve `segments[i]` (188 bytes of byte `i`) for GET /seg{i}, with a
/// staggered delay so later segments can finish before earlier ones.
fn serve(port: u16, delays_ms: Vec<u64>) {
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    for stream in listener.incoming().take(3) {
        let mut stream = stream.unwrap();
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("");
        let idx: usize = path.trim_start_matches("/seg").parse().unwrap_or(0);
        std::thread::sleep(std::time::Duration::from_millis(delays_ms[idx]));
        let body = vec![idx as u8; 1024];
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: video/mp2t\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.write_all(&body);
        let _ = stream.flush();
    }
}

#[tokio::test]
async fn download_writes_segments_in_order_and_streams() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    // seg2 is fastest, seg0 slowest: the writer must buffer out-of-order
    // segments and still produce seg0+seg1+seg2.
    let handle = std::thread::spawn(move || serve(port, vec![60, 30, 0]));

    let dir = std::env::temp_dir().join("nguonc-stream-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let playlist = dir.join("test.m3u8");
    let out = dir.join("out.mp4");
    let text = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXTINF:10,\nhttp://127.0.0.1:{port}/seg0\n#EXTINF:10,\nhttp://127.0.0.1:{port}/seg1\n#EXTINF:10,\nhttp://127.0.0.1:{port}/seg2\n"
    );
    std::fs::write(&playlist, text).unwrap();

    let progress = Arc::new(Mutex::new((0usize, 0usize)));
    let p2 = progress.clone();
    nguonc_core::download(
        playlist.to_str().unwrap(),
        out.to_str().unwrap(),
        "http://127.0.0.1/",
        3,
        move |p| {
            let mut g = p2.lock().unwrap();
            *g = (p.done, p.total);
        },
    )
    .await
    .unwrap();

    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(bytes.len(), 3 * 1024);
    assert!(bytes[..1024].iter().all(|&b| b == 0));
    assert!(bytes[1024..2048].iter().all(|&b| b == 1));
    assert!(bytes[2048..].iter().all(|&b| b == 2));
    assert_eq!(*progress.lock().unwrap(), (3, 3));
    // No leftover .part file after the atomic rename.
    assert!(!dir.join("out.part").exists());
    handle.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn download_reports_progress_as_segments_land() {
    // Reuse the same server shape with a single segment.
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let count = Arc::new(AtomicUsize::new(0));
    let c2 = count.clone();
    let handle = std::thread::spawn(move || {
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        for stream in listener.incoming().take(1) {
            let mut stream = stream.unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let body = vec![7u8; 2048];
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(&body);
            c2.fetch_add(1, Ordering::SeqCst);
        }
    });

    let dir = std::env::temp_dir().join("nguonc-stream-test-2");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let playlist = dir.join("test.m3u8");
    std::fs::write(
        &playlist,
        format!("#EXTM3U\nhttp://127.0.0.1:{port}/seg0\n"),
    )
    .unwrap();
    let out = dir.join("out.mp4");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    nguonc_core::download(
        playlist.to_str().unwrap(),
        out.to_str().unwrap(),
        "http://127.0.0.1/",
        1,
        move |p| s2.lock().unwrap().push((p.done, p.total)),
    )
    .await
    .unwrap();
    assert_eq!(*seen.lock().unwrap(), vec![(1, 1)]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    handle.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
