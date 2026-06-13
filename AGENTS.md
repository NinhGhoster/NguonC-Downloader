# AGENTS.md — Nguồn Downloader

## Project Overview

Cross-platform desktop app that downloads videos from phim.nguonc.com.

## Key Files

| File | Purpose |
|------|---------|
| `nguonc_downloader.py` | Core engine (scrape, decode m3u8 URLs, download via yt-dlp) |
| `nguonc_app.py` | Flet desktop GUI |
| `requirements.txt` | Python deps (flet) |

## Commands

```bash
# Run app
python3 nguonc_app.py

# Build standalone
flet pack nguonc_app.py --name "Nguon Downloader"

# Test core module
python3 -c "from nguonc_downloader import NguoncDownloader; d = NguoncDownloader('https://phim.nguonc.com/phim/mot-ngay-no'); print(d.scrape()['english_title'])"
```

## Architecture

1. `scrape()` — fetch phim page, extract `episodes` JSON + metadata from HTML
2. `resolve_stream_url()` — fetch embed3/embed2.streamc.xyz, decode `data-obf` base64 → find `.m3u8` URL
3. `download_episode()` — shell out to `yt-dlp --concurrent-fragments N` with proper Referer
4. GUI calls core in background threads (no blocking)

## Stream URL Pattern

Embed page `data-obf` → base64 decode → `{"sUb":"<b64>","hD":"<hash>"}` → stream URL = `https://{domain}/{sUb}.m3u8`

## Naming Convention

`{English Title} ({Year}) EP {N}.mp4` — year editable in GUI

## Dependencies

- Runtime: `yt-dlp` (must be installed on system via brew/pip)
- Python: `flet` (for GUI)
- Packaging: `pyinstaller` (for standalone build)
