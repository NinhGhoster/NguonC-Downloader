//! Native GUI for the Rust engine: egui + eframe (classic `update` API, v0.32).
//! All engine work runs on tokio tasks; results arrive over an mpsc queue and
//! are applied at the top of each frame, so UI state is never touched from a
//! worker thread.
use eframe::egui;
use nguonc_core::{download, generate_filename, scrape, Progress};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Default)]
struct NguoncApp {
    movie_url: String,
    output_dir: String,
    title: String,
    year: String,
    loaded_url: String,
    servers: Vec<ServerRow>,
    selected_server: usize,
    concurrency: usize,
    terminal: Vec<String>,
    error: Option<String>,
    loading: bool,
    downloading: bool,
    rx: Option<Receiver<UiMsg>>,
    last_tick: HashMap<String, Instant>,
    handle: Option<tokio::runtime::Handle>,
}

struct ServerRow {
    name: String,
    eps: Vec<EpiRow>,
}

struct EpiRow {
    num: String,
    embed: String,
    checked: bool,
    status: String, // "" | "downloading... n/m" | "done" | "fail: ..."
    filename: String,
}

enum UiMsg {
    Scraped { url: String, result: Result<nguonc_core::Movie, String> },
    EpProgress { ep: String, done: usize, total: usize, bytes: u64 },
    EpDone { ep: String, ok: bool, err: String },
    AllDone,
}

impl NguoncApp {
    fn new(handle: tokio::runtime::Handle) -> Self {
        let output_dir = std::env::var("HOME")
            .map(|h| format!("{h}/Downloads"))
            .unwrap_or_else(|_| "downloads".to_string());
        Self {
            handle: Some(handle),
            output_dir,
            ..Default::default()
        }
    }

    fn push_terminal(&mut self, line: String) {
        self.terminal.push(line);
        while self.terminal.len() > 100 {
            self.terminal.remove(0);
        }
    }

    fn find_ep_mut(&mut self, num: &str) -> Option<&mut EpiRow> {
        self.servers
            .iter_mut()
            .flat_map(|s| s.eps.iter_mut())
            .find(|e| e.num == num)
    }

