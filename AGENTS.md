# AGENTS.md — NguonC Downloader

## Entrypoints

- `nguonc_app.py` — Flet GUI (run directly: `python3 nguonc_app.py`)
- `nguonc_downloader.py` — core engine (scrape → resolve `data-obf` m3u8 → `yt_dlp` download)

## Running

```bash
pip install -r requirements.txt
python3 nguonc_app.py
```

No external yt-dlp needed — uses bundled `yt_dlp` Python library.

## Building

| Platform | Command | Output |
|----------|---------|--------|
| macOS | `bash build_macos.sh` | `dist/NguonC Downloader.app` (then CI wraps in .dmg) |
| Windows | `flet pack nguonc_app.py --name "NguonC Downloader" --icon assets/icon.ico` | `.exe` |
| Linux | `xvfb-run flet pack nguonc_app.py --name "NguonC Downloader" --icon assets/icon.png` | `.bin` |

macOS: `build_macos.sh` patches Flet.app `CFBundleName` in cache before build, restores after. Runtime also patches via `sys._MEIPASS`. Run it on macOS only.

## Flet 0.85.3 quirks

- `page.clipboard` is **read-only** — use `pbcopy`/`clip`/`xclip` subprocess instead
- `page.scroll = ft.ScrollMode.AUTO` (not `ADAPTIVE`)
- No `ft.padding.symmetric()` — use `ft.Padding(...)` tuple
- `ft.SnackBar` shown via `page.show_dialog(SnackBar(...))`, not `page.snack_bar =`
- `ft.Border.all(...)` not `ft.border.all(...)`
- `ft.run(func)` — pass function reference, not instance

## Architecture

`_fetch()` in `nguonc_downloader.py` disables SSL verification (phim.nguonc.com has misconfigured certs).

m3u8 resolution: embed page → `data-obf` → base64 decode → `{"sUb":"<b64>","hD":"<hash>"}` → `domain/{sub}.m3u8`. Referer header must be the embed URL (CDN requires it).

Episode checkboxes in `GridView` are wrapped as `ft.Row([ft.Checkbox(data=ep), ft.Text(..., selectable=True)])`. Access checkbox via `controls[0]`.

## CI & Releases

Triggered on tag push (`*`), PR, or manual. Workflow in `.github/workflows/build.yml`. Tag format: `YYYY.MM.DD` (force push to update).

Release assets per platform:

| Platform | File |
|----------|------|
| macOS | `NguonC Downloader.dmg` |
| Windows | `NguonC Downloader.exe` |
| Linux | `NguonC Downloader.bin` |

## Icon assets

`assets/icon.png` — source. Convert to `icon.icns` (macOS) and `icon.ico` (Windows) using `sips` + `iconutil` or Python struct packing. Each platform `flet pack --icon` uses its own format.
