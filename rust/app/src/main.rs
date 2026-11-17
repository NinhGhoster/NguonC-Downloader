//! Native GUI for the Rust engine: egui + eframe (classic `update` API, v0.32).
//! Engine work runs on tokio tasks; results arrive over mpsc queues and are
//! applied at the top of each frame, so UI state is never touched from a
//! worker thread.
use eframe::egui;
use nguonc_core::{download, generate_filename, scrape, Progress};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

const GITHUB_API_LATEST: &str = "https://api.github.com/repos/NinhGhoster/NguonC-Downloader/releases/latest";
const RELEASES_URL: &str = "https://github.com/NinhGhoster/NguonC-Downloader/releases";
/// Build date of this brand (YYYYMMDDHHMM as yyyymmdd). Compared against the
/// YYYY.MM.DD tag of the latest GitHub release for the update check.
const BUILD_DATE: u32 = 20261118;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeMode {
    System,
    Dark,
    Light,
}

impl ThemeMode {
    fn label(self) -> &'static str {
        match self {
            ThemeMode::System => "Theme: System",
            ThemeMode::Dark => "Theme: Dark",
            ThemeMode::Light => "Theme: Light",
        }
    }
    fn next(self) -> Self {
        match self {
            ThemeMode::System => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::System,
        }
    }
    fn preference(self) -> egui::ThemePreference {
        match self {
            ThemeMode::System => egui::ThemePreference::System,
            ThemeMode::Dark => egui::ThemePreference::Dark,
            ThemeMode::Light => egui::ThemePreference::Light,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TermColor {
    Grey,
    Red,
    Green,
    Normal,
}

impl TermColor {
    fn egui(self) -> egui::Color32 {
        match self {
            TermColor::Grey => egui::Color32::from_rgb(150, 150, 150),
            TermColor::Red => egui::Color32::from_rgb(244, 67, 54),
            TermColor::Green => egui::Color32::from_rgb(76, 175, 80),
            TermColor::Normal => egui::Color32::from_rgb(230, 230, 230),
        }
    }
}

struct TermLine {
    text: String,
    color: TermColor,
}

struct EpiRow {
    num: String,
    embed: String,
    checked: bool,
    status: String,
    filename: String,
}

struct ServerRow {
    name: String,
    eps: Vec<EpiRow>,
}

#[derive(Default)]
struct NguoncApp {
    movie_url: String,
    output_dir: String,
    english_title: String,
    vietnamese_title: String,
    year: String,
    loaded_url: String,
    servers: Vec<ServerRow>,
    selected_server: usize,
    concurrency: usize,
    subfolder: bool,
    theme: ThemeMode,
    terminal: Vec<TermLine>,
    ep_line: HashMap<String, usize>,
    ep_progress: HashMap<String, (usize, usize)>,
    status: String,
    status_red: bool,
    loading: bool,
    downloading: bool,
    rx: Option<Receiver<UiMsg>>,
    update_rx: Option<Receiver<UiMsg>>,
    update_info: Option<Result<String, String>>,
    show_update_dialog: bool,
    last_tick: HashMap<String, Instant>,
    handle: Option<tokio::runtime::Handle>,
}

enum UiMsg {
    Scraped { url: String, result: Result<nguonc_core::Movie, String> },
    EpProgress { ep: String, done: usize, total: usize, bytes: u64 },
    EpDone { ep: String, ok: bool, err: String },
    AllDone,
    UpdateCheck(Result<String, String>),
}

impl Default for ThemeMode {
    fn default() -> Self {
        ThemeMode::System
    }
}

impl NguoncApp {
    fn new(handle: tokio::runtime::Handle) -> Self {
        let output_dir = std::env::var("HOME")
            .map(|h| format!("{h}/Downloads"))
            .unwrap_or_else(|_| "downloads".to_string());
        Self {
            handle: Some(handle),
            output_dir,
            subfolder: true,
            theme: ThemeMode::System,
            status: "Ready".to_string(),
            ..Default::default()
        }
    }

    fn set_status(&mut self, msg: impl Into<String>, red: bool) {
        self.status = msg.into();
        self.status_red = red;
    }

    fn push_line(&mut self, text: String, color: TermColor) {
        self.terminal.push(TermLine { text, color });
        while self.terminal.len() > 100 {
            self.terminal.remove(0);
            self.ep_line.retain(|_, i| *i > 0);
            for i in self.ep_line.values_mut() {
                *i -= 1;
            }
        }
    }

    /// In-place update of an episode's status line in the terminal log.
    fn update_ep_line(&mut self, ep: &str, text: String, color: TermColor) {
        if let Some(&i) = self.ep_line.get(ep) {
            if i < self.terminal.len() {
                self.terminal[i] = TermLine { text, color };
                return;
            }
        }
        self.push_line(text, color);
    }

    fn find_ep_mut(&mut self, num: &str) -> Option<&mut EpiRow> {
        self.servers
            .iter_mut()
            .flat_map(|s| s.eps.iter_mut())
            .find(|e| e.num == num)
    }

    fn display_title(&self) -> &str {
        if self.english_title.is_empty() {
            &self.vietnamese_title
        } else {
            &self.english_title
        }
    }

    fn apply_msg(&mut self, msg: UiMsg) {
        match msg {
            UiMsg::Scraped { url, result } => {
                self.loading = false;
                self.rx = None;
                match result {
                    Ok(movie) => {
                        self.english_title = movie.english_title.clone();
                        self.vietnamese_title = movie.title.clone();
                        self.year = movie.year.clone();
                        self.loaded_url = url;
                        self.selected_server = 0;
                        self.servers = movie
                            .servers
                            .iter()
                            .map(|s| ServerRow {
                                name: s.name.clone(),
                                eps: s
                                    .list
                                    .iter()
                                    .map(|ep| EpiRow {
                                        num: ep.name.clone(),
                                        embed: ep.embed.clone(),
                                        checked: false,
                                        status: String::new(),
                                        filename: generate_filename(
                                            &movie.english_title,
                                            &movie.title,
                                            &ep.name,
                                            1,
                                        ),
                                    })
                                    .collect(),
                            })
                            .collect();
                        let name = self.display_title().to_string();
                        self.set_status(format!("Loaded: {name}"), false);
                        let n: usize = self.servers.iter().map(|s| s.eps.len()).sum();
                        self.push_line(
                            format!("$ Loaded {name} ({}) - {n} episodes on {} server(s)", self.year, self.servers.len()),
                            TermColor::Grey,
                        );
                    }
                    Err(e) => {
                        self.set_status(format!("Error: {e}"), true);
                        self.push_line(format!("! Load failed: {e}"), TermColor::Red);
                    }
                }
            }
            UiMsg::EpProgress { ep, done, total, bytes } => {
                self.ep_progress.insert(ep.clone(), (done, total));
                if let Some(row) = self.find_ep_mut(&ep) {
                    row.status = format!("downloading... {done}/{total}");
                }
                let now = Instant::now();
                let due = self
                    .last_tick
                    .get(&ep)
                    .map(|t| now.duration_since(*t) >= Duration::from_millis(300))
                    .unwrap_or(true);
                if due || done == total {
                    self.last_tick.insert(ep.clone(), now);
                    self.update_ep_line(
                        &ep,
                        format!("> EP {ep}: {done}/{total} segments ({:.0} MiB)", bytes as f64 / 1048576.0),
                        TermColor::Normal,
                    );
                }
            }
            UiMsg::EpDone { ep, ok, err } => {
                if let Some(row) = self.find_ep_mut(&ep) {
                    row.status = if ok { "done".into() } else { format!("fail: {err}") };
                }
                if ok {
                    self.update_ep_line(&ep, format!("EP {ep}: Done"), TermColor::Green);
                } else {
                    self.update_ep_line(&ep, format!("EP {ep}: Failed"), TermColor::Red);
                    self.push_line(format!("> EP {ep}: {err}"), TermColor::Red);
                }
            }
            UiMsg::AllDone => {
                self.downloading = false;
                self.rx = None;
                self.ep_progress.clear();
                self.set_status("Download complete!", false);
                self.push_line("$ All downloads finished".to_string(), TermColor::Grey);
            }
            UiMsg::UpdateCheck(result) => {
                self.update_rx = None;
                match &result {
                    Ok(msg) => self.set_status(msg.clone(), false),
                    Err(e) => self.set_status(format!("Update check failed: {e}"), true),
                }
                self.update_info = Some(result);
                self.show_update_dialog = true;
            }
        }
    }

    fn start_download(&mut self) {
        let Some(server) = self.servers.get_mut(self.selected_server) else { return };
        let jobs: Vec<(String, String, String)> = server
            .eps
            .iter_mut()
            .filter(|e| e.checked && e.status.is_empty())
            .map(|e| {
                e.status = "resolving...".to_string();
                (e.num.clone(), e.embed.clone(), e.filename.clone())
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let Some(handle) = self.handle.clone() else { return };
        self.terminal.clear();
        self.ep_line.clear();
        self.ep_progress.clear();
        self.set_status("Downloading...", false);
        self.rx = Some(rx);
        self.downloading = true;
        let parallel = self.concurrency.max(1);
        let movie_url = self.loaded_url.clone();
        let mut out_dir = self.output_dir.trim().to_string();
        if out_dir.is_empty() {
            out_dir = std::env::var("HOME").map(|h| format!("{h}/Downloads")).unwrap_or_else(|_| "downloads".to_string());
        }
        if self.subfolder {
            let base = self.display_title();
            let safe: String = base
                .chars()
                .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
                .collect();
            let safe = safe.trim();
            if !safe.is_empty() {
                out_dir = format!("{out_dir}/{safe}");
            }
        }
        self.push_line(
            format!("$ Episodes: {}, Parallel: {parallel}, Output: {out_dir}", jobs.len()),
            TermColor::Grey,
        );
        for (num, _, _) in &jobs {
            let idx = self.terminal.len();
            self.push_line(format!("$ EP {num}: Starting..."), TermColor::Grey);
            self.ep_line.insert(num.clone(), idx);
        }
        let remaining = Arc::new(AtomicUsize::new(jobs.len()));

        for (num, embed, filename) in jobs {
            let tx = tx.clone();
            let movie_url = movie_url.clone();
            let out_dir = out_dir.clone();
            let remaining = remaining.clone();
            handle.spawn(async move {
                let ep = nguonc_core::Episode { name: num.clone(), slug: None, embed };
                let resolved = nguonc_core::resolve::resolve_one(&ep, &movie_url, 1).await;
                if let Some(err) = resolved.error {
                    let _ = tx.send(UiMsg::EpDone { ep: num, ok: false, err });
                } else {
                    let m3u8 = resolved.m3u8.unwrap_or_default();
                    let referer = ep.embed.clone();
                    let out = std::path::Path::new(&out_dir)
                        .join(&filename)
                        .to_string_lossy()
                        .into_owned();
                    let tx2 = tx.clone();
                    let ep2 = num.clone();
                    let res = download(&m3u8, &out, &referer, parallel, move |p: Progress| {
                        let _ = tx2.send(UiMsg::EpProgress {
                            ep: ep2.clone(),
                            done: p.done,
                            total: p.total,
                            bytes: p.bytes,
                        });
                    })
                    .await;
                    let _ = std::fs::remove_file(&m3u8);
                    match res {
                        Ok(()) => {
                            let _ = tx.send(UiMsg::EpDone { ep: num, ok: true, err: String::new() });
                        }
                        Err(e) => {
                            let _ = tx.send(UiMsg::EpDone { ep: num, ok: false, err: e.to_string() });
                        }
                    }
                }
                if remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
                    let _ = tx.send(UiMsg::AllDone);
                }
            });
        }
    }

    fn check_update(&mut self) {
        if self.update_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.update_rx = Some(rx);
        self.show_update_dialog = true;
        self.set_status("Checking for updates...", false);
        let Some(handle) = self.handle.clone() else { return };
        handle.spawn(async move {
            let result = async {
                let body = nguonc_core::http::get(GITHUB_API_LATEST, None)
                    .await
                    .map_err(|e| e.to_string())?;
                let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
                let tag = v["tag_name"].as_str().unwrap_or("").trim().to_string();
                if tag.is_empty() {
                    return Err("no tag_name in release payload".to_string());
                }
                let tag_num: u32 = tag.chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0);
                if tag_num > BUILD_DATE {
                    Ok(format!("Update available: {tag}"))
                } else {
                    Ok(format!("You are up to date (latest: {tag})"))
                }
            }
            .await;
            let _ = tx.send(UiMsg::UpdateCheck(result));
        });
    }

    /// Native "choose folder" dialog, shelled out per platform (same approach
    /// as the Python app) to avoid GUI-library dependencies.
    fn pick_folder(&mut self) {
        let picked: Option<String> = (|| {
            let (cmd, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
                (
                    "osascript",
                    vec![
                        "-e",
                        "set theFolder to choose folder with prompt \"Select download directory\"",
                        "-e",
                        "set thePath to POSIX path of theFolder",
                        "-e",
                        "return thePath",
                    ],
                )
            } else if cfg!(target_os = "windows") {
                (
                    "powershell",
                    vec![
                        "-NoProfile",
                        "-Command",
                        "Add-Type -AssemblyName System.Windows.Forms; $f = New-Object System.Windows.Forms.FolderBrowserDialog; $f.Description = \"Select download directory\"; $f.ShowDialog() | Out-Null; if ($f.SelectedPath) { Write-Output $f.SelectedPath }",
                    ],
                )
            } else {
                for cmd in ["zenity --file-selection --directory", "kdialog --getexistingdirectory"] {
                    let mut parts = cmd.split_whitespace();
                    let program = parts.next().unwrap_or_default();
                    let args: Vec<&str> = parts.collect();
                    if let Ok(out) = std::process::Command::new(program).args(&args).output() {
                        if out.status.success() {
                            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                            if !path.is_empty() {
                                return Some(path);
                            }
                        }
                    }
                }
                return None;
            };
            let out = std::process::Command::new(cmd).args(&args).output().ok()?;
            if !out.status.success() {
                return None;
            }
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if path.is_empty() {
                None
            } else {
                Some(path)
            }
        })();
        match picked {
            Some(path) => self.output_dir = path,
            None => self.set_status("No folder selected".to_string(), false),
        }
    }
}

impl eframe::App for NguoncApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain inbound async messages first so no task touches UI state.
        let msgs: Vec<UiMsg> = self
            .rx
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for msg in msgs {
            self.apply_msg(msg);
        }
        let umsgs: Vec<UiMsg> = self
            .update_rx
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for msg in umsgs {
            self.apply_msg(msg);
        }
        if self.loading || self.downloading {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(14, 10)))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 10.0);

                // Header: brand + theme cycle + update check.
                ui.horizontal(|ui| {
                    ui.heading("NguonC Downloader");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Check for Update").clicked() {
                            self.check_update();
                        }
                        if ui.button(self.theme.label()).clicked() {
                            self.theme = self.theme.next();
                            ctx.set_theme(self.theme.preference());
                        }
                    });
                });
                ui.separator();

                // URL row.
                ui.horizontal(|ui| {
                    ui.label("Movie URL:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.movie_url)
                            .hint_text("https://phim.nguonc.com/phim/...")
                            .desired_width(f32::INFINITY),
                    );
                    if ui
                        .add_enabled(!self.loading && !self.downloading, egui::Button::new("Load Movie"))
                        .clicked()
                    {
                        let url = self.movie_url.trim().to_string();
                        if !url.is_empty() {
                            self.set_status("Loading...", false);
                            self.loading = true;
                            self.servers.clear();
                            self.english_title.clear();
                            self.vietnamese_title.clear();
                            self.terminal.clear();
                            self.ep_line.clear();
                            self.ep_progress.clear();
                            let (tx, rx) = mpsc::channel();
                            self.rx = Some(rx);
                            match self.handle.clone() {
                                Some(handle) => {
                                    handle.spawn(async move {
                                        let result = scrape(&url).await.map_err(|e| e.to_string());
                                        let _ = tx.send(UiMsg::Scraped { url, result });
                                    });
                                }
                                None => self.loading = false,
                            }
                        }
                    }
                    if self.loading {
                        ui.spinner();
                    }
                });

                // Status bar.
                let sc = if self.status_red {
                    egui::Color32::from_rgb(244, 67, 54)
                } else {
                    egui::Color32::from_rgb(76, 175, 80)
                };
                egui::Frame::group(ui.style())
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .corner_radius(6.0)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(egui::RichText::new(self.status.clone()).color(sc).size(13.0)));
                    });

                if self.english_title.is_empty() && self.vietnamese_title.is_empty() {
                    ui.label("Enter a movie page URL and press Load Movie.");
                    return;
                }

                // Title block.
                ui.label(egui::RichText::new(self.display_title().to_string()).size(20.0).strong());
                let ep_count = self.servers.first().map(|s| s.eps.len()).unwrap_or(0);
                ui.label(egui::RichText::new(format!("{}  |  {} episodes", self.year, ep_count)).color(egui::Color32::GRAY));

                // Server picker.
                ui.horizontal(|ui| {
                    ui.label("Server / Source:");
                    egui::ComboBox::from_id_salt("server")
                        .selected_text(
                            self.servers
                                .get(self.selected_server)
                                .map(|s| s.name.as_str())
                                .unwrap_or(""),
                        )
                        .show_ui(ui, |ui| {
                            for (i, s) in self.servers.iter().enumerate() {
                                ui.selectable_value(&mut self.selected_server, i, &s.name);
                            }
                        });
                });

                ui.horizontal(|ui| {
                    if ui.button("Select All").clicked() {
                        if let Some(server) = self.servers.get_mut(self.selected_server) {
                            for e in &mut server.eps {
                                e.checked = true;
                            }
                        }
                    }
                    if ui.button("Deselect All").clicked() {
                        if let Some(server) = self.servers.get_mut(self.selected_server) {
                            for e in &mut server.eps {
                                e.checked = false;
                            }
                        }
                    }
                });

                // Episode grid.
                let nchecked: usize;
                {
                    let Some(server) = self.servers.get_mut(self.selected_server) else {
                        return;
                    };
                    nchecked = server.eps.iter().filter(|e| e.checked).count();
                    if nchecked < 2 && self.concurrency > 1 {
                        self.concurrency = 1;
                    }
                    egui::ScrollArea::vertical()
                        .max_height(170.0)
                        .show(ui, |ui| {
                            egui::Grid::new("episodes")
                                .num_columns(4)
                                .spacing([14.0, 8.0])
                                .show(ui, |ui| {
                                    for chunk in server.eps.chunks_mut(4) {
                                        for ep in chunk {
                                            ui.horizontal(|ui| {
                                                let mut checked = ep.checked;
                                                if ui.checkbox(&mut checked, "").changed() {
                                                    ep.checked = checked;
                                                }
                                                let color = if ep.status.starts_with("fail") {
                                                    egui::Color32::from_rgb(244, 67, 54)
                                                } else if ep.status.starts_with("downloading")
                                                    || ep.status.starts_with("resolving")
                                                {
                                                    egui::Color32::from_rgb(200, 150, 0)
                                                } else if ep.status == "done" {
                                                    egui::Color32::from_rgb(76, 175, 80)
                                                } else {
                                                    ui.visuals().text_color()
                                                };
                                                let label = if ep.status.is_empty() {
                                                    format!("EP {}", ep.num)
                                                } else {
                                                    format!("EP {} · {}", ep.num, ep.status)
                                                };
                                                ui.label(egui::RichText::new(label).color(color));
                                            });
                                        }
                                        ui.end_row();
                                    }
                                });
                        });
                }

                // Concurrency + subfolder + output dir.
                ui.horizontal(|ui| {
                    ui.label("Concurrent Episodes:");
                    ui.add_enabled(
                        nchecked >= 2,
                        egui::Slider::new(&mut self.concurrency, 1..=8),
                    );
                    if nchecked >= 2 {
                        ui.label(format!("{nchecked} checked"));
                    } else {
                        ui.label("disabled (<2 checked)");
                    }
                });

                ui.checkbox(&mut self.subfolder, "Create movie name subfolder");

                ui.horizontal(|ui| {
                    ui.label("Output dir:");
                    ui.add(egui::TextEdit::singleline(&mut self.output_dir).desired_width(f32::INFINITY));
                    if ui.button("Browse...").clicked() {
                        self.pick_folder();
                    }
                });

                // Download button.
                let can_download = !self.loading && !self.downloading && nchecked > 0;
                let label = if self.downloading { "Downloading..." } else { "Download Selected" };
                let btn = egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE))
                    .fill(egui::Color32::from_rgb(63, 81, 181))
                    .min_size(egui::vec2(0.0, 34.0));
                if ui.add_enabled(can_download, btn).clicked() {
                    self.start_download();
                }

                // Overall progress bar while downloading.
                if self.downloading {
                    let (done, total): (usize, usize) = self
                        .ep_progress
                        .values()
                        .fold((0, 0), |(d, t), &(dd, tt)| (d + dd, t + tt));
                    let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                    ui.add(egui::ProgressBar::new(frac).text(format!("{done}/{total} segments")));
                }

                ui.separator();
                ui.label(egui::RichText::new("Terminal:").strong());
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgb(16, 16, 16))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(150.0)
                            .stick_to_bottom(true)
                            .id_salt("terminal")
                            .show(ui, |ui| {
                                for line in &self.terminal {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(line.text.clone())
                                                .monospace()
                                                .size(12.0)
                                                .color(line.color.egui()),
                                        )
                                        .selectable(true),
                                    );
                                }
                            });
                    });

                // Footer.
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                        ui.hyperlink_to("github.com/NinhGhoster/NguonC-Downloader", RELEASES_URL);
                    });
                });
            });

        // Update-check dialog.
        if self.show_update_dialog {
            let mut open = true;
            let mut close_clicked = false;
            egui::Window::new("Check for Update")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    match &self.update_info {
                        Some(Ok(msg)) => {
                            ui.label(msg.clone());
                        }
                        Some(Err(e)) => {
                            ui.colored_label(egui::Color32::RED, format!("Update check failed: {e}"));
                        }
                        None => {
                            ui.spinner();
                            ui.label("Checking GitHub releases...");
                        }
                    }
                    ui.separator();
                    ui.hyperlink_to(RELEASES_URL, "Releases page");
                    if ui.button("Close").clicked() {
                        close_clicked = true;
                    }
                });
            self.show_update_dialog = open && !close_clicked;
        }
    }
}

fn main() -> eframe::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let handle = rt.handle().clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([860.0, 720.0])
            .with_min_inner_size([700.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "NguonC Downloader",
        options,
        Box::new(|cc| {
            // Be Vietnam Pro covers the Vietnamese diacritics egui's default
            // fonts lack; default families stay as fallback.
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "bevn".to_owned(),
                std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                    "../assets/BeVietnamPro-Regular.ttf"
                ))),
            );
            if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                f.insert(0, "bevn".to_owned());
            }
            if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                f.insert(0, "bevn".to_owned());
            }
            cc.egui_ctx.set_fonts(fonts);
            Ok(Box::new(NguoncApp::new(handle)))
        }),
    )
}