    fn apply_msg(&mut self, msg: UiMsg) {
        match msg {
            UiMsg::Scraped { url, result } => {
                self.loading = false;
                self.rx = None;
                match result {
                    Ok(movie) => {
                        self.title = movie.title.clone();
                        self.year = movie.year.clone();
                        self.loaded_url = url;
                        self.selected_server = 0;
                        self.error = None;
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
                        let n: usize = self.servers.iter().map(|s| s.eps.len()).sum();
                        self.push_terminal(format!(
                            "Loaded {} ({}) - {} episodes across {} server(s)",
                            self.title,
                            self.year,
                            n,
                            self.servers.len()
                        ));
                    }
                    Err(e) => {
                        self.error = Some(e.clone());
                        self.push_terminal(format!("load failed: {e}"));
                    }
                }
            }
            UiMsg::EpProgress { ep, done, total, bytes } => {
                let now = Instant::now();
                let due = self
                    .last_tick
                    .get(&ep)
                    .map(|t| now.duration_since(*t) >= Duration::from_millis(300))
                    .unwrap_or(true);
                if due || done == total {
                    self.last_tick.insert(ep.clone(), now);
                    self.push_terminal(format!(
                        "{ep}: {done}/{total} segments ({:.0} MiB)",
                        bytes as f64 / 1048576.0
                    ));
                }
                if let Some(row) = self.find_ep_mut(&ep) {
                    row.status = format!("downloading... {done}/{total}");
                }
            }
            UiMsg::EpDone { ep, ok, err } => {
                if let Some(row) = self.find_ep_mut(&ep) {
                    row.status = if ok { "done".into() } else { format!("fail: {err}") };
                }
                self.push_terminal(if ok {
                    format!("{ep}: done")
                } else {
                    format!("{ep}: FAILED - {err}")
                });
            }
            UiMsg::AllDone => {
                self.downloading = false;
                self.rx = None;
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
                e.status = "downloading...".to_string();
                (e.num.clone(), e.embed.clone(), e.filename.clone())
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let Some(handle) = self.handle.clone() else { return };
        self.rx = Some(rx);
        self.downloading = true;
        let parallel = self.concurrency.max(1);
        let movie_url = self.loaded_url.clone();
        let out_dir = self.output_dir.clone();
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
                    let _ = std::fs::remove_file(&m3u8); // temp playlist; Python leaves these in $TMPDIR
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
        if self.loading || self.downloading {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("NguonC Downloader");
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Movie URL:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.movie_url)
                        .hint_text("https://phim.nguonc.com/phim/...")
                        .desired_width(420.0),
                );
                if ui
                    .add_enabled(!self.loading && !self.downloading, egui::Button::new("Load Movie"))
                    .clicked()
                {
                    let url = self.movie_url.trim().to_string();
                    if !url.is_empty() {
                        self.error = None;
                        self.loading = true;
                        self.servers.clear();
                        self.title.clear();
                        self.terminal.clear();
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

            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, format!("Load failed: {err}"));
            }

            if self.title.is_empty() {
                ui.label("Enter a movie page URL and press Load Movie.");
                return;
            }

            ui.label(format!("Title: {}  ({})", self.title, self.year));

            ui.horizontal(|ui| {
                ui.label("Output dir:");
                ui.add(egui::TextEdit::singleline(&mut self.output_dir).desired_width(420.0));
            });

            egui::ComboBox::from_label("Server")
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

            // Episode list and bulk actions; scoped so the &mut borrow on
            // self.servers is released before the Download button needs it.
            let nchecked: usize;
            {
                let Some(server) = self.servers.get_mut(self.selected_server) else { return };
                nchecked = server.eps.iter().filter(|e| e.checked).count();

                ui.horizontal(|ui| {
                    ui.label("Concurrency:");
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

                ui.horizontal(|ui| {
                    if ui.button("Select All").clicked() {
                        for e in &mut server.eps {
                            e.checked = true;
                        }
                    }
                    if ui.button("Deselect All").clicked() {
                        for e in &mut server.eps {
                            e.checked = false;
                        }
                    }
                });

                egui::ScrollArea::vertical()
                    .max_height(230.0)
                    .show(ui, |ui| {
                        for ep in &mut server.eps {
                            ui.horizontal(|ui| {
                                let mut checked = ep.checked;
                                if ui.checkbox(&mut checked, ep.num.as_str()).changed() {
                                    ep.checked = checked;
                                }
                                let status = if ep.status.is_empty() {
                                    if ep.checked { "pending".to_string() } else { "-".to_string() }
                                } else {
                                    ep.status.clone()
                                };
                                let color = if status.starts_with("fail") {
                                    egui::Color32::RED
                                } else if status.starts_with("downloading") {
                                    egui::Color32::from_rgb(200, 150, 0)
                                } else if status == "done" {
                                    egui::Color32::GREEN
                                } else {
                                    egui::Color32::GRAY
                                };
                                ui.colored_label(color, status);
                            });
                        }
                    });
            }

            let can_download = !self.loading && !self.downloading && nchecked > 0;
            if ui
                .add_enabled(can_download, egui::Button::new("Download selected"))
                .clicked()
            {
                self.start_download();
            }

            ui.separator();
            ui.label("Terminal:");
            egui::ScrollArea::vertical()
                .max_height(150.0)
                .stick_to_bottom(true)
                .id_salt("terminal")
                .show(ui, |ui| {
                    for line in &self.terminal {
                        ui.monospace(line);
                    }
                });
            if self.downloading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Downloading...");
                });
            }
        });
    }
}

fn main() -> eframe::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let handle = rt.handle().clone();
    let app = NguoncApp::new(handle);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([780.0, 640.0])
            .with_min_inner_size([600.0, 460.0]),
        ..Default::default()
    };
    eframe::run_native("rust", options, Box::new(|_cc| Ok(Box::new(app))))
}
