import re
import json
import base64
import ssl
import urllib.request
import urllib.error
import os
import urllib.parse
import shutil
import subprocess
import threading
import tempfile
import hmac
import hashlib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from datetime import datetime
from typing import Optional, Callable

USER_AGENT = (
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
    "AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/120.0.0.0 Safari/537.36"
)

# Hosts with broken TLS certs. All requests are verified by default; only
# hosts listed here fall back to an unverified connection.
INSECURE_HOSTS: set[str] = set()

_SECURE_CTX = ssl.create_default_context()


def _insecure_context() -> ssl.SSLContext:
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


def _format_resolve_error(embed_url: str, error: Exception) -> str:
    host = urllib.parse.urlparse(embed_url).hostname or "embed host"
    if isinstance(error, urllib.error.HTTPError):
        server = (error.headers.get("Server", "") or "").lower()
        cloudflare = " (Cloudflare)" if "cloudflare" in server or error.headers.get("CF-RAY") else ""
        return f"HTTP {error.code} from {host}{cloudflare}"
    if isinstance(error, urllib.error.URLError):
        reason = error.reason
        if isinstance(reason, ssl.SSLCertVerificationError):
            return f"TLS certificate verification failed for {host}"
        return f"Network error from {host}: {reason}"
    message = str(error).strip()
    return message or f"{type(error).__name__} from {host}"


def _fetch(url: str, referer: str = "") -> str:
    host = (urllib.parse.urlparse(url).hostname or "").lower()
    req = urllib.request.Request(url, headers={
        "User-Agent": USER_AGENT,
    })
    if referer:
        req.add_header("Referer", referer)
    try:
        with urllib.request.urlopen(req, timeout=30, context=_SECURE_CTX) as resp:
            return resp.read().decode("utf-8", errors="replace")
    except urllib.error.URLError as e:
        if (
            host in INSECURE_HOSTS
            and isinstance(e.reason, ssl.SSLCertVerificationError)
        ):
            with urllib.request.urlopen(req, timeout=30, context=_insecure_context()) as resp:
                return resp.read().decode("utf-8", "replace")
        raise


_BOOTSTRAP_AAD_PREFIX = "stream-bootstrap-v1\n"


class _NoBootstrap(ValueError):
    """Embed page uses the legacy data-obf format (no bootstrap script)."""


def _curl_available() -> bool:
    return shutil.which("curl") is not None


_CURL_HTTP2: Optional[bool] = None


def _curl_supports_http2() -> bool:
    """True when the system curl can speak HTTP/2 (required by embed host)."""
    global _CURL_HTTP2
    if _CURL_HTTP2 is not None:
        return _CURL_HTTP2

    curl = shutil.which("curl")
    if not curl:
        _CURL_HTTP2 = False
        return _CURL_HTTP2

    # `curl --http2 --version` exits non-zero on builds without HTTP/2
    # ("option --http2: the installed libcurl version does not support this").
    try:
        proc = subprocess.run(
            [curl, "-sS", "--http2", "--version"],
            capture_output=True,
            timeout=10,
        )
        if proc.returncode == 0:
            _CURL_HTTP2 = True
            return _CURL_HTTP2
    except Exception:
        pass

    # Fallback: Features line on `curl --version` lists HTTP2 when enabled.
    try:
        proc = subprocess.run([curl, "--version"], capture_output=True, timeout=10)
        out = (proc.stdout + b"\n" + proc.stderr).decode("utf-8", "replace")
        _CURL_HTTP2 = "HTTP2" in out or "HTTP/2" in out
    except Exception:
        _CURL_HTTP2 = False
    return _CURL_HTTP2


def _curl_fetch(
    url: str,
    referer: str = "",
    method: str = "GET",
    headers: Optional[dict] = None,
    body: Optional[str] = None,
    timeout: int = 30,
) -> tuple[int, bytes, dict[str, str]]:
    """HTTP/2 fetch for embed traffic. Prefer curl --http2; fall back to httpx."""
    if _curl_available() and _curl_supports_http2():
        return _curl_http2_fetch(
            url, referer=referer, method=method,
            headers=headers, body=body, timeout=timeout,
        )
    if _h2_available():
        return _http2_fetch(
            url, referer=referer, method=method,
            headers=headers, body=body, timeout=timeout,
        )
    if not _curl_available():
        raise RuntimeError("curl is not available on PATH")
    raise RuntimeError(
        "System curl has no HTTP/2 support and httpx[h2] is not installed. "
        "Embed host requires HTTP/2. Install a newer curl (e.g. "
        "`brew install curl`) or `pip install 'httpx[http2]'`."
    )


