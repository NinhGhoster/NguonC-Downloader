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
        page.title = "Nguồn Downloader"
        page.theme_mode = ft.ThemeMode.LIGHT
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
            hint_text="2021",
            on_change=lambda _: update_filenames(),
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

        progress_container = ft.ListView(
            expand=True,
            spacing=4,
            height=200,
        )

        status_bar = ft.Text(size=12, color=ft.Colors.GREY_500)

        def load_movie():
            url = url_field.value.strip()
            if not url:
                snack(page, "Please enter a URL")
                return

            load_btn.disabled = True
            load_btn.text = "Loading..."
            page.update()

            def do_load():
                try:
                    d = NguoncDownloader(url)
                    info = d.scrape()
                    self.downloader = d

                    title_text.value = info["english_title"] or info["title"]
                    if info["year"]:
                        subtitle_text.value = f"{info['year']}  |  {len(info['servers'][0]['list'])} episodes"
                        year_field.value = info["year"]
                    else:
                        subtitle_text.value = f"{len(info['servers'][0]['list'])} episodes"

                    server_dropdown.options = [
                        ft.dropdown.Option(str(i), s["server_name"])
                        for i, s in enumerate(info["servers"])
                    ]
                    server_dropdown.value = "0"
                    download_btn.disabled = False

                    update_episodes()
                    snack(page, "Movie loaded successfully!")
                except Exception as ex:
                    snack(page, f"Error: {ex}")
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
                self.episodes_resolved = self.downloader.resolve_all_m3u8(server_idx)
            except Exception:
                server = self.downloader.servers[server_idx]
                self.episodes_resolved = []
                for ep in server["list"]:
                    self.episodes_resolved.append({
                        "num": ep["name"],
                        "embed": ep["embed"],
                        "m3u8": None,
                        "filename": self.downloader.generate_filename(ep["name"]),
                    })

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
            year = year_field.value.strip()
            for ep in self.episodes_resolved:
                name = self.downloader.english_title or self.downloader.title
                safe_name = re.sub(r'[\\/*?:"<>|]', "", name).strip()
                if year:
                    ep["filename"] = f"{safe_name} ({year}) EP {ep['num']}.mp4"
                else:
                    ep["filename"] = f"{safe_name} EP {ep['num']}.mp4"

        def toggle_all(select: bool):
            for c in episodes_grid.controls:
                if isinstance(c, ft.Checkbox):
                    c.value = select
            episodes_grid.update()

        status_lines = []

        def on_episode_start(ep: dict):
            line = ft.Text(f"EP {ep['num']}: Starting...", size=13)
            status_lines.append(line)
            progress_container.controls.append(line)
            page.update()

        def on_episode_done(ep: dict, success: bool):
            for line in status_lines:
                if line.value.startswith(f"EP {ep['num']}:"):
                    icon = "\u2705" if success else "\u274c"
                    line.value = f"{icon} EP {ep['num']}: {'Done' if success else 'Failed'}"
                    break
            page.update()

        def on_progress(ep: dict, line: str):
            for status_line in status_lines:
                if status_line.value.startswith(f"EP {ep['num']}:"):
                    if "Starting" in status_line.value or "Done" in status_line.value or "Failed" in status_line.value:
                        pass
                    pct_match = re.search(r'([\d.]+)%', line)
                    if pct_match:
                        status_line.value = f"EP {ep['num']}: {pct_match.group(1)}%"
                    break
            page.update()

        def start_download():
            if self.downloading:
                return
            self.downloading = True
            download_btn.disabled = True
            download_btn.text = "Downloading..."
            status_lines.clear()
            progress_container.controls.clear()
            page.update()

            selected = []
            for c in episodes_grid.controls:
                if isinstance(c, ft.Checkbox) and c.value:
                    selected.append(c.data)

            if not selected:
                snack(page, "No episodes selected")
                self.downloading = False
                download_btn.disabled = False
                download_btn.text = "Download Selected"
                page.update()
                return

            update_filenames()
            output_dir = output_path_field.value.strip() or os.path.expanduser("~/Downloads")
            concurrent = int(concurrent_slider.value)
            server_name = server_dropdown.options[int(server_dropdown.value)].text if server_dropdown.value else "Unknown"

            def do_download():
                try:
                    self.downloader.download_multiple(
                        episodes=selected,
                        output_dir=output_dir,
                        server_name=server_name,
                        referer=self.downloader.url,
                        concurrent=concurrent,
                        on_episode_start=on_episode_start,
                        on_episode_done=on_episode_done,
                        on_progress=on_progress,
                    )
                    status_bar.value = "Download complete!"
                except Exception as ex:
                    status_bar.value = f"Error: {ex}"
                finally:
                    self.downloading = False
                    download_btn.disabled = False
                    download_btn.text = "Download Selected"
                    page.update()

            threading.Thread(target=do_download, daemon=True).start()

        page.add(
            ft.Row([url_field, load_btn], alignment=ft.MainAxisAlignment.SPACE_BETWEEN),
            ft.Divider(height=10, color=ft.Colors.TRANSPARENT),
            ft.Container(
                content=ft.Column([
                    title_text,
                    ft.Row([subtitle_text, year_field], alignment=ft.MainAxisAlignment.START),
                ]),
                padding=10,
                border=ft.Border.all(1, ft.Colors.GREY_300),
                border_radius=8,
            ),
            ft.Divider(height=10, color=ft.Colors.TRANSPARENT),
            ft.Row([server_dropdown], alignment=ft.MainAxisAlignment.START),
            ft.Divider(height=5, color=ft.Colors.TRANSPARENT),
            ft.Row([select_all_btn, deselect_all_btn], alignment=ft.MainAxisAlignment.START),
            ft.Container(
                content=episodes_grid,
                height=120,
                border=ft.Border.all(1, ft.Colors.GREY_300),
                border_radius=8,
                padding=10,
            ),
            ft.Divider(height=5, color=ft.Colors.TRANSPARENT),
            ft.Row([
                ft.Text("Concurrent Fragments:", size=14),
                concurrent_slider,
                concurrent_label,
            ], alignment=ft.MainAxisAlignment.START),
            ft.Row([output_path_field, output_picker], alignment=ft.MainAxisAlignment.START),
            ft.Divider(height=5, color=ft.Colors.TRANSPARENT),
            download_btn,
            ft.Divider(height=5, color=ft.Colors.TRANSPARENT),
            ft.Text("Progress:", weight=ft.FontWeight.BOLD, size=14),
            ft.Container(
                content=progress_container,
                height=180,
                border=ft.Border.all(1, ft.Colors.GREY_300),
                border_radius=8,
                padding=10,
            ),
            status_bar,
        )


def main():
    ft.run(NguoncApp().build)


if __name__ == "__main__":
    main()
