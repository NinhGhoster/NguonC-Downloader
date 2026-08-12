# AGENTS.md — NguonC Downloader

## Entrypoints

- `nguonc_app.py` — Flet GUI (run directly: `uv run nguonc_app.py`)
- `nguonc_downloader.py` — core engine (scrape → resolve `data-obf` m3u8 → `yt_dlp` download)

## Running

```bash
uv sync
uv run nguonc_app.py
```

Uses [uv](https://docs.astral.sh/uv/) — creates `.venv`, installs deps from `pyproject.toml` (`uv.lock` pinned). Runtime deps are in `[project]`; build tools (`flet-cli`, `pyinstaller`) in the `dev` dependency group, installed by `uv sync` by default.

No external yt-dlp needed — uses bundled `yt_dlp` Python library.

## Building

| Platform | Command | Output |
|----------|---------|--------|
| macOS | `bash build_macos.sh` | `dist/NguonC Downloader.app` (then CI wraps in .dmg) |
| Windows | `uv run flet pack nguonc_app.py --name "NguonC Downloader" --icon assets/icon.ico` | `.exe` |
| Linux | `xvfb-run uv run flet pack nguonc_app.py --name "NguonC Downloader" --icon assets/icon.png` | `.bin` |

macOS: `build_macos.sh` patches Flet.app `CFBundleName` in cache before build, restores after. Runtime also patches via `sys._MEIPASS`. Run it on macOS only.

## Flet quirks

All verified against the app code (flet `>=0.25.0`, locked 0.86.x; `build_macos.sh` caches flet-desktop client `0.85.3`):

- `page.clipboard` is **read-only** — use `pbcopy`/`clip`/`xclip` subprocess instead
- `page.scroll = ft.ScrollMode.AUTO` (not `ADAPTIVE`)
- No `ft.padding.symmetric()` — use `ft.Padding(...)` tuple
- `ft.SnackBar` shown via `page.show_dialog(SnackBar(...))`, not `page.snack_bar =`
- `ft.Border.all(...)` not `ft.border.all(...)`
- `ft.run(func)` is only the app entry point — pass function reference, not instance
- flet 0.86.5 has **no** main-thread marshaling API (`page.run_thread`/`run_on_main` do not exist). `control.update()` from a background thread races with flet's own patch/diff on lifecycle events (`on_app_lifecycle_state_change` → `__auto_update` → `ObjectPatch._compare_lists` → `IndexError`). Fix: `build()` captures the app's asyncio loop (`asyncio.get_running_loop()`) and all worker-thread UI work goes through `loop.call_soon_threadsafe` (the app's `ui(fn)` helper). Never touch controls from worker threads directly.

## Architecture

`_fetch()` in `nguonc_downloader.py` uses a **verified** TLS context by default (`_SECURE_CTX`); `INSECURE_HOSTS` is an allowlist of hosts with broken certs that get an unverified fallback only on `SSLCertVerificationError` (currently empty — all live hosts verify OK). Never add a host there without probing it live first.

### Episode data extraction

Movie pages embed episode JSON in `<div id="nc-episode-data">[{...}]</div>` (site redesign ~2026-07). `scrape()` matches `id="nc-episode-data">(\[.*?\])</` with a `var episodes = [...]` fallback for old pages. Server objects use key `name` (was `server_name`) and each episode has `name`/`slug`/`embed`.

Year comes from `https://phim.nguonc.com/api/film/{slug}` (category group `Năm`), slug via `_extract_slug()` on the movie URL, with HTML regex fallbacks.

### m3u8 resolution

embed page (`embed2/embed3.streamc.xyz/embed.php?hash=...`) → `data-obf` → base64 decode → `{"sUb":"<b64>","hD":"<hash>"}` → m3u8 at `{embed_domain}/{sUb}.m3u8` (the `.m3u8` suffix is optional). `sUb` base64-decodes to `{"h":"<hash>","t":"<token>"}` — the `t` token is for the player's `?d=1` variant; the downloader ignores it.

`resolve_all_m3u8()` resolves episodes in parallel (`ThreadPoolExecutor`, 8 workers) with a single embed fetch per episode (parses `sUb` + `hD` from one `data-obf`).

### Encrypted m3u8 decryption

The response is UA-dependent: with the app's macOS Chrome UA the server returns a **plaintext** playlist; with other UAs it returns:

```
#ENC-AESGCM;iv=<12-byte-hex>
<base64-encoded-ciphertext>
```

**Key derivation**: `HMAC-SHA256(key="stream-derive-v1" as UTF-8 bytes, msg=videoHash as UTF-8 bytes)` → 32-byte AES-256 key.

The `videoHash` comes from `data-obf` → `{"sUb":"...","hD":"<hash>"}` field `hD`.

**Decryption**: AES-256-GCM with 12-byte IV (from `#ENC-AESGCM` tag), no additional authenticated data (AAD). The plaintext is a standard m3u8 playlist.

**Important**: The `#EXT-X-B65:0-138` tag sometimes present is just metadata indicating the first 138 bytes of the base64 payload — it is NOT used as AAD.

### Encryption key constant

The 18-byte string `"stream-derive-v1"` (UTF-8) is hardcoded in player.js. There is no per-episode or per-server variation.

### CDN segments

After decryption, the m3u8 references TS segments hosted on `sings*.amass2.top` (was `jps*.hihihoho4.top`). These segments may have a `.html` extension but are valid MPEG-TS.

Referer header must be the embed URL (CDN requires it). The CDN also requires a full browser User-Agent and compression (`Accept-Encoding`) — bare UA gets Cloudflare 403. yt-dlp's default headers work; do not override `Accept-Encoding` in `http_headers` or fragments arrive gzip-corrupted.

### Download

Decrypted m3u8 is written to a temp file. yt-dlp is invoked with `--enable-file-urls` to handle local file:// paths. `cryptography` library is required for AES-GCM (declared in `pyproject.toml`).

## UI

Episode checkboxes in `GridView` are wrapped as `ft.Row([ft.Checkbox(data=ep), ft.Text(..., selectable=True)])`. Access checkbox via `controls[0]`.

Download concurrency is per-**episode**, not per-fragment: `download_multiple(parallel=N)` runs episodes in a `ThreadPoolExecutor` (`pool.map` keeps result order), and yt-dlp's `concurrent_fragments` is left at its default (1). The "Concurrent Episodes" slider (1–8, default 1) is disabled unless ≥2 episodes are selected — checkbox `on_change`, Select/Deselect All, and the resolve rebuild all call `refresh_concurrency_state()`.

Progress UI is a terminal-style panel (black `Container` + monospace `ft.Text`): one in-place-updating line per episode (`> EP 3: [download] 45.3% of ~ 1.24GiB at 18.2MiB/s ETA 00:14`), throttled to ~3 updates/sec/episode via `last_progress_tick`, history capped at 100 lines, cleared on each download. There is no log file and no copy/clear buttons. All episode callbacks (`on_episode_start`/`done`/`progress`) fire from pool threads and marshal UI changes via the `ui()` helper.

`download_multiple` callback contract: `on_episode_done(ep, success, error)` — error is a **positional** third arg (empty string on success paths); handlers must accept it even on the error branches.

## Testing

No test suite exists. The scraper is regex-based against **live** site HTML that changes without notice — after any scraper/resolver change, verify against real pages:

```bash
uv run python -m py_compile nguonc_downloader.py nguonc_app.py
uv run python -c "
from nguonc_downloader import NguoncDownloader
d = NguoncDownloader('https://phim.nguonc.com/phim/ngu-dinh-dao')
info = d.scrape()
res = d.resolve_all_m3u8(0)
assert info['servers'] and sum(1 for r in res if r['m3u8']) >= len(res) - 1
print('OK', info['title'], info['year'])
"
```

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
