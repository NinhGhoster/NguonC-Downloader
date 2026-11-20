//! Native GUI for the Rust engine: egui + eframe (classic `update` API, v0.32).
//! Engine work runs on tokio tasks; results arrive over mpsc queues and are
//! applied at the top of each frame, so UI state is never touched from a
//! worker thread.
use eframe::egui;
use egui::load::SizedTexture;
use nguonc_core::{Progress, download, generate_filename, scrape};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const GITHUB_API_LATEST: &str =
    "https://api.github.com/repos/NinhGhoster/NguonC-Downloader/releases/latest";
const RELEASES_URL: &str = "https://github.com/NinhGhoster/NguonC-Downloader/releases";
/// Build date of this brand (YYYYMMDDHHMM as yyyymmdd). Compared against the
/// YYYY.MM.DD tag of the latest GitHub release for the update check.
const BUILD_DATE: u32 = 20261118;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum ThemeMode {
    #[default]
    System,
    Dark,
    Light,
}

impl ThemeMode {
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
    fn egui_for(self, dark: bool) -> egui::Color32 {
        match self {
            TermColor::Grey => {
                if dark {
                    egui::Color32::from_rgb(130, 136, 145)
                } else {
                    egui::Color32::from_rgb(100, 106, 115)
                }
            }
            TermColor::Red => egui::Color32::from_rgb(235, 82, 78),
            TermColor::Green => egui::Color32::from_rgb(46, 170, 96),
            TermColor::Normal => {
                if dark {
                    egui::Color32::from_rgb(222, 226, 232)
                } else {
                    egui::Color32::from_rgb(38, 42, 48)
                }
            }
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
    logo: Option<egui::TextureHandle>,
}

enum UiMsg {
    Scraped {
        url: String,
        result: Result<nguonc_core::Movie, String>,
    },
    EpProgress {
        ep: String,
        done: usize,
        total: usize,
        bytes: u64,
    },
    EpDone {
        ep: String,
        ok: bool,
        err: String,
    },
    AllDone,
    UpdateCheck(Result<String, String>),
}

impl NguoncApp {
    fn new(handle: tokio::runtime::Handle, logo: Option<egui::TextureHandle>) -> Self {
        let output_dir = std::env::var("HOME")
            .map(|h| format!("{h}/Downloads"))
            .unwrap_or_else(|_| "downloads".to_string());
        Self {
            handle: Some(handle),
            output_dir,
            subfolder: true,
            theme: ThemeMode::System,
            status: "Ready".to_string(),
            logo,
            ..Default::default()
        }
    }

    fn set_status(&mut self, msg: impl Into<String>, red: bool) {
        self.status = msg.into();
        self.status_red = red;
    }

    /// Kick off a scrape of the current URL, if one is present and not busy.
    fn begin_load(&mut self) {
        let url = self.movie_url.trim().to_string();
        if url.is_empty() || self.loading || self.downloading {
            return;
        }
        self.set_status("Loading…", false);
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
        if let Some(&i) = self.ep_line.get(ep)
            && i < self.terminal.len()
        {
            self.terminal[i] = TermLine { text, color };
            return;
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
                            format!(
                                "$ Loaded {name} ({}) - {n} episodes on {} server(s)",
                                self.year,
                                self.servers.len()
                            ),
                            TermColor::Grey,
                        );
                    }
                    Err(e) => {
                        self.set_status(format!("Error: {e}"), true);
                        self.push_line(format!("! Load failed: {e}"), TermColor::Red);
                    }
                }
            }
            UiMsg::EpProgress {
                ep,
                done,
                total,
                bytes,
            } => {
                self.ep_progress.insert(ep.clone(), (done, total));
                if let Some(row) = self.find_ep_mut(&ep) {
                    row.status = format!("downloading… {done}/{total}");
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
                        format!(
                            "> EP {ep}: {done}/{total} segments ({:.0} MiB)",
                            bytes as f64 / 1048576.0
                        ),
                        TermColor::Normal,
                    );
                }
            }
            UiMsg::EpDone { ep, ok, err } => {
                if let Some(row) = self.find_ep_mut(&ep) {
                    row.status = if ok {
                        "done".into()
                    } else {
                        format!("fail: {err}")
                    };
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
        let Some(server) = self.servers.get_mut(self.selected_server) else {
            return;
        };
        let jobs: Vec<(String, String, String)> = server
            .eps
            .iter_mut()
            .filter(|e| e.checked && e.status.is_empty())
            .map(|e| {
                e.status = "resolving…".to_string();
                (e.num.clone(), e.embed.clone(), e.filename.clone())
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let Some(handle) = self.handle.clone() else {
            return;
        };
        self.terminal.clear();
        self.ep_line.clear();
        self.ep_progress.clear();
        self.set_status("Downloading…", false);
        self.rx = Some(rx);
        self.downloading = true;
        let parallel = self.concurrency.max(1);
        let movie_url = self.loaded_url.clone();
        let mut out_dir = self.output_dir.trim().to_string();
        if out_dir.is_empty() {
            out_dir = std::env::var("HOME")
                .map(|h| format!("{h}/Downloads"))
                .unwrap_or_else(|_| "downloads".to_string());
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
            format!(
                "$ Episodes: {}, Parallel: {parallel}, Output: {out_dir}",
                jobs.len()
            ),
            TermColor::Grey,
        );
        for (num, _, _) in &jobs {
            let idx = self.terminal.len();
            self.push_line(format!("$ EP {num}: Starting…"), TermColor::Grey);
            self.ep_line.insert(num.clone(), idx);
        }
        let remaining = Arc::new(AtomicUsize::new(jobs.len()));

        for (num, embed, filename) in jobs {
            let tx = tx.clone();
            let movie_url = movie_url.clone();
            let out_dir = out_dir.clone();
            let remaining = remaining.clone();
            handle.spawn(async move {
                let ep = nguonc_core::Episode {
                    name: num.clone(),
                    slug: None,
                    embed,
                };
                let resolved = nguonc_core::resolve::resolve_one(&ep, &movie_url, 1).await;
                if let Some(err) = resolved.error {
                    let _ = tx.send(UiMsg::EpDone {
                        ep: num,
                        ok: false,
                        err,
                    });
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
                            let _ = tx.send(UiMsg::EpDone {
                                ep: num,
                                ok: true,
                                err: String::new(),
                            });
                        }
                        Err(e) => {
                            let _ = tx.send(UiMsg::EpDone {
                                ep: num,
                                ok: false,
                                err: e.to_string(),
                            });
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
        self.set_status("Checking for updates…", false);
        let Some(handle) = self.handle.clone() else {
            return;
        };
        handle.spawn(async move {
            let result = async {
                let body = nguonc_core::http::get(GITHUB_API_LATEST, None)
                    .await
                    .map_err(|e| e.to_string())?;
                let v: serde_json::Value =
                    serde_json::from_str(&body).map_err(|e| e.to_string())?;
                let tag = v["tag_name"].as_str().unwrap_or("").trim().to_string();
                if tag.is_empty() {
                    return Err("no tag_name in release payload".to_string());
                }
                let tag_num: u32 = tag
                    .chars()
                    .filter(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0);
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
                for cmd in [
                    "zenity --file-selection --directory",
                    "kdialog --getexistingdirectory",
                ] {
                    let mut parts = cmd.split_whitespace();
                    let program = parts.next().unwrap_or_default();
                    let args: Vec<&str> = parts.collect();
                    if let Ok(out) = std::process::Command::new(program).args(&args).output()
                        && out.status.success()
                    {
                        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                        if !path.is_empty() {
                            return Some(path);
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
            if path.is_empty() { None } else { Some(path) }
        })();
        match picked {
            Some(path) => self.output_dir = path,
            None => self.set_status("No folder selected".to_string(), false),
        }
    }
}

impl eframe::App for NguoncApp {
    /// Match the window's clear color to the active theme so the whole window
    /// (including any gap around the panel) switches between dark and light.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

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

        // Accent colors for primary actions and status.
        let accent = egui::Color32::from_rgb(79, 110, 247);
        let green = egui::Color32::from_rgb(46, 170, 96);
        let red = egui::Color32::from_rgb(225, 75, 70);
        let amber = egui::Color32::from_rgb(200, 145, 30);
        let muted = ctx.style().visuals.weak_text_color();
        let dark = ctx.theme() == egui::Theme::Dark;
        let term_bg = if dark {
            egui::Color32::from_rgb(18, 20, 24)
        } else {
            egui::Color32::from_rgb(245, 246, 248)
        };

        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(20, 16)),
            )
            .show(ctx, |ui| {
                // The whole column scrolls vertically: content is taller than the
                // window at small sizes, and widgets must never be clipped away.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(12.0, 10.0);

                        // ── Header ────────────────────────────────────────────────
                        ui.horizontal(|ui| {
                            if let Some(tex) = &self.logo {
                                ui.add(
                                    egui::Image::from_texture(SizedTexture::from_handle(tex))
                                        .fit_to_exact_size(egui::vec2(34.0, 34.0)),
                                );
                            }
                            ui.vertical(|ui| {
                                ui.heading(
                                    egui::RichText::new("NguonC Downloader").size(20.0).strong(),
                                );
                                ui.hyperlink_to(
                                    egui::RichText::new("github.com/NinhGhoster/NguonC-Downloader")
                                        .size(12.0)
                                        .color(muted),
                                    "https://github.com/NinhGhoster/NguonC-Downloader",
                                );
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    // Theme button shows the active mode; click cycles System / Dark / Light.
                                    let theme_label = match self.theme {
                                        ThemeMode::System => "System",
                                        ThemeMode::Dark => "Dark",
                                        ThemeMode::Light => "Light",
                                    };
                                    if ui
                                        .add_sized(
                                            [130.0, 30.0],
                                            egui::Button::new(
                                                egui::RichText::new(format!("Theme: {theme_label}"))
                                                    .size(14.0),
                                            ),
                                        )
                                        .on_hover_text("Click to cycle: System / Dark / Light")
                                        .clicked()
                                    {
                                        self.theme = self.theme.next();
                                        ctx.set_theme(self.theme.preference());
                                        ctx.request_repaint();
                                    }
                                    if ui
                                        .add_sized([130.0, 30.0], egui::Button::new("Check for Update"))
                                        .clicked()
                                    {
                                        self.check_update();
                                    }
                                },
                            );
                        });
                        ui.add_space(2.0);
                        ui.separator();

                        // ── Movie URL ─────────────────────────────────────────────
                        ui.label(
                            egui::RichText::new("Movie URL")
                                .size(12.0)
                                .color(muted)
                                .strong(),
                        );
                        // Finite edit width: an `INFINITY` desired width makes the row's
                        // content wider than the panel (which then grows/clips), and an
                        // RTL layout places the edit's far edge off-panel.
                        ui.horizontal(|ui| {
                            let btn_w = 120.0;
                            let edit_w =
                                (ui.available_width() - btn_w - ui.spacing().item_spacing.x)
                                    .max(100.0);
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.movie_url)
                                    .hint_text("https://phim.nguonc.com/phim/...")
                                    .desired_width(edit_w),
                            );
                            if self.loading {
                                ui.spinner();
                            }
                            if ui
                                .add_enabled(
                                    !self.loading && !self.downloading,
                                    egui::Button::new(egui::RichText::new("Load Movie").strong())
                                        .min_size(egui::vec2(120.0, 32.0)),
                                )
                                .clicked()
                                || (resp.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                            {
                                self.begin_load();
                            }
                        });

                        // ── Status line ───────────────────────────────────────────
                        let sc = if self.status_red { red } else { green };
                        ui.horizontal(|ui| {
                            // Painted dot: guaranteed to render regardless of font coverage.
                            let (rect, _resp) = ui
                                .allocate_exact_size(egui::vec2(12.0, 16.0), egui::Sense::hover());
                            ui.painter().circle_filled(rect.center(), 4.5, sc);
                            ui.add(egui::Label::new(
                                egui::RichText::new(&self.status).size(13.0).color(sc),
                            ));
                        });

                        if self.english_title.is_empty() && self.vietnamese_title.is_empty() {
                            ui.add_space(20.0);
                            ui.vertical_centered(|ui| {
                                ui.add_space(30.0);
                                if let Some(tex) = &self.logo {
                                    ui.add(
                                        egui::Image::from_texture(SizedTexture::from_handle(tex))
                                            .fit_to_exact_size(egui::vec2(100.0, 100.0)),
                                    );
                                    ui.add_space(20.0);
                                }
                                ui.heading(
                                    egui::RichText::new("Paste a movie URL above").size(19.0),
                                );
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new("Press Load Movie (or Enter) to fetch the episode list")
                                        .size(14.0)
                                        .color(muted),
                                );
                            });
                            return;
                        }

                        ui.add_space(4.0);

                        // ── Movie info card ───────────────────────────────────────
                        egui::Frame::group(ui.style())
                            .inner_margin(egui::Margin::symmetric(14, 12))
                            .corner_radius(10.0)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(self.display_title())
                                                .size(17.0)
                                                .strong(),
                                        )
                                        .wrap(),
                                    );
                                    if !self.vietnamese_title.is_empty()
                                        && self.vietnamese_title != self.english_title
                                    {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "({})",
                                                self.vietnamese_title
                                            ))
                                            .size(12.0)
                                            .color(muted),
                                        );
                                    }
                                });
                                let n_eps: usize = self.servers.iter().map(|s| s.eps.len()).sum();
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{}  ·  {} episodes  ·  {} server{}",
                                        self.year,
                                        n_eps,
                                        self.servers.len(),
                                        if self.servers.len() == 1 { "" } else { "s" }
                                    ))
                                    .size(12.0)
                                    .color(muted),
                                );
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new("Server:").size(12.0).color(muted),
                                    );
                                    egui::ComboBox::from_id_salt("server")
                                        .selected_text(
                                            self.servers
                                                .get(self.selected_server)
                                                .map(|s| s.name.as_str())
                                                .unwrap_or(""),
                                        )
                                        .show_ui(ui, |ui| {
                                            for (i, s) in self.servers.iter().enumerate() {
                                                ui.selectable_value(
                                                    &mut self.selected_server,
                                                    i,
                                                    &s.name,
                                                );
                                            }
                                        });
                                });
                            });

                        ui.add_space(4.0);

                        // ── Episode selection ─────────────────────────────────────
                        let nchecked: usize;
                        {
                            let Some(server) = self.servers.get_mut(self.selected_server) else {
                                return;
                            };
                            nchecked = server.eps.iter().filter(|e| e.checked).count();
                            if nchecked < 2 && self.concurrency > 1 {
                                self.concurrency = 1;
                            }
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("Episodes")
                                        .size(12.0)
                                        .color(muted)
                                        .strong(),
                                );
                                ui.add_space(4.0);
                                if ui.small_button("Select All").clicked() {
                                    for e in &mut server.eps {
                                        e.checked = true;
                                    }
                                }
                                if ui.small_button("Deselect All").clicked() {
                                    for e in &mut server.eps {
                                        e.checked = false;
                                    }
                                }
                                ui.label(
                                    egui::RichText::new(format!("{nchecked} selected"))
                                        .size(11.0)
                                        .color(muted),
                                );
                            });

                            egui::Frame::group(ui.style())
                                .inner_margin(egui::Margin::symmetric(10, 10))
                                .corner_radius(10.0)
                                .show(ui, |ui| {
                                    egui::ScrollArea::vertical()
                                        .max_height(150.0)
                                        .id_salt("episodes")
                                        .show(ui, |ui| {
                                            for chunk in server.eps.chunks_mut(4) {
                                                ui.horizontal(|ui| {
                                                    for ep in chunk {
                                                        let color = if ep.status.starts_with("fail")
                                                        {
                                                            red
                                                        } else if ep
                                                            .status
                                                            .starts_with("downloading")
                                                            || ep.status.starts_with("resolving")
                                                        {
                                                            amber
                                                        } else if ep.status == "done" {
                                                            green
                                                        } else {
                                                            muted
                                                        };
                                                        // Short cell label; full text is on hover.
                                                        let short = match ep
                                                            .status
                                                            .split_whitespace()
                                                            .next()
                                                        {
                                                            Some("downloading")
                                                            | Some("downloading…") => ep
                                                                .status
                                                                .split_whitespace()
                                                                .nth(1)
                                                                .unwrap_or("…")
                                                                .to_string(),
                                                            Some("fail:") => "fail".to_string(),
                                                            Some("resolving…") => {
                                                                "resolving…".to_string()
                                                            }
                                                            Some(s) => s.to_string(),
                                                            None => String::new(),
                                                        };
                                                        let label = if short.is_empty() {
                                                            format!("EP {}", ep.num)
                                                        } else {
                                                            format!("EP {} · {}", ep.num, short)
                                                        };
                                                        let full = if ep.status.is_empty() {
                                                            format!(
                                                                "EP {}\n{}",
                                                                ep.num, ep.filename
                                                            )
                                                        } else {
                                                            format!(
                                                                "EP {} · {}\n{}",
                                                                ep.num, ep.status, ep.filename
                                                            )
                                                        };
                                                        let mut checked = ep.checked;
                                                        if ui.checkbox(&mut checked, "").changed() {
                                                            ep.checked = checked;
                                                        }
                                                        // "•" (U+2022) exists in all bundled fonts.
                                                        ui.add_sized(
                                                            [160.0, 24.0],
                                                            egui::Label::new(
                                                                egui::RichText::new(format!(
                                                                    "• {label}"
                                                                ))
                                                                .size(12.0)
                                                                .color(color),
                                                            )
                                                            .truncate(),
                                                        )
                                                        .on_hover_text(full);
                                                    }
                                                });
                                            }
                                        });
                                });
                        }

                        ui.add_space(6.0);

                        // ── Options ───────────────────────────────────────────────
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Concurrent episodes:")
                                    .size(12.0)
                                    .color(muted),
                            );
                            ui.add_enabled(
                                nchecked >= 2,
                                egui::Slider::new(&mut self.concurrency, 1..=8)
                                    .show_value(true)
                                    .text(""),
                            );
                            if nchecked < 2 {
                                ui.label(
                                    egui::RichText::new("(select 2+ to enable)")
                                        .size(11.0)
                                        .color(muted),
                                );
                            }
                        });
                        ui.checkbox(
                            &mut self.subfolder,
                            egui::RichText::new("Save into a movie-name subfolder").size(12.0),
                        );
                        // Output folder: label far left, path fills middle, Browse on the right.
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Output:").size(12.0).color(muted));
                            let btn_w = 84.0;
                            let edit_w =
                                (ui.available_width() - btn_w - ui.spacing().item_spacing.x)
                                    .max(120.0);
                            ui.add(
                                egui::TextEdit::singleline(&mut self.output_dir)
                                    .desired_width(edit_w)
                                    .font(egui::TextStyle::Monospace),
                            );
                            if ui.button("Browse…").clicked() {
                                self.pick_folder();
                            }
                        });

                        ui.add_space(6.0);

                        // ── Download action ───────────────────────────────────────
                        ui.horizontal(|ui| {
                            let can_download = !self.loading && !self.downloading && nchecked > 0;
                            let label = if self.downloading {
                                "Downloading…"
                            } else {
                                "Download Selected"
                            };
                            let btn = egui::Button::new(
                                egui::RichText::new(label)
                                    .size(14.0)
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(if can_download { accent } else { muted })
                            .corner_radius(8.0)
                            .min_size(egui::vec2(200.0, 40.0));
                            if ui.add_enabled(can_download, btn).clicked() {
                                self.start_download();
                            }
                            ui.label(
                                egui::RichText::new(if nchecked == 0 {
                                    "Select episodes to begin".to_string()
                                } else {
                                    format!(
                                        "{nchecked} episode{} ready",
                                        if nchecked == 1 { "" } else { "s" }
                                    )
                                })
                                .size(12.0)
                                .color(muted),
                            );
                        });

                        // Overall progress bar while downloading.
                        if self.downloading {
                            ui.add_space(6.0);
                            let (done, total): (usize, usize) = self
                                .ep_progress
                                .values()
                                .fold((0, 0), |(d, t), &(dd, tt)| (d + dd, t + tt));
                            let frac = if total > 0 {
                                done as f32 / total as f32
                            } else {
                                0.0
                            };
                            ui.add(
                                egui::ProgressBar::new(frac)
                                    .text(format!("{done} / {total} segments"))
                                    .desired_height(12.0),
                            );
                        }

                        ui.add_space(6.0);

                        // ── Terminal ──────────────────────────────────────────────
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Terminal")
                                    .size(12.0)
                                    .color(muted)
                                    .strong(),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(format!("{} lines", self.terminal.len()))
                                    .size(11.0)
                                    .color(muted),
                            );
                        });
                        egui::Frame::NONE
                            .fill(term_bg)
                            .corner_radius(10.0)
                            .inner_margin(egui::Margin::symmetric(12, 10))
                            .stroke(egui::Stroke::new(1.0_f32, muted.gamma_multiply(0.3)))
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .max_height(160.0)
                                    .min_scrolled_height(70.0)
                                    .stick_to_bottom(true)
                                    .id_salt("terminal")
                                    .show(ui, |ui| {
                                        for line in &self.terminal {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(line.text.clone())
                                                        .monospace()
                                                        .size(12.0)
                                                        .color(line.color.egui_for(dark)),
                                                )
                                                .selectable(true)
                                                .wrap(),
                                            );
                                        }
                                    });
                            });
                    }); // end outer ScrollArea
            }); // end CentralPanel

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
                            ui.colored_label(red, format!("Update check failed: {e}"));
                        }
                        None => {
                            ui.spinner();
                            ui.label("Checking GitHub releases…");
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
    let icon_data = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).ok();
    // 3:2 aspect ratio, ~960x640: wide enough for the columns, not square.
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([960.0, 640.0])
        .with_min_inner_size([760.0, 540.0]);
    if let Some(icon) = &icon_data {
        viewport = viewport.with_icon(std::sync::Arc::new(icon.clone()));
    }
    let options = eframe::NativeOptions {
        viewport,
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
            let logo = icon_data.map(|icon| {
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [icon.width as usize, icon.height as usize],
                    &icon.rgba,
                );
                cc.egui_ctx.load_texture("logo", img, Default::default())
            });
            Ok(Box::new(NguoncApp::new(handle, logo)))
        }),
    )
}
