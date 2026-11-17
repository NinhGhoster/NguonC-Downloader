# rust — NguonC Downloader (Rust port)

Rust port of `nguonc_downloader.py`, shipped as a separate brand called
**rust** from the same repo. The Python `NguonC Downloader` stays as-is on
`main`; this branch holds the Rust line until the brand ships.

## Layout

| Crate | Role |
|-------|------|
| `core` | scrape → bootstrap (`aes-gcm-v1` envelope) → decrypt m3u8 → HLS download (native GETs, no ffmpeg/yt-dlp) |
| `cli` | thin binary wrapping `core` for scripted verification |
| `app` | native GUI (egui/eframe 0.32) wrapping `core` |

## Build

```bash
cargo build --release        # target/release/nguonc (CLI) and target/release/nguonc-app (GUI)
```

## Run

```bash
# GUI (brand window title: rust)
cargo run -p nguonc-app --release

# CLI (needs VPN for resolve/download)
cargo run -p nguonc-cli -- scrape "https://phim.nguonc.com/phim/gign-luc-luong-tinh-nhue"
cargo run -p nguonc-cli -- download "https://phim.nguonc.com/phim/gign-luc-luong-tinh-nhue" out/ 0 0 1
# usage: download <url> <out-dir> [server-idx] [ep-idx] [parallel]
```

GUI defaults: output dir `~/Downloads` (editable in-app), concurrency slider
1–8 enabled only with ≥2 episodes checked (matches the Python app), terminal
panel capped at 100 lines with 0.3 s per-episode progress throttle.

## Verified live (2026-11-17)

- GIGN (1 server/6 eps): ep 1 downloaded, 331/331 segments, 741 MiB, h264 1920×1072 + AAC.
- Ngự Đình Dao (2 servers/32 eps): scrape OK; ep 1 from server 1 (Thuyết minh) with parallel=4, 273/273 segments, 731 MiB, h264 + AAC.
- Filenames match the Python engine (`English.Title.S01E01.mp4`); content is MPEG-TS named `.mp4` (same as the Python engine — no ffmpeg remux).

Scrape works without VPN; resolve/download need the VN VPN (embed host is
VN-only). Per-episode failures (`Rate limited`, `Turnstile verification
required`, Cloudflare 403, `wrong_origin`) are site responses, not code bugs —
they surface in the terminal panel.

## Differences vs. Python

- No yt-dlp / ffmpeg anywhere: HLS segments are fetched via `curl --http2`
  and concatenated directly (sidesteps issue #2's failure class).
- Temp `.m3u8` playlists are deleted after each download (Python leaves them
  in `$TMPDIR`).
- Single binaries: `nguonc` ~3.7 MB, `nguonc-app` ~8.9 MB (vs. 46–135 MB Python bundles).
- Embed host still requires HTTP/2 + `Origin`/`Referer`; CDN segment 403s
  are network/VPN issues, not code (curl works where Python clients get 403).
