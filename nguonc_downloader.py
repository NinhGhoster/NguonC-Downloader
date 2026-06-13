import re
import json
import base64
import subprocess
import urllib.request
import urllib.error
import os
import urllib.parse
import threading
from pathlib import Path
from datetime import datetime
from typing import Optional, Callable

USER_AGENT = (
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
    "AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/120.0.0.0 Safari/537.36"
)


def _fetch(url: str, referer: str = "") -> str:
    req = urllib.request.Request(url, headers={
        "User-Agent": USER_AGENT,
    })
    if referer:
        req.add_header("Referer", referer)
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.read().decode("utf-8", errors="replace")


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

        m = re.search(r'var episodes\s*=\s*(\[.*?\]);', html, re.DOTALL)
        if not m:
            raise ValueError("Could not find episode data on page")

        self.servers = json.loads(m.group(1))

        return {
            "title": self.title,
            "english_title": self.english_title,
            "year": self.year,
            "director": self.director,
            "servers": self.servers,
        }

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

    def generate_filename(self, episode_num: str, season: int = 1) -> str:
        name = self.english_title or self.title
        safe_name = re.sub(r'[\\/*?:"<>|]', "", name).strip()
        dotted = re.sub(r'\s+', '.', safe_name)
        try:
            ep = int(episode_num)
        except ValueError:
            ep = 0
        return f"{dotted}.S{season:02d}E{ep:02d}.mp4"

    def resolve_all_m3u8(self, server_index: int = 0, season: int = 1) -> list[dict]:
        if not self.servers:
            self.scrape()
        if server_index >= len(self.servers):
            raise ValueError(f"Server index {server_index} out of range")

        server = self.servers[server_index]
        results = []
        for ep in server["list"]:
            try:
                m3u8_url = self.resolve_stream_url(ep["embed"])
            except Exception:
                m3u8_url = None
            results.append({
                "num": ep["name"],
                "embed": ep["embed"],
                "m3u8": m3u8_url,
                "filename": self.generate_filename(ep["name"], season=season),
            })
        return results

    @staticmethod
    def download_episode(
        m3u8_url: str,
        output_path: str,
        concurrent: int = 8,
        referer: str = "",
        on_progress: Optional[Callable[[str], None]] = None,
    ) -> bool:
        cmd = [
            "yt-dlp",
            "--concurrent-fragments", str(concurrent),
            "--output", output_path,
            "--no-part",
            "--progress-template", "download:%(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s",
            "--newline",
        ]
        if referer:
            cmd += ["--referer", referer]
        cmd.append(m3u8_url)

        try:
            proc = subprocess.Popen(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                bufsize=1,
            )
            for line in proc.stdout:
                line = line.strip()
                if on_progress:
                    on_progress(line)
            proc.wait()
            return proc.returncode == 0
        except FileNotFoundError:
            raise RuntimeError(
                "yt-dlp not found. Install it: brew install yt-dlp"
            )

    @staticmethod
    def download_multiple(
        episodes: list[dict],
        output_dir: str,
        server_name: str,
        referer: str,
        concurrent: int = 8,
        on_episode_start: Optional[Callable[[dict], None]] = None,
        on_episode_done: Optional[Callable[[dict, bool], None]] = None,
        on_progress: Optional[Callable[[dict, str], None]] = None,
    ) -> list[dict]:
        os.makedirs(output_dir, exist_ok=True)
        results = []

        for ep in episodes:
            if on_episode_start:
                on_episode_start(ep)

            if not ep["m3u8"]:
                if on_episode_done:
                    on_episode_done(ep, False)
                results.append({**ep, "success": False, "error": "No m3u8 URL"})
                continue

            safe_name = re.sub(r'[\\/*?:"<>|]', "", server_name).strip()
            episode_dir = os.path.join(output_dir, safe_name)
            os.makedirs(episode_dir, exist_ok=True)
            output_path = os.path.join(episode_dir, ep["filename"])

            if os.path.exists(output_path):
                if on_episode_done:
                    on_episode_done(ep, True)
                results.append({**ep, "success": True, "skipped": True})
                continue

            def _on_progress(line: str):
                if on_progress:
                    on_progress(ep, line)

            try:
                ok = NguoncDownloader.download_episode(
                    m3u8_url=ep["m3u8"],
                    output_path=output_path,
                    concurrent=concurrent,
                    referer=ep["embed"],
                    on_progress=_on_progress,
                )
                if on_episode_done:
                    on_episode_done(ep, ok)
                results.append({**ep, "success": ok})
            except Exception as e:
                if on_episode_done:
                    on_episode_done(ep, False)
                results.append({**ep, "success": False, "error": str(e)})

        return results
