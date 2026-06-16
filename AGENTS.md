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

### m3u8 resolution

embed page → `data-obf` → base64 decode → `{"sUb":"<b64>","hD":"<hash>"}` → `streamc.xyz/{sub}.m3u8` (encrypted).

### Encrypted m3u8 decryption

The `streamc.xyz/*.m3u8` response contains:

```
#ENC-AESGCM;iv=<12-byte-hex>
<base64-encoded-ciphertext>
```

**Key derivation**: `HMAC-SHA256(key="stream-derive-v1" as UTF-8 bytes, msg=videoHash as UTF-8 bytes)` → 32-byte AES-256 key.

The `videoHash` comes from `data-obf` → `{"sUb":"...","hD":"<hash>"}` field `hD`.

**Decryption**: AES-256-GCM with 12-byte IV (from `#ENC-AESGCM` tag), no additional authenticated data (AAD). The plaintext is a standard m3u8 playlist.

**Important**: The `#EXT-X-B65:0-138` tag sometimes present is just metadata indicating the first 138 bytes of the base64 payload — it is NOT used as AAD.

`sing.phimmoi.net` direct m3u8 URLs are dead (NXDOMAIN). All episodes go through the encrypted path.

### Encryption key constant

The 18-byte string `"stream-derive-v1"` (UTF-8) is hardcoded in player.js. There is no per-episode or per-server variation.

### CDN segments

After decryption, the m3u8 references TS segments hosted on `jps*.hihihoho4.top`. These segments may have a `.png` extension but are valid MPEG-TS.

Referer header must be the embed URL (CDN requires it).

### Download

Decrypted m3u8 is written to a temp file. yt-dlp is invoked with `--enable-file-urls` to handle local file:// paths. `cryptography` library is required for AES-GCM (installed as part of requirements.txt).

## Flet quirks

- `page.clipboard` is **read-only** — use `pbcopy`/`clip`/`xclip` subprocess instead
- `page.scroll = ft.ScrollMode.AUTO` (not `ADAPTIVE`)
- No `ft.padding.symmetric()` — use `ft.Padding(...)` tuple
- `ft.SnackBar` shown via `page.show_dialog(SnackBar(...))`, not `page.snack_bar =`
- `ft.Border.all(...)` not `ft.border.all(...)`
- `ft.run(func)` — pass function reference, not instance

## UI

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
