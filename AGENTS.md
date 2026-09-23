# AGENTS.md — NguonC Downloader

## Entrypoints

- `nguonc_app.py` — Flet GUI (`uv run nguonc_app.py`)
- `nguonc_downloader.py` — engine: scrape → bootstrap/`data-obf` resolve → decrypt m3u8 → bundled `yt_dlp`

```bash
uv sync          # CI uses: uv sync --frozen
uv run nguonc_app.py
```

Python `>=3.10` (CI: 3.12). Runtime deps in `[project]`; `flet-cli` / `pyinstaller` are dev-group but installed by default `uv sync`. No external yt-dlp binary.

## Verify (no test suite / linter / typecheck)

```bash
uv run python -m py_compile nguonc_downloader.py nguonc_app.py
uv run python -c "
from nguonc_downloader import NguoncDownloader
d = NguoncDownloader('https://phim.nguonc.com/phim/gign-luc-luong-tinh-nhue')
info = d.scrape()
res = d.resolve_all_m3u8(0)
assert info['servers'] and res
available = sum(1 for r in res if r['m3u8'])
assert all(r['m3u8'] or r.get('error') for r in res)
print('OK', info['title'], info['year'], f'{available}/{len(res)} streams')
for r in res:
    if not r['m3u8']:
        print('UNAVAILABLE', r['num'], r['error'])
"
```

Scraper/resolver are regex-based against **live** HTML that changes without notice — after any resolver change, re-run against real pages. Expected per-episode failures (report; do not treat as missing URLs): Cloudflare `HTTP 403` (no h2/curl), `wrong_origin` (missing `Origin`), `Rate limited ... retry after Ns` (≈20 bootstrap grants/hour/IP — wait it out), `Turnstile verification required` (no preissued grant; no captcha solving is implemented).

## Building / CI

| Platform | Command | Output |
|----------|---------|--------|
| macOS | `bash build_macos.sh` | `dist/NguonC Downloader.app` → CI wraps `.dmg` |
| Windows | `uv run flet pack nguonc_app.py --name "NguonC Downloader" --icon assets/icon.ico` | `.exe` |
| Linux | `xvfb-run uv run flet pack nguonc_app.py --name "NguonC Downloader" --icon assets/icon.png` | renamed `.bin` |

- `build_macos.sh` (macOS only): patches `CFBundleName` in cached Flet.app, builds, restores. Path is hard-coded to `~/.flet/client/flet-desktop-full-0.85.3` while the Python package is **0.86.5** — bump both together if upgrading Flet.
- Runtime also patches `sys._MEIPASS/Flet.app` plist (`main()` in `nguonc_app.py`).
- CI: `.github/workflows/build.yml` — tag push (`*`), PR, or manual. Python 3.12, `uv sync --frozen`. Release job runs **only on tags** (`softprops/action-gh-release`). Tags: `YYYY.MM.DD` (force-push to move).
- Icons: `assets/icon.png` source; `icon.icns` / `icon.ico` already committed for pack.

## Flet quirks (locked 0.86.5; desktop client 0.85.3)

- `page.clipboard` is **read-only** — use `pbcopy` / `clip` / `xclip` subprocess.
- `page.scroll = ft.ScrollMode.AUTO` (not `ADAPTIVE`).
- No `ft.padding.symmetric()` — use `ft.Padding(...)`.
- `page.show_dialog(ft.SnackBar(...))`, not `page.snack_bar =`.
- `ft.Border.all(...)` not `ft.border.all(...)`.
- `ft.run(func)` — pass a function reference (`NguoncApp().build`), not an instance.
- **No main-thread marshaling API** (`page.run_thread` / `run_on_main` do not exist). `control.update()` from a worker thread races flet’s patch/diff (`on_app_lifecycle_state_change` → `__auto_update` → `ObjectPatch._compare_lists` → `IndexError`). Fix: `build()` captures `asyncio.get_running_loop()`; all worker-thread UI work goes through `loop.call_soon_threadsafe` (the `ui(fn)` helper). Never touch controls from worker threads.

## Architecture

`_fetch()` uses verified TLS (`_SECURE_CTX`); `INSECURE_HOSTS` is empty allowlist for `SSLCertVerificationError` only — never add a host without probing live.

### Episode data

`<div id="nc-episode-data">[{...}]</div>` (redesign ~2026-07); fallback `var episodes = [...]`. Server key `name` (was `server_name`); episode: `name`/`slug`/`embed`. Year: `https://phim.nguonc.com/api/film/{slug}` (category `Năm`), slug via `_extract_slug()`, HTML regex fallbacks.

