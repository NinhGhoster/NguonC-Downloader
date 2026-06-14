import os
import sys
import threading
import re
import subprocess
import flet as ft
from nguonc_downloader import NguoncDownloader


def snack(page: ft.Page, msg: str):
    page.show_dialog(ft.SnackBar(ft.Text(msg)))


class NguoncApp:

    def __init__(self):
        self.downloader: NguoncDownloader | None = None
        self.episodes_resolved: list[dict] = []
        self.downloading = False

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
            content=ft.Text("Ready", size=13),
            padding=ft.Padding(12, 8, 12, 8),
            border_radius=8,
            bgcolor=ft.Colors.with_opacity(0.12, ft.Colors.ON_SURFACE),
        )

        def set_status(msg: str, color=None):
            status_bar.content = ft.Text(msg, size=13, color=color)
            page.update()

        url_field = ft.TextField(
            label="Movie URL",
            hint_text="https://phim.nguonc.com/phim/...",
            prefix_icon=ft.Icons.LINK,
            expand=True,
            on_submit=lambda _: load_movie(),
        )

        load_btn = ft.ElevatedButton(
            "Load Movie",
            icon=ft.Icons.SEARCH,
            on_click=lambda _: load_movie(),
        )

        title_text = ft.Text(size=22, weight=ft.FontWeight.BOLD)
        subtitle_text = ft.Text(size=14, color=ft.Colors.GREY_600)
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
            max=32,
            value=8,
            divisions=31,
            label="{value}",
            width=300,
        )
        concurrent_label = ft.Text("8", size=14)

        def on_concurrent_change(e):
            concurrent_label.value = str(int(concurrent_slider.value))
            concurrent_label.update()

        concurrent_slider.on_change = on_concurrent_change

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
                                output_path_field.value = path
                                output_path_field.update()
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
                                output_path_field.value = path
                                output_path_field.update()
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
                                        output_path_field.value = path
                                        output_path_field.update()
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

        download_btn = ft.ElevatedButton(
            "Download Selected",
            icon=ft.Icons.DOWNLOAD,
            disabled=True,
            style=ft.ButtonStyle(
                color=ft.Colors.WHITE,
                bgcolor=ft.Colors.INDIGO,
            ),
            on_click=lambda _: start_download(),
        )

        log_container = ft.ListView(
            expand=True,
            spacing=2,
            height=380,
        )

        def load_movie():
            url = url_field.value.strip()
            if not url:
                snack(page, "Please enter a URL")
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
            set_status("Loading... \u23f3")
            page.update()

            def do_load():
                try:
                    d = NguoncDownloader(url)
                    info = d.scrape()
                    self.downloader = d
                    title_text.value = info["english_title"] or info["title"]

                    servers = info.get("servers", [])
                    if not servers:
                        raise ValueError("No servers found for this movie")
                    ep_count = len(servers[0].get("list", []))
                    if info["year"]:
                        subtitle_text.value = f"{info['year']}  |  {ep_count} episodes"
                        year_field.value = info["year"]
                    else:
                        subtitle_text.value = f"{ep_count} episodes"

                    server_dropdown.options = [
                        ft.dropdown.Option(str(i), s["server_name"])
                        for i, s in enumerate(servers)
                    ]
                    server_dropdown.value = "0"
                    download_btn.disabled = False

                    update_episodes()
                    set_status(f"Loaded: {info['english_title'] or info['title']} \u2713")
                except Exception as ex:
                    set_status(f"Error: {ex} \u274c", ft.Colors.RED)
                finally:
                    load_btn.disabled = False
                    load_btn.text = "Load Movie"
                    page.update()

            threading.Thread(target=do_load, daemon=True).start()

        def update_episodes():
            if not self.downloader or server_dropdown.value is None:
                return

            server_idx = int(server_dropdown.value)
            try:
                self.episodes_resolved = self.downloader.resolve_all_m3u8(server_idx, season=1)
            except Exception as ex:
                set_status(f"m3u8 resolution failed: {ex}", ft.Colors.RED)
                self.episodes_resolved = []
                try:
                    server = self.downloader.servers[server_idx]
                    for ep in server["list"]:
                        self.episodes_resolved.append({
                            "num": ep["name"],
                            "embed": ep["embed"],
                            "m3u8": None,
                            "filename": self.downloader.generate_filename(ep["name"], season=1),
                        })
                except Exception:
                    pass

            episodes_grid.controls.clear()
            for ep in self.episodes_resolved:
                cb = ft.Checkbox(
                    label=f"EP {ep['num']}",
                    value=True,
                    data=ep,
                )
                episodes_grid.controls.append(cb)
            episodes_grid.update()

        def update_filenames():
            if not self.downloader or not self.episodes_resolved:
                return
            for ep in self.episodes_resolved:
                ep["filename"] = self.downloader.generate_filename(ep["num"], season=1)

        def toggle_all(select: bool):
            for c in episodes_grid.controls:
                if isinstance(c, ft.Checkbox):
                    c.value = select
            episodes_grid.update()

        status_lines = []

        def copy_log(e):
            lines = []
            for c in log_container.controls:
                if isinstance(c, ft.Text) and c.value:
                    lines.append(c.value)
            text = "\n".join(lines)
            try:
                if sys.platform == "darwin":
                    p = subprocess.Popen(["pbcopy"], stdin=subprocess.PIPE)
                    p.communicate(text.encode("utf-8"))
                elif sys.platform == "win32":
                    p = subprocess.Popen(["clip"], stdin=subprocess.PIPE)
                    p.communicate(text.encode("utf-8"))
                else:
                    for cmd in [["xclip", "-selection", "clipboard"],
                                ["xsel", "--clipboard", "--input"]]:
                        try:
                            p = subprocess.Popen(cmd, stdin=subprocess.PIPE)
                            p.communicate(text.encode("utf-8"))
                            break
                        except FileNotFoundError:
                            continue
                snack(page, "Log copied to clipboard")
            except Exception:
                snack(page, "Failed to copy to clipboard")

        copy_btn = ft.TextButton("Copy log", on_click=copy_log)
        clear_btn = ft.TextButton("Clear log", on_click=lambda _: (log_container.controls.clear(), page.update()))

        def log_write(msg: str, color=None, size=12):
            try:
                entry = ft.Text(msg, size=size, font_family="monospace", selectable=True, color=color)
                log_container.controls.append(entry)
                if len(log_container.controls) % 5 == 0:
                    page.update()
            except Exception:
                pass

        def on_episode_start(ep: dict):
            try:
                line = ft.Text(f"EP {ep['num']}: Starting...", size=13, font_family="monospace", selectable=True)
                status_lines.append(line)
                log_container.controls.append(line)
                page.update()
            except Exception:
                pass

        def on_episode_done(ep: dict, success: bool, error: str = ""):
            try:
                for line in status_lines:
                    if line.value and line.value.startswith(f"EP {ep['num']}:"):
                        if success:
                            line.value = f"\u2705 EP {ep['num']}: Done"
                        else:
                            reason = f" ({error})" if error else ""
                            line.value = f"\u274c EP {ep['num']}: Failed{reason}"
                        break
                if error:
                    log_write(f"EP {ep['num']}: {error}", ft.Colors.RED, size=13)
                page.update()
            except Exception:
                pass

        def on_progress(ep: dict, line: str):
            try:
                for status_line in status_lines:
                    if status_line.value and status_line.value.startswith(f"EP {ep['num']}:"):
                        status_line.value = f"EP {ep['num']}: {line}"
                        break
                color = ft.Colors.RED if "ERROR" in line.upper() else None
                log_write(line, color)
            except Exception:
                pass

        def start_download():
            if self.downloading:
                return
            self.downloading = True
            download_btn.disabled = True
            download_btn.text = "Downloading..."
            status_lines.clear()
            log_container.controls.clear()
            page.update()

            selected = []
            for c in episodes_grid.controls:
                if isinstance(c, ft.Checkbox) and c.value:
                    selected.append(c.data)

            if not selected:
                set_status("No episodes selected")
                self.downloading = False
                download_btn.disabled = False
                download_btn.text = "Download Selected"
                page.update()
                return

            update_filenames()
            output_dir = output_path_field.value.strip() or os.path.expanduser("~/Downloads")
            concurrent = int(concurrent_slider.value)
            movie_title = (self.downloader.english_title or self.downloader.title).strip()
            use_subfolder = create_subfolder_cb.value
            folder_name = movie_title if use_subfolder else ""

            log_write(f"Episodes: {len(selected)}, Threads: {concurrent}, Output: {output_dir}", ft.Colors.GREY_400)
            set_status("Downloading... \u23f3")
            page.update()

            def do_download():
                try:
                    self.downloader.download_multiple(
                        episodes=selected,
                        output_dir=output_dir,
                        folder_name=folder_name,
                        referer=self.downloader.url,
                        concurrent=concurrent,
                        on_episode_start=on_episode_start,
                        on_episode_done=on_episode_done,
                        on_progress=on_progress,
                    )
                    set_status("Download complete! \u2713")
                except Exception as ex:
                    set_status(f"Error: {ex} \u274c", ft.Colors.RED)
                finally:
                    self.downloading = False
                    download_btn.disabled = False
                    download_btn.text = "Download Selected"
                    page.update()

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
                ft.Text("Concurrent Fragments:", size=14),
                concurrent_slider,
                concurrent_label,
            ], alignment=ft.MainAxisAlignment.START),
            ft.Row([create_subfolder_cb], alignment=ft.MainAxisAlignment.START),
            ft.Row([output_path_field, output_picker], alignment=ft.MainAxisAlignment.START),
            ft.Divider(height=5, color=ft.Colors.TRANSPARENT),
            download_btn,
            ft.Divider(height=5, color=ft.Colors.TRANSPARENT),
            ft.Text("Log:", weight=ft.FontWeight.BOLD, size=14),
            ft.Container(
                content=log_container,
                height=380,
                border=ft.Border.all(1, border_color),
                border_radius=8,
                padding=10,
            ),
            ft.Row([copy_btn, clear_btn], alignment=ft.MainAxisAlignment.START),
            ft.Divider(height=10, color=ft.Colors.TRANSPARENT),
            ft.Row([
                ft.Text("github.com/NinhGhoster/NguonC-Downloader",
                        size=11, color=ft.Colors.GREY_500, italic=True),
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
