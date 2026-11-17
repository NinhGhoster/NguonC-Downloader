# NguonC Downloader — Rust (Phase 1)

Port of `nguonc_downloader.py` (engine) + a native GUI (Phase 2) to Rust.
Keep the Python `NguonC Downloader` as-is; this is a separate brand/product
shipped from the same repo.

## Layout

| Crate | Role |
|-------|------|
| `core` | scrape → bootstrap (`aes-gcm-v1` envelope) → decrypt m3u8 → HLS download (native GETs, no ffmpeg/yt-dlp) |
| `cli` | thin binary wrapping `core` for scripted verification |

## Build

```bash
cargo build --release        # binary at target/release/nguonc
```

## Test (needs VPN for resolve/download)

```bash
# Scrape (no VPN needed)
cargo run -p nguonc-cli -- scrape "https://phim.nguonc.com/phim/gign-luc-luong-tinh-nhue"

# Resolve + download (VPN required)
cargo run -p nguonc-cli -- download "https://phim.nguonc.com/phim/gign-luc-luong-tinh-nhue" out/ 0 0 1
# usage: download <url> <out-dir> [server-idx] [ep-idx] [parallel]
```

## Phase 2 (GUI)

Native egui/eframe GUI was scaffolded during Phase 1 but removed because
async borrows never cross task boundaries (classic eframe::App API). Re-add
as `app` crate with an mpsc-based update loop.

## Differences vs. Python

- No yt-dlp / ffmpeg: HLS segments are fetched via `curl --http2` and
  concatenated directly.
- Single static-ish binary (~4 MB release vs. 46–135 MB bundles).
- Embed host still requires HTTP/2; CDN segments are TLS-fingerprint-gated
  (curl works, Python clients get 403 from non-VN IPs — same as before).