### m3u8 resolution

**Primary (live ~2026-09): HTTP/2 bootstrap POST.** Embed host (`embed*.streamc.xyz`) is behind Cloudflare and **hard-blocks HTTP/1.1 with 403** — urllib cannot negotiate h2, so embed traffic goes through system `curl --http2` (`_curl_fetch`; curl ships with macOS / Win10+ / Linux). Flow (from the player’s own public JS):

1. `GET embed.php?hash=...` (h2) → shell HTML with `<script id="stream-bootstrap" type="application/json">{"api":"<embed url>"}</script>` (no `data-obf` on these pages).
2. `POST` that `api` URL (h2): `Content-Type: application/json`, **`Origin: <embed origin>`** (missing → `403 {"error":"wrong_origin"}`), `Referer: <embed url>`; body `{"action":"bootstrap","referrer":<movie url>,"frame_origins":[<movie origin>],"request_grant":true,"playlist_format":"aesgcm-v2","pretty_url":true,"path_chunks":true,"bootstrap_format":"aesgcm-v1"}`. No cookies (`credentials` omitted).
3. Response: AES-GCM envelope `{format:"aesgcm-v1", iv:<24-hex>, data:<b64>}`. Decrypt with key = AAD = `SHA-256(b"stream-bootstrap-v1\n" + api_href)` (`_open_bootstrap`). Plaintext: `video` (hash), `nonce`, `preissued.playlist` (signed URL, 4h TTL), `turnstileEnabled`.
4. `GET preissued.playlist` (h2, `Referer: <embed url>`) → `#ENC-AESGCM` playlist → decrypt → temp `.m3u8` (`delete=False` — leftovers in `$TMPDIR` until manually cleaned).

`429`/`503` + `Retry-After` → per-episode `Rate limited by embed host (retry after Ns)`.

**Legacy fallback only** when curl is missing or shell has no bootstrap script (`_NoBootstrap`): `data-obf` → `{"sUb","hD"}` → `{embed_domain}/{sUb}.m3u8`. `resolve_stream_url()` is this legacy path only.

`resolve_all_m3u8()`: `ThreadPoolExecutor`, 8 workers; per episode = shell GET + bootstrap POST + playlist GET.

### Encrypted playlist decrypt

If body contains `#ENC-AESGCM;iv=<12-byte-hex>` + base64 line (plaintext vs encrypted depends on UA/path; code decrypts only when the tag is present):

- Key: `HMAC-SHA256(key=b"stream-derive-v1", msg=videoHash)` → AES-256 key. Constant is fixed in player.js — no per-episode variation.
- `videoHash` = bootstrap `video` (primary) or `data-obf` `hD` (legacy).
- AES-256-GCM, 12-byte IV from the tag, **no AAD**. `#EXT-X-B65:0-138` is metadata only — not AAD.

### CDN segments

TS on `jps*.hihihoho4.top` (also seen `sings*.amass2.top`); `.html`/`.png` extensions are still MPEG-TS. CDN allows HTTP/1.1 (yt-dlp as-is); only the embed host needs h2.

- Segment `Referer` must be the **embed URL** (`download_episode(..., referer=ep["embed"])`).
- Needs full browser UA + `Accept-Encoding` — bare UA → CF 403. Do not override `Accept-Encoding` in `http_headers` or fragments arrive gzip-corrupted.
- `download_multiple`’s `referer` parameter is currently **unused** (always uses `ep["embed"]`).

### Download

Decrypted playlist → temp file; yt-dlp with `enable_file_urls` for local `file://`. `cryptography` required for AES-GCM.

## UI

- Episode rows: `ft.Row([ft.Checkbox(data=ep), ft.Text(...)])` — checkbox is `controls[0]`.
- Concurrency is **per-episode** (`download_multiple(parallel=N)`); yt-dlp `concurrent_fragments` stays at default 1. Slider 1–8 default 1, disabled unless ≥2 checked — `refresh_concurrency_state()` from checkbox `on_change`, Select/Deselect All, and resolve rebuild.
- Terminal panel: in-place line per episode, throttle 0.3s (`last_progress_tick`), history cap 100, cleared each download. No log file / copy / clear buttons. Callbacks fire from pool threads → marshal via `ui()`.
- `download_multiple` contract: `on_episode_done(ep, success, error)` — error is a **positional** 3rd arg (`""` on success and on skip-existing); handlers must accept it on every branch.
- Resolve results can go stale if the user re-selects a server: `update_episodes` bumps `self._resolve_seq` and drops out-of-date worker results — keep that guard when touching resolve UI.
