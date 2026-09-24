import asyncio
import os
import sys
import threading
import re
import subprocess
import time
import flet as ft
from nguonc_downloader import NguoncDownloader


def snack(page: ft.Page, msg: str):
    page.show_dialog(ft.SnackBar(ft.Text(msg, selectable=True)))


class NguoncApp:

    def __init__(self):
        self.downloader: NguoncDownloader | None = None
        self.episodes_resolved: list[dict] = []
        self.downloading = False
        self._resolve_seq = 0

    def build(self, page: ft.Page):
        page.title = "NguonC Downloader"
        page.theme_mode = ft.ThemeMode.SYSTEM
        page.window_width = 860
        page.window_height = 720
        page.window_min_width = 700
        page.window_min_height = 600
        page.padding = 20

        page.theme = ft.Theme(
            color_scheme=ft.ColorScheme(
                primary=ft.Colors.INDIGO,
                primary_container=ft.Colors.INDIGO_100,
            ),
        )
        page.dark_theme = ft.Theme(
            color_scheme=ft.ColorScheme(
                primary=ft.Colors.INDIGO_200,
                primary_container=ft.Colors.INDIGO_800,
            ),
        )

        # Flet 0.86 has no main-thread marshaling API; worker threads must
        # not touch controls directly (races with flet's own patch/diff on
        # lifecycle events -> IndexError in ObjectPatch). Route all UI work
        # through this helper onto the app's asyncio loop thread.
        loop = asyncio.get_running_loop()

        def ui(fn):
            try:
                loop.call_soon_threadsafe(fn)
            except Exception:
                pass

        border_color = ft.Colors.OUTLINE

        def toggle_theme(e):
            if page.theme_mode == ft.ThemeMode.SYSTEM:
                page.theme_mode = ft.ThemeMode.DARK
            elif page.theme_mode == ft.ThemeMode.DARK:
                page.theme_mode = ft.ThemeMode.LIGHT
            else:
                page.theme_mode = ft.ThemeMode.SYSTEM
            theme_btn.icon = {
                ft.ThemeMode.SYSTEM: ft.Icons.BRIGHTNESS_AUTO,
                ft.ThemeMode.DARK: ft.Icons.DARK_MODE,
                ft.ThemeMode.LIGHT: ft.Icons.LIGHT_MODE,
            }[page.theme_mode]
            page.update()

        theme_btn = ft.IconButton(
            icon=ft.Icons.BRIGHTNESS_AUTO,
            tooltip="Theme: System (click to cycle System → Dark → Light)",
            on_click=toggle_theme,
        )

        status_bar = ft.Container(
            content=ft.Text("Ready", size=13, selectable=True),
            padding=ft.Padding(12, 8, 12, 8),
            border_radius=8,
            bgcolor=ft.Colors.with_opacity(0.12, ft.Colors.ON_SURFACE),
        )

        def set_status(msg: str, color=None):
            status_bar.content = ft.Text(msg, size=13, color=color, selectable=True)
            page.update()

        url_field = ft.TextField(
            label="Movie URL",
            hint_text="https://phim.nguonc.com/phim/...",
            prefix_icon=ft.Icons.LINK,
            expand=True,
            on_submit=lambda _: load_movie(),
        )

        load_btn = ft.Button(
            "Load Movie",
            icon=ft.Icons.SEARCH,
            on_click=lambda _: load_movie(),
        )

        title_text = ft.Text(size=22, weight=ft.FontWeight.BOLD, selectable=True)
        subtitle_text = ft.Text(size=14, color=ft.Colors.GREY_600, selectable=True)
        year_field = ft.TextField(
            label="Year",
            width=100,
            hint_text="",
            read_only=True,
        )

        server_dropdown = ft.Dropdown(
            label="Server / Source",
            width=400,
            on_select=lambda e: update_episodes(),
        )

        episodes_grid = ft.GridView(
            expand=True,
            runs_count=4,
            max_extent=100,
            spacing=8,
            run_spacing=8,
            child_aspect_ratio=2.5,
        )

        select_all_btn = ft.TextButton("Select All", on_click=lambda _: toggle_all(True))
        deselect_all_btn = ft.TextButton("Deselect All", on_click=lambda _: toggle_all(False))

        concurrent_slider = ft.Slider(
            min=1,
            max=8,
            value=1,
            divisions=7,
            label="{value}",
            width=300,
            disabled=True,
        )
        concurrent_label = ft.Text("1", size=14, selectable=True)

        def on_concurrent_change(e):
            concurrent_label.value = str(int(concurrent_slider.value))
            concurrent_label.update()

        concurrent_slider.on_change = on_concurrent_change

        def refresh_concurrency_state():
            count = 0
            for c in episodes_grid.controls:
                if (
                    isinstance(c, ft.Row)
                    and c.controls
                    and isinstance(c.controls[0], ft.Checkbox)
                    and c.controls[0].value
                ):
                    count += 1
            enabled = count >= 2
            concurrent_slider.disabled = not enabled
            if not enabled and int(concurrent_slider.value) > 1:
                concurrent_slider.value = 1
                concurrent_label.value = "1"
            page.update()

        create_subfolder_cb = ft.Checkbox(
            label="Create movie name subfolder",
            value=True,
            tooltip="Create a subfolder named after the movie title inside the output directory",
        )

        output_path_field = ft.TextField(
            label="Output Directory",
            value=str(os.path.expanduser("~/Downloads")),
            expand=True,
        )

        def pick_directory(e):
            def set_path(path: str):
                ui(lambda: setattr(output_path_field, "value", path))
                ui(lambda: output_path_field.update())

            def _pick():
                try:
                    if sys.platform == "darwin":
                        script = '''
                        set theFolder to choose folder with prompt "Select download directory"
                        set thePath to POSIX path of theFolder
                        return thePath
                        '''
                        result = subprocess.run(
                            ["osascript", "-e", script],
                            capture_output=True, text=True, timeout=30
                        )
                        if result.returncode == 0:
                            path = result.stdout.strip()
                            if path:
                                set_path(path)
                    elif sys.platform == "win32":
                        script = '''
                        Add-Type -AssemblyName System.Windows.Forms
                        $f = New-Object System.Windows.Forms.FolderBrowserDialog
                        $f.Description = "Select download directory"
                        $f.ShowDialog() | Out-Null
                        if ($f.SelectedPath) { Write-Output $f.SelectedPath }
                        '''
                        result = subprocess.run(
                            ["powershell", "-NoProfile", "-Command", script],
                            capture_output=True, text=True, timeout=30
                        )
                        if result.returncode == 0:
                            path = result.stdout.strip()
                            if path:
                                set_path(path)
                    else:
                        for cmd in [["zenity", "--file-selection", "--directory"],
                                    ["kdialog", "--getexistingdirectory"]]:
                            try:
                                result = subprocess.run(
                                    cmd, capture_output=True, text=True, timeout=30
                                )
                                if result.returncode == 0:
                                    path = result.stdout.strip()
                                    if path:
                                        set_path(path)
                                    break
                            except FileNotFoundError:
                                continue
                except Exception:
                    pass
            threading.Thread(target=_pick, daemon=True).start()

        output_picker = ft.IconButton(
            icon=ft.Icons.FOLDER_OPEN,
            on_click=pick_directory,
        )

        download_btn = ft.Button(
            "Download Selected",
            icon=ft.Icons.DOWNLOAD,
            disabled=True,
            style=ft.ButtonStyle(
                color=ft.Colors.WHITE,
                bgcolor=ft.Colors.INDIGO,
            ),
            on_click=lambda _: start_download(),
        )

        terminal_view = ft.ListView(
            expand=True,
            spacing=2,
            height=380,
        )
        terminal_view.controls.append(
            ft.Text(
                "$ NguonC Downloader - ready",
                size=13,
                font_family="monospace",
                color=ft.Colors.GREY_400,
                selectable=True,
            )
        )

        def load_movie():
            try:
                url = (url_field.value or "").strip()
                if not url:
                    set_status("Error: Please enter a URL ❌", ft.Colors.RED)
                    return

                load_btn.disabled = True
                load_btn.text = "Loading..."
                title_text.value = ""
                subtitle_text.value = ""
                year_field.value = ""
                server_dropdown.options = []
                server_dropdown.value = None
                download_btn.disabled = True
                episodes_grid.controls.clear()
                set_status("Loading... ⏳")
                page.update()
            except Exception as ex:
                set_status(f"Error: {ex} ❌", ft.Colors.RED)
                return

            def do_load():
                try:
                    d = NguoncDownloader(url)
                    info = d.scrape()

                    def apply():
                        self.downloader = d
                        title_text.value = info["english_title"] or info["title"]

                        servers = info.get("servers", [])
                        if not servers:
                            set_status("Error: No servers found for this movie \u274c", ft.Colors.RED)
                            return
                        ep_count = len(servers[0].get("list", []))
                        if info["year"]:
                            subtitle_text.value = f"{info['year']}  |  {ep_count} episodes"
                            year_field.value = info["year"]
                        else:
                            subtitle_text.value = f"{ep_count} episodes"

                        server_dropdown.options = [
                            ft.dropdown.Option(
                                str(i),
                                s.get("server_name") or s.get("name") or f"Server {i + 1}",
                            )
                            for i, s in enumerate(servers)
                        ]
                        server_dropdown.value = "0"
                        download_btn.disabled = False

                        update_episodes()
                        set_status(f"Loaded: {info['english_title'] or info['title']} \u2713")

                    ui(apply)
                except Exception as ex:
                    ui(lambda: set_status(f"Error: {ex} \u274c", ft.Colors.RED))
                finally:
                    def restore():
                        load_btn.disabled = False
                        load_btn.text = "Load Movie"
                        page.update()
                    ui(restore)

            threading.Thread(target=do_load, daemon=True).start()

        def update_episodes():
            if not self.downloader or server_dropdown.value is None:
                return

            server_idx = int(server_dropdown.value)
            seq = self._resolve_seq = self._resolve_seq + 1
            set_status("Resolving episode streams... \u23f3")
            download_btn.disabled = True
            episodes_grid.controls.clear()
            page.update()

            def do_resolve():
                resolve_error = ""
                try:
                    resolved = self.downloader.resolve_all_m3u8(server_idx, season=1)
                except Exception as ex:
                    resolve_error = str(ex)
                    resolved = []

                if seq != getattr(self, "_resolve_seq", 0):
                    return

                self.episodes_resolved = resolved
                if not resolved:
                    try:
                        server = self.downloader.servers[server_idx]
                        self.episodes_resolved = [{
                            "num": ep["name"],
                            "embed": ep["embed"],
                            "m3u8": None,
                            "error": resolve_error or "Could not resolve stream",
                            "filename": self.downloader.generate_filename(ep["name"], season=1),
                        } for ep in server["list"]]
                    except Exception:
                        pass

                def apply():
                    episodes_grid.controls.clear()
                    available = sum(1 for ep in self.episodes_resolved if ep.get("m3u8"))
                    total = len(self.episodes_resolved)
                    for ep in self.episodes_resolved:
                        available_ep = bool(ep.get("m3u8"))
                        cb = ft.Checkbox(
                            value=available_ep,
                            disabled=not available_ep,
                            data=ep,
                            on_change=lambda _: refresh_concurrency_state(),
                        )
                        suffix = "" if available_ep else " (unavailable)"
                        label = ft.Text(f"EP {ep['num']}{suffix}", selectable=True)
                        episodes_grid.controls.append(ft.Row([cb, label]))
                    download_btn.disabled = available == 0
                    episodes_grid.update()
                    refresh_concurrency_state()
                    if total and available == total:
                        set_status(f"Resolved {available}/{total} episode streams \u2713")
                    elif available:
                        set_status(
                            f"Resolved {available}/{total} episodes; unavailable episodes are listed in Terminal.",
                            ft.Colors.RED,
                        )
                    else:
                        set_status(
                            f"Resolve failed: 0/{total} episodes. See Terminal for details.",
                            ft.Colors.RED,
                        )
                    if total:
                        color = None if available == total else ft.Colors.RED
                        add_terminal_line(
                            f"$ Resolve: {available}/{total} episode streams available",
                            color,
                        )
                        for ep in self.episodes_resolved:
                            if not ep.get("m3u8"):
                                add_terminal_line(
                                    f"! EP {ep['num']}: {ep.get('error') or 'No m3u8 URL'}",
                                    ft.Colors.RED,
                                )
                        page.update()

                ui(apply)

            threading.Thread(target=do_resolve, daemon=True).start()

        def update_filenames():
            if not self.downloader or not self.episodes_resolved:
                return
            for ep in self.episodes_resolved:
                ep["filename"] = self.downloader.generate_filename(ep["num"], season=1)

        def toggle_all(select: bool):
            for c in episodes_grid.controls:
                if isinstance(c, ft.Row) and isinstance(c.controls[0], ft.Checkbox):
                    checkbox = c.controls[0]
                    checkbox.value = select if not checkbox.disabled else False
            refresh_concurrency_state()

        status_lines: dict[str, ft.Text] = {}
        last_progress_tick: dict[str, float] = {}

        def add_terminal_line(text: str, color=None) -> ft.Text:
            line = ft.Text(
                text,
                size=13,
                font_family="monospace",
                selectable=True,
                color=color,
            )
            terminal_view.controls.append(line)
            if len(terminal_view.controls) > 100:
                del terminal_view.controls[: len(terminal_view.controls) - 100]
            return line

        def on_episode_start(ep: dict):
            def apply():
                line = add_terminal_line(f"$ EP {ep['num']}: Starting...", ft.Colors.GREY_400)
                status_lines[ep["num"]] = line
                page.update()
            ui(apply)

        def on_episode_done(ep: dict, success: bool, error: str = ""):
            def apply():
                line = status_lines.get(ep["num"])
                if line is not None:
                    if success:
                        line.value = f"\u2705 EP {ep['num']}: Done"
                        line.color = None
                    else:
                        reason = f" ({error})" if error else ""
                        line.value = f"\u274c EP {ep['num']}: Failed{reason}"
                        line.color = ft.Colors.RED
                if error:
                    add_terminal_line(f"> EP {ep['num']}: {error}", ft.Colors.RED)
                page.update()
            ui(apply)

        def on_progress(ep: dict, line: str):
            now = time.monotonic()
            if now - last_progress_tick.get(ep["num"], 0.0) < 0.3:
                return
            last_progress_tick[ep["num"]] = now

            def apply():
                status_line = status_lines.get(ep["num"])
                if status_line is not None:
                    status_line.value = f"> EP {ep['num']}: {line}"
                    status_line.update()
            ui(apply)

        def start_download():
            if self.downloading:
                return
            self.downloading = True
            download_btn.disabled = True
            download_btn.text = "Downloading..."
            status_lines.clear()
            last_progress_tick.clear()
            terminal_view.controls.clear()
            page.update()

            selected = []
            for c in episodes_grid.controls:
                if (
                    isinstance(c, ft.Row)
                    and isinstance(c.controls[0], ft.Checkbox)
                    and c.controls[0].value
                    and c.controls[0].data.get("m3u8")
                ):
                    selected.append(c.controls[0].data)

            if not selected:
                set_status("No episodes selected")
                self.downloading = False
                download_btn.disabled = False
                download_btn.text = "Download Selected"
                page.update()
                return

            update_filenames()
            output_dir = output_path_field.value.strip() or os.path.expanduser("~/Downloads")
            parallel = int(concurrent_slider.value)
            movie_title = (self.downloader.english_title or self.downloader.title).strip()
            use_subfolder = create_subfolder_cb.value
            folder_name = movie_title if use_subfolder else ""

            add_terminal_line(
                f"$ Episodes: {len(selected)}, Parallel: {parallel}, Output: {output_dir}",
                ft.Colors.GREY_400,
            )
            set_status("Downloading... \u23f3")
            page.update()

            def do_download():
                try:
                    self.downloader.download_multiple(
                        episodes=selected,
                        output_dir=output_dir,
                        folder_name=folder_name,
                        referer=self.downloader.url,
                        parallel=parallel,
                        on_episode_start=on_episode_start,
                        on_episode_done=on_episode_done,
                        on_progress=on_progress,
                    )
                    ui(lambda: set_status("Download complete! \u2713"))
                except Exception as ex:
                    ui(lambda: set_status(f"Error: {ex} \u274c", ft.Colors.RED))
                finally:
                    def restore():
                        self.downloading = False
                        download_btn.disabled = False
                        download_btn.text = "Download Selected"
                        page.update()
                    ui(restore)

            threading.Thread(target=do_download, daemon=True).start()

        page.scroll = ft.ScrollMode.AUTO
        page.add(
            ft.Row([url_field, load_btn, theme_btn], alignment=ft.MainAxisAlignment.SPACE_BETWEEN),
            status_bar,
            ft.Divider(height=8, color=ft.Colors.TRANSPARENT),
            ft.Container(
                content=ft.Column([
                    title_text,
                    ft.Row([subtitle_text, year_field], alignment=ft.MainAxisAlignment.START),
                ]),
                padding=10,
                border=ft.Border.all(1, border_color),
                border_radius=8,
            ),
            ft.Divider(height=10, color=ft.Colors.TRANSPARENT),
            ft.Row([server_dropdown], alignment=ft.MainAxisAlignment.START),
            ft.Divider(height=5, color=ft.Colors.TRANSPARENT),
            ft.Row([select_all_btn, deselect_all_btn], alignment=ft.MainAxisAlignment.START),
            ft.Container(
                content=episodes_grid,
                height=120,
                border=ft.Border.all(1, border_color),
                border_radius=8,
                padding=10,
            ),
            ft.Divider(height=5, color=ft.Colors.TRANSPARENT),
            ft.Row([
                ft.Text("Concurrent Episodes:", size=14, selectable=True),
                concurrent_slider,
                concurrent_label,
            ], alignment=ft.MainAxisAlignment.START),
            ft.Row([create_subfolder_cb], alignment=ft.MainAxisAlignment.START),
            ft.Row([output_path_field, output_picker], alignment=ft.MainAxisAlignment.START),
            ft.Divider(height=5, color=ft.Colors.TRANSPARENT),
            download_btn,
            ft.Divider(height=5, color=ft.Colors.TRANSPARENT),
            ft.Text("Terminal:", weight=ft.FontWeight.BOLD, size=14, selectable=True),
            ft.Container(
                content=terminal_view,
                height=380,
                border=ft.Border.all(1, border_color),
                border_radius=8,
                padding=10,
                bgcolor=ft.Colors.BLACK,
            ),
            ft.Divider(height=10, color=ft.Colors.TRANSPARENT),
            ft.Row([
                ft.TextButton(
                    "github.com/NinhGhoster/NguonC-Downloader",
                    url="https://github.com/NinhGhoster/NguonC-Downloader",
                    style=ft.ButtonStyle(
                        color=ft.Colors.GREY_500,
                        text_style=ft.TextStyle(size=11, italic=True),
                    ),
                ),
            ], alignment=ft.MainAxisAlignment.CENTER),
        )


def main():
    # Patch extracted Flet.app's bundle name so macOS menu bar says "NguonC Downloader"
    meipass = getattr(sys, '_MEIPASS', None)
    if meipass:
        flet_plist = os.path.join(meipass, 'Flet.app', 'Contents', 'Info.plist')
        if os.path.exists(flet_plist):
            try:
                subprocess.run(
                    ['plutil', '-replace', 'CFBundleName', '-string', 'NguonC Downloader', flet_plist],
                    capture_output=True, timeout=5,
                )
                subprocess.run(
                    ['plutil', '-replace', 'CFBundleDisplayName', '-string', 'NguonC Downloader', flet_plist],
                    capture_output=True, timeout=5,
                )
            except Exception:
                pass
    ft.run(NguoncApp().build)


if __name__ == "__main__":
    main()
