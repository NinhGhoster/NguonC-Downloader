# AGENTS.md

## NguonC Downloader — Build and Release

### Local Build (macOS)
```bash
bash build_macos.sh              # Output: dist/NguonC Downloader.app
```

### CI Builds
Pushing a tag triggers `.github/workflows/build.yml` which builds:
- **macOS** (macos-latest): `.app` bundle via `build_macos.sh`
- **Windows** (windows-latest): `.exe` via `flet pack`
- **Linux** (ubuntu-latest): Linux binary via `flet pack`

### Tag format
```
git tag 2026.06.14
git push origin --tags
```

This auto-creates a GitHub Release with all platform artifacts.

### Key Files
| File | Purpose |
|------|---------|
| `nguonc_downloader.py` | Core engine: scrape phim.nguonc.com, resolve stream URLs, download |
| `nguonc_app.py` | Flet GUI with theme toggle, episode grid, log view |
| `build_macos.sh` | macOS build script — patches Flet.app CFBundleName → NguonC Downloader |
| `.github/workflows/build.yml` | CI pipeline for all 3 platforms |

### Platform Quirks
- `pbcopy` (macOS), `clip` (Windows), `xclip`/`xsel` (Linux) for clipboard
- `osascript` (macOS), `powershell` (Windows), `zenity`/`kdialog` (Linux) for folder picker
- `plutil` only on macOS — runtime plist patching silently skipped on other platforms