def _curl_http2_fetch(
    url: str,
    referer: str = "",
    method: str = "GET",
    headers: Optional[dict] = None,
    body: Optional[str] = None,
    timeout: int = 30,
) -> tuple[int, bytes, dict[str, str]]:
    body_path = None
    out_fd, out_path = tempfile.mkstemp(suffix=".curlout")
    os.close(out_fd)
    try:
        all_headers = dict(headers or {})
        all_headers.setdefault("User-Agent", USER_AGENT)
        if referer:
            all_headers.setdefault("Referer", referer)
        cmd = ["curl", "-sS", "--http2", "--compressed",
            "-m", str(timeout),
            "-o", out_path, "-D", "-", "-w", "\n%{http_code}",
        ]
        if method.upper() != "GET":
            cmd += ["-X", method.upper()]
        for key, value in all_headers.items():
            cmd += ["-H", f"{key}: {value}"]
        if body is not None:
            fd, body_path = tempfile.mkstemp(suffix=".curlbody")
            with os.fdopen(fd, "w", encoding="utf-8") as fh:
                fh.write(body)
            cmd += ["--data-binary", f"@{body_path}"]
        cmd.append(url)

        proc = subprocess.run(cmd, capture_output=True, timeout=timeout + 10)
        if proc.returncode != 0:
            err = proc.stderr.decode("utf-8", "replace").strip()
            if "does not support this" in err and "--http2" in err:
                _CURL_HTTP2 = False
            raise RuntimeError(f"curl failed for {url}: {err or f'exit {proc.returncode}'}")

        raw = proc.stdout
        sep = raw.rfind(b"\n\n")
        if sep < 0:
            raise RuntimeError(f"unexpected curl response from {url}")
        head_block = raw[:sep].decode("latin-1", "replace")
        status = int(raw[sep + 2:].strip() or 0)
        resp_headers: dict[str, str] = {}
        for line in head_block.splitlines():
            if ":" in line and not line.startswith(("HTTP/", "  ")):
                k, v = line.split(":", 1)
                resp_headers[k.strip().lower()] = v.strip()
        with open(out_path, "rb") as fh:
            resp_body = fh.read()
        return status, resp_body, resp_headers
    finally:
        for path in (body_path, out_path):
            if path:
                try:
                    os.unlink(path)
                except OSError:
                    pass


def _h2_available() -> bool:
    """True when httpx can negotiate HTTP/2 (fallback when curl has no HTTP/2)."""
    try:
        import httpx  # noqa: F401
        import h2  # noqa: F401
        return True
    except Exception:
        return False


def _can_h2() -> bool:
    """True when we have any HTTP/2 client for the embed host."""
    if _curl_available() and _curl_supports_http2():
        return True
    return _h2_available()


def _http2_fetch(
    url: str,
    referer: str = "",
    method: str = "GET",
    headers: Optional[dict] = None,
    body: Optional[str] = None,
    timeout: int = 30,
) -> tuple[int, bytes, dict[str, str]]:
    """HTTP/2 fetch via httpx — used when system curl lacks HTTP/2 support."""
    import httpx

    all_headers = dict(headers or {})
    all_headers.setdefault("User-Agent", USER_AGENT)
    if referer:
        all_headers.setdefault("Referer", referer)

    with httpx.Client(http2=True, timeout=timeout, follow_redirects=True) as client:
        resp = client.request(
            method.upper(),
            url,
            headers=all_headers,
            content=body.encode("utf-8") if body is not None else None,
        )
        resp_headers = {k.lower(): v for k, v in resp.headers.items()}
        return resp.status_code, resp.content, resp_headers


