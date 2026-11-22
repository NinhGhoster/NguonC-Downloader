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
# GUI (window title: "NguonC Downloader")
cargo run -p nguonc-app --release

# CLI (needs VPN for resolve/download)
cargo run -p nguonc-cli -- scrape "https://phim.nguonc.com/phim/gign-luc-luong-tinh-nhue"
cargo run -p nguonc-cli -- download "https://phim.nguonc.com/phim/gign-luc-luong-tinh-nhue" out/ 0 0 1
# usage: download <url> <out-dir> [server-idx] [ep-idx] [parallel]
```

GUI defaults: window 960×640 (min 760×540), output dir `~/Downloads`
(editable, with a native Browse… folder picker), "Save into a movie-name
subfolder" checkbox (default on), concurrency slider 1–8 enabled only with
≥2 episodes checked (matches the Python app), terminal panel capped at
100 lines with in-place per-episode progress lines (0.3 s throttle) plus an
overall segment progress bar while downloading.

Feature parity with the Python GUI: theme toggle (System → Dark → Light),
status bar, English-title display (falls back to the Vietnamese title),
year/episode/server summary card, Select All / Deselect All, 4-column
episode grid (hover a cell for full status + filename), and the repo link
in the header. The Rust GUI also adds a **Check for Update** button (the
Python app never had one — only the footer link): it compares this build's
date against the latest GitHub release tag (`YYYY.MM.DD`) and shows a modal
with a link to the releases page.

Verify a change: `cargo fmt && cargo clippy --workspace --all-targets &&
cargo test --workspace && cargo build --release --workspace` (scrape via
the CLI works without VPN, resolve/download need it). The GUI binary users
run is `target/release/nguonc-app` — always rebuild `--release`.

Design notes (refactored 2026-11-22):
- Errors are typed (`core::NguoncError`): a Cloudflare 403 is reported as
  a block ("this IP is blocked, not the movie"), rate limits carry their
  retry delay, and a failed scrape keeps the real cause instead of the
  old generic "Could not find episode data on page".
- Downloads stream to disk in playlist order — peak memory is bounded by
  in-flight segments, not the whole movie — and an output under 1 KB is
  treated as a failed download (same rule as the Python engine).
- The "Concurrent episodes" slider bounds *episodes* in the GUI (segment
  parallelism inside one episode is fixed at 4); failed episodes can be
  retried without reloading the movie.
- Tests: `cargo test -p nguonc-core` (unit tests for filenames, episode
  parsing, header parsing; integration test for ordered streaming
  download against a local HTTP server).

Be Vietnam Pro is embedded (`app/assets/BeVietnamPro-Regular.ttf`) so
Vietnamese diacritics in movie titles render correctly.

Unlike the Python GUI (which resolves every episode's m3u8 when you pick a
server, burning bootstrap grants), the Rust GUI resolves each episode at
download time — switching servers costs nothing until you download.

## Verified live (2026-11-17)

- GIGN (1 server/6 eps): ep 1 downloaded, 331/331 segments, 741 MiB, h264 1920×1072 + AAC.
- Ngự Đình Dao (2 servers/32 eps): scrape OK; ep 1 from server 1 (Thuyết minh) with parallel=4, 273/273 segments, 731 MiB, h264 + AAC.
- Filenames match the Python engine (`English.Title.S01E01.mp4`); content is MPEG-TS named `.mp4` (same as the Python engine — no ffmpeg remux).

Scrape works without VPN; resolve/download need the VN VPN (embed host is
VN-only). Per-episode failures (`Rate limited`, `Turnstile verification
required`, Cloudflare 403, `wrong_origin`) are site responses, not code bugs —
they surface in the terminal panel. Connection resets/timeouts from the embed
host mean the VPN is off or slow: bootstrap requests allow 45 s and report
"Request timed out … check VPN Vietnam and retry"; segments retry 3× at 60 s.

## Differences vs. Python

- No yt-dlp / ffmpeg anywhere: HLS segments are fetched via `curl --http2`
  and concatenated directly (sidesteps issue #2's failure class).
- Temp `.m3u8` playlists are deleted after each download (Python leaves them
  in `$TMPDIR`).
- Single binaries: `nguonc` ~3.7 MB, `nguonc-app` ~8.9 MB (vs. 46–135 MB Python bundles).
- Embed host still requires HTTP/2 + `Origin`/`Referer`; CDN segment 403s
  are network/VPN issues, not code (curl works where Python clients get 403).
