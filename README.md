# Nguồn Downloader

A cross-platform desktop app that downloads videos from **phim.nguonc.com** with maximum speed via parallel HLS fragment downloading.

## Features

- **One-click download**: Paste any movie URL from phim.nguonc.com
- **Multiple sources**: Choose between Vietsub, Thuyết minh, and other available servers
- **Batch episodes**: Select individual episodes or ranges
- **Maximum speed**: Downloads HLS segments in parallel (configurable up to 32 concurrent fragments)
- **Smart naming**: Automatically names files as `{Title} ({Year}) EP {N}.mp4`
- **Progress tracking**: Real-time per-episode progress bars
- **Cross-platform**: Works on macOS, Windows, and Linux

## Prerequisites

- **yt-dlp** (required for downloading):
  - macOS: `brew install yt-dlp`
  - Windows: `winget install yt-dlp` or `pip install yt-dlp`
  - Linux: `sudo apt install yt-dlp` or `pip install yt-dlp`

## Installation

```bash
# Clone or download this repo
cd nguonc-downloader

# Install Python dependencies
pip install -r requirements.txt

# Run the app
python3 nguonc_app.py
```

## Usage

1. Launch the app
2. Paste a phim.nguonc.com movie URL (e.g., `https://phim.nguonc.com/phim/mot-ngay-no`)
3. Click **Load Movie** — the app fetches title, year, servers, and episodes
4. Select the **server/source** (Vietsub, Thuyết minh, etc.)
5. Tick the **episodes** you want to download
6. Adjust **Concurrent Fragments** slider (higher = faster, default 8)
7. Choose an **output directory**
8. Click **Download Selected**

## How It Works

1. **Scrape**: Fetches the movie page and extracts episode data from embedded JSON
2. **Resolve**: For each episode, fetches the stream embed page and decodes the obfuscated HLS URL
3. **Download**: Uses `yt-dlp --concurrent-fragments N` to download HLS segments in parallel
4. **Save**: Outputs original-quality MP4 files with clean naming

## Building a Standalone App

### macOS (.app bundle)
```bash
pip install pyinstaller
flet pack nguonc_app.py --name "Nguon Downloader"
```
Output: `dist/Nguon Downloader.app`

### Windows (.exe)
```powershell
pip install pyinstaller
flet pack nguonc_app.py --name "Nguon Downloader"
```

### Manual PyInstaller
```bash
pip install pyinstaller
pyinstaller --onefile --windowed --name "NguonDownloader" --add-data "nguonc_downloader.py:." nguonc_app.py
```

## Project Structure

```
nguonc-downloader/
├── nguonc_downloader.py    # Core engine: scrape, decode, download
├── nguonc_app.py           # Flet desktop GUI
├── requirements.txt        # Python dependencies
├── AGENTS.md               # For AI coding assistants
└── README.md               # This file
```

## Tech Stack

- **Python 3** — Core logic
- **Flet** — Cross-platform GUI (native Flutter widgets)
- **yt-dlp** — HLS downloading with parallel fragments
- **PyInstaller** — App packaging

## License

MIT