class NguoncDownloader:

    def __init__(self, url: str):
        self.url = url.rstrip("/")
        self.title: str = ""
        self.english_title: str = ""
        self.year: str = ""
        self.director: str = ""
        self.servers: list[dict] = []

    @staticmethod
    def _extract_slug(url: str) -> str | None:
        m = re.search(r'/phim/([^/]+?)(?:-\d+)?(?:\.html?)?$', url)
        if m:
            return m.group(1).rstrip("/")
        return None

    @staticmethod
    def _fetch_year_from_api(slug: str) -> str:
        try:
            api_url = f"https://phim.nguonc.com/api/film/{slug}"
            resp = _fetch(api_url)
            data = json.loads(resp)
            for cat in data.get("movie", {}).get("category", {}).values():
                if cat.get("group", {}).get("name") == "Năm":
                    years = [x["name"] for x in cat.get("list", [])]
                    if years:
                        return years[0]
        except Exception:
            pass
        return ""

    def _fetch_year_from_html(self, html: str) -> str:
        m = re.search(r'"(?:dateCreated|datePublished|releaseDate)":"?(\d{4})', html)
        if m:
            return m.group(1)
        m = re.search(r'itemprop=["\']name["\'][^>]*>(\d{4})<', html)
        if m:
            return m.group(1)
        m = re.search(r'Nam[^<]*<[^>]*>[^<]*(\d{4})', html)
        if m:
            return m.group(1)
        return ""

    def scrape(self) -> dict:
        html = _fetch(self.url)

        m = re.search(r'<title>(.*?)</title>', html)
        if m:
            self.title = m.group(1).strip()
            parts = self.title.split(" - ", 1)
            if len(parts) == 2:
                self.english_title = parts[1].strip()
            else:
                self.english_title = self.title

        slug = self._extract_slug(self.url)
        if slug:
            self.year = self._fetch_year_from_api(slug)

        if not self.year:
            self.year = self._fetch_year_from_html(html)

        m = re.search(r'"director":"([^"]*)"', html)
        if m:
            self.director = m.group(1)

        self.servers = self._fetch_servers(html)

        return {
            "title": self.title,
            "english_title": self.english_title,
            "year": self.year,
            "director": self.director,
            "servers": self.servers,
        }

    def _fetch_servers(self, html: str) -> list[dict]:
        # Primary (site redesign 2026-09-24): data-episode-url points to a JSON
        # endpoint returning {"servers":[{"name","list":[{name,slug,embed}]}]}.
        m = re.search(r'data-episode-url="([^"]+)"', html)
        if m:
            ep_url = m.group(1)
            try:
                data = json.loads(_fetch(ep_url, referer=self.url))
                servers = data.get("servers") if isinstance(data, dict) else None
                if servers:
                    return servers
            except Exception:
                pass

        # Fallback: inline episode data (legacy redesign ~2026-07).
        m = re.search(r'id="nc-episode-data">(\[.*?\])</', html, re.DOTALL)
        if not m:
            m = re.search(r'var episodes\s*=\s*(\[.*?\]);', html, re.DOTALL)
        if m:
            return json.loads(m.group(1))

        # Last resort: conventional /episodes endpoint next to the movie URL.
        try:
            data = json.loads(_fetch(self.url + "/episodes", referer=self.url))
            servers = data.get("servers") if isinstance(data, dict) else None
            if servers:
                return servers
        except Exception:
            pass

        raise ValueError("Could not find episode data on page")

    @staticmethod
    def _decrypt_m3u8(encrypted_content: str, video_hash: str) -> str:
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM

        lines = encrypted_content.strip().split("\n")

        iv_hex = None
        data_line = None
        for line in lines:
            if "#ENC-AESGCM" in line:
                m = re.search(r"iv=([0-9a-fA-F]+)", line)
                if m:
                    iv_hex = m.group(1)
            elif not line.startswith("#") and line.strip():
                data_line = line.strip()

        if not iv_hex or not data_line:
            raise ValueError("Could not parse encrypted m3u8")

        key = hmac.new(
            b"stream-derive-v1",
            video_hash.encode("utf-8"),
            hashlib.sha256,
        ).digest()

        iv = bytes.fromhex(iv_hex)
        ciphertext = base64.b64decode(data_line)

        plaintext = AESGCM(key).decrypt(iv, ciphertext, None)
        return plaintext.decode("utf-8")

    @staticmethod
    def _open_bootstrap(envelope: dict, api_href: str) -> dict:
        """Decrypt the player's aesgcm-v1 bootstrap envelope.

        Key derivation (from the embed page's own public JS):
        key = AAD = SHA-256(b"stream-bootstrap-v1\\n" + api_href)
        AES-256-GCM, 12-byte IV from the envelope's hex `iv`.
        """
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM

        if envelope.get("format") != "aesgcm-v1":
            raise ValueError(f"Unsupported bootstrap format: {envelope.get('format')!r}")
        iv_hex = envelope.get("iv", "")
        data_b64 = envelope.get("data", "")
        if not re.fullmatch(r"[0-9a-fA-F]{24}", iv_hex) or not data_b64:
            raise ValueError("Malformed bootstrap envelope")

        aad = f"{_BOOTSTRAP_AAD_PREFIX}{api_href}".encode("utf-8")
        key = hashlib.sha256(aad).digest()
        plaintext = AESGCM(key).decrypt(
            bytes.fromhex(iv_hex), base64.b64decode(data_b64), aad,
        )
        return json.loads(plaintext.decode("utf-8"))

    def _bootstrap_playlist(self, embed_url: str) -> tuple[str, str]:
        """Resolve an embed URL through the player's bootstrap POST flow.

        Returns (playlist_text, video_hash). Playlist text is the decrypted
        m3u8 (or the raw text when the server returns it unencrypted).
        The embed host blocks HTTP/1.1 with Cloudflare 403, so all embed
        requests go through curl's HTTP/2.

        Raises _NoBootstrap when the page has no stream-bootstrap script
        (legacy data-obf page) — callers may fall back. All other errors
        are real failures and should propagate.
        """
        parsed = urllib.parse.urlparse(embed_url)
        origin = f"{parsed.scheme}://{parsed.netloc}"

        status, html_bytes, _ = _curl_fetch(
            embed_url, referer=self.url, timeout=30,
        )
        if status != 200:
            raise ValueError(f"HTTP {status} from {parsed.hostname or 'embed host'}")
        html = html_bytes.decode("utf-8", "replace")

        boot_m = re.search(
            r'<script id="stream-bootstrap"[^>]*>(.*?)</script>', html, re.DOTALL,
        )
        if not boot_m:
            raise _NoBootstrap("stream-bootstrap script missing from embed page")
        boot_meta = json.loads(boot_m.group(1))
        api_href = boot_meta.get("api")
        if not api_href or urllib.parse.urlparse(api_href).netloc != parsed.netloc:
            raise ValueError("Invalid bootstrap api URL")

        hash_match = re.search(r"[?&]hash=([0-9a-f]+)", embed_url)
        video_hash = hash_match.group(1) if hash_match else ""

        movie_parsed = urllib.parse.urlparse(self.url)
        movie_origin = f"{movie_parsed.scheme}://{movie_parsed.netloc}" if self.url else ""
        post_body = json.dumps({
            "action": "bootstrap",
            "referrer": self.url[:4096],
            "frame_origins": [movie_origin] if movie_origin else [],
            "request_grant": True,
            "playlist_format": "aesgcm-v2",
            "pretty_url": True,
            "path_chunks": True,
            "bootstrap_format": "aesgcm-v1",
        })

        status, resp_bytes, resp_headers = _curl_fetch(
            api_href,
            method="POST",
            headers={
                "Content-Type": "application/json",
                "Origin": origin,
                "Referer": embed_url,
            },
            body=post_body,
            timeout=30,
        )
        if status in (429, 503):
            retry = resp_headers.get("retry-after", "60")
            raise ValueError(f"Rate limited by embed host (retry after {retry}s)")
        if status != 200:
            err_detail = ""
            try:
                err_detail = json.loads(resp_bytes).get("error", "")
            except Exception:
                pass
            host = parsed.hostname or "embed host"
            raise ValueError(
                f"HTTP {status} from {host} (bootstrap{': ' + err_detail if err_detail else ''})"
            )

        envelope = json.loads(resp_bytes)
        if envelope.get("format") == "aesgcm-v1":
            opened = self._open_bootstrap(envelope, api_href)
        else:
            opened = envelope
        video_hash = opened.get("video") or video_hash

        if opened.get("turnstileEnabled") and not opened.get("preissued"):
            raise ValueError("Turnstile verification required (no preissued grant)")

        preissued = opened.get("preissued") or {}
        playlist_url = preissued.get("playlist")
        if not playlist_url:
            raise ValueError("Bootstrap response has no preissued playlist")
        if urllib.parse.urlparse(playlist_url).netloc != parsed.netloc:
            raise ValueError("Playlist URL on unexpected host")

        status, pl_bytes, _ = _curl_fetch(
            playlist_url, referer=embed_url, timeout=30,
        )
        if status != 200:
            raise ValueError(f"HTTP {status} fetching playlist")
        playlist_text = pl_bytes.decode("utf-8", "replace")

        if "#ENC-AESGCM" in playlist_text:
            playlist_text = self._decrypt_m3u8(playlist_text, video_hash)

        return playlist_text, video_hash

    def _write_playlist(self, playlist_text: str) -> str:
        tmp = tempfile.NamedTemporaryFile(mode="w", suffix=".m3u8", delete=False)
        tmp.write(playlist_text)
        tmp.close()
        return tmp.name

    def resolve_stream_url(self, embed_url: str) -> str:
        html = _fetch(embed_url, referer=self.url)

        m = re.search(r'data-obf="([^"]+)"', html)
        if not m:
            raise ValueError(f"Could not find data-obf in {embed_url}")

        data_obf = m.group(1)
        stream_data = json.loads(base64.b64decode(data_obf).decode())
        sub_base64 = stream_data["sUb"]

        parsed = urllib.parse.urlparse(embed_url)
        base_domain = f"{parsed.scheme}://{parsed.netloc}"
        return f"{base_domain}/{sub_base64}.m3u8"

    def _resolve_one(self, ep: dict, season: int = 1) -> dict:
        embed_url = ep["embed"]
        used_bootstrap = False

        if not _can_h2():
            raise RuntimeError(
                "Embed host requires HTTP/2 but no client is available: "
                "system curl lacks --http2 and httpx[h2] is not installed. "
                "Install a newer curl (e.g. `brew install curl`) or "
                "`pip install 'httpx[http2]'`."
            )

        try:
            playlist_text, _ = self._bootstrap_playlist(embed_url)
            m3u8_url = self._write_playlist(playlist_text)
            used_bootstrap = True
        except _NoBootstrap:
            # Legacy data-obf page — fall through below.
            used_bootstrap = False

        if not used_bootstrap:
            embed_html = _fetch(embed_url, referer=self.url)

            obf_m = re.search(r'data-obf="([^"]+)"', embed_html)
            if not obf_m:
                raise ValueError(f"Could not find data-obf in {embed_url}")

            stream_data = json.loads(base64.b64decode(obf_m.group(1)).decode())
            sub_base64 = stream_data["sUb"]
            video_hash = stream_data.get("hD", "")

            parsed = urllib.parse.urlparse(embed_url)
            base_domain = f"{parsed.scheme}://{parsed.netloc}"
            encrypted_url = f"{base_domain}/{sub_base64}.m3u8"

            encrypted = _fetch(encrypted_url, referer=embed_url)
            if "#ENC-AESGCM" in encrypted:
                decrypted = self._decrypt_m3u8(encrypted, video_hash)
                m3u8_url = self._write_playlist(decrypted)
            else:
                m3u8_url = encrypted_url

        return {
            "num": ep["name"],
            "embed": ep["embed"],
            "m3u8": m3u8_url,
            "filename": self.generate_filename(ep["name"], season=season),
        }

    def generate_filename(self, episode_num: str, season: int = 1) -> str:
        name = self.english_title or self.title
        safe_name = re.sub(r'[\\/*?:"<>|]', "", name).strip()
        dotted = re.sub(r'\s+', '.', safe_name)
        try:
            ep = int(episode_num)
        except ValueError:
            ep = 0
        return f"{dotted}.S{season:02d}E{ep:02d}.mp4"

    def resolve_all_m3u8(
        self,
        server_index: int = 0,
        season: int = 1,
        workers: int = 8,
    ) -> list[dict]:
        if not self.servers:
            self.scrape()
        if server_index >= len(self.servers):
            raise ValueError(f"Server index {server_index} out of range")

        server = self.servers[server_index]
        episodes = server["list"]

        def resolve_one(ep: dict) -> dict:
            try:
                return self._resolve_one(ep, season=season)
            except Exception as ex:
                return {
                    "num": ep["name"],
                    "embed": ep["embed"],
                    "m3u8": None,
                    "error": _format_resolve_error(ep["embed"], ex),
                    "filename": self.generate_filename(ep["name"], season=season),
                }

        if len(episodes) <= 1:
            return [resolve_one(ep) for ep in episodes]

        with ThreadPoolExecutor(max_workers=min(workers, len(episodes))) as pool:
            return list(pool.map(resolve_one, episodes))

    @staticmethod
    def download_episode(
        m3u8_url: str,
        output_path: str,
        referer: str = "",
        on_progress: Optional[Callable[[str], None]] = None,
    ) -> bool:
        import yt_dlp

        def hook(d):
            if on_progress:
                status = d.get("status", "")
                if status == "downloading":
                    pct = d.get("_percent_str", "").strip()
                    speed = d.get("_speed_str", "").strip()
                    eta = d.get("_eta_str", "").strip()
                    total = d.get("_total_bytes_str", d.get("_total_bytes_estimate_str", "?"))
                    on_progress(f"[download] {pct} of {total} at {speed} ETA {eta}")
                elif status == "finished":
                    on_progress(f"[download] 100% - {d.get('_total_bytes_str', '?')} downloaded")
                elif status == "error":
                    on_progress(f"[download] ERROR: {d.get('error', 'Unknown error')}")

        opts = {
            "outtmpl": output_path,
            "quiet": True,
            "no_warnings": True,
            "progress_hooks": [hook],
        }
        if referer:
            opts["http_headers"] = {"Referer": referer}

        is_local = not m3u8_url.startswith("http://") and not m3u8_url.startswith("https://")
        if is_local:
            opts["enable_file_urls"] = True
            url = Path(m3u8_url).resolve().as_uri()
        else:
            url = m3u8_url

        try:
            with yt_dlp.YoutubeDL(opts) as ydl:
                failed = ydl.download([url])
            return failed == 0
        except Exception as e:
            if on_progress:
                on_progress(f"[download] ERROR: {e}")
            return False

    @staticmethod
    def download_multiple(
        episodes: list[dict],
        output_dir: str,
        folder_name: str,
        referer: str,
        parallel: int = 1,
        on_episode_start: Optional[Callable[[dict], None]] = None,
        on_episode_done: Optional[Callable[[dict, bool, str], None]] = None,
        on_progress: Optional[Callable[[dict, str], None]] = None,
    ) -> list[dict]:
        os.makedirs(output_dir, exist_ok=True)
        safe_name = re.sub(r'[\\/*?:"<>|]', "", folder_name).strip()
        episode_dir = os.path.join(output_dir, safe_name)
        os.makedirs(episode_dir, exist_ok=True)

        def download_one(ep: dict) -> dict:
            if on_episode_start:
                on_episode_start(ep)

            if not ep["m3u8"]:
                error = ep.get("error") or "No m3u8 URL"
                if on_episode_done:
                    on_episode_done(ep, False, error)
                return {**ep, "success": False, "error": error}

            output_path = os.path.join(episode_dir, ep["filename"])

            if os.path.exists(output_path):
                if on_episode_done:
                    on_episode_done(ep, True, "")
                return {**ep, "success": True, "skipped": True}

            def _on_progress(line: str):
                if on_progress:
                    on_progress(ep, line)

            try:
                ok = NguoncDownloader.download_episode(
                    m3u8_url=ep["m3u8"],
                    output_path=output_path,
                    referer=ep["embed"],
                    on_progress=_on_progress,
                )
                if on_episode_done:
                    on_episode_done(ep, ok, "")
                return {**ep, "success": ok}
            except Exception as e:
                if on_episode_done:
                    on_episode_done(ep, False, str(e))
                return {**ep, "success": False, "error": str(e)}

        if len(episodes) <= 1 or parallel <= 1:
            return [download_one(ep) for ep in episodes]

        with ThreadPoolExecutor(max_workers=min(parallel, len(episodes))) as pool:
            return list(pool.map(download_one, episodes))
