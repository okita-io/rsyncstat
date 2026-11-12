//! RSyncStat M0: single-root scanner + treemap viewer.

mod ui;

use crossbeam_channel::{unbounded, Receiver, TryRecvError};
use eframe::egui;
use rsyncstat_filter::FilterSet;
use rsyncstat_model::{file_type_bucket, TreeNode};
use rsyncstat_scan::{scan_root_async, ScanUpdate};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("RSyncStat"),
        ..Default::default()
    };
    eframe::run_native(
        "RSyncStat",
        native_options,
        Box::new(|cc| Ok(Box::new(RsyncStatApp::new(cc)))),
    )
}

struct ScanStats {
    paths: u64,
    files: u64,
    excluded_dirs: u64,
    excluded_files: u64,
}

struct RsyncStatApp {
    root: Option<PathBuf>,
    filter: Arc<FilterSet>,
    tree: Option<TreeNode>,
    scanning: bool,
    scan_rx: Option<Receiver<ScanUpdate>>,
    cancel: Option<Arc<AtomicBool>>,
    status: String,
    selected_path: Option<Vec<String>>,
    hover_path: Option<Vec<String>>,
    recent_dirs: VecDeque<String>,
    legend: HashMap<&'static str, u64>,
    stats: ScanStats,
    started: Instant,
}

impl RsyncStatApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            root: None,
            filter: Arc::new(FilterSet::default_presets()),
            tree: None,
            scanning: false,
            scan_rx: None,
            cancel: None,
            status: "Choose a folder to scan.".to_string(),
            selected_path: None,
            hover_path: None,
            recent_dirs: VecDeque::new(),
            legend: HashMap::new(),
            stats: ScanStats {
                paths: 0,
                files: 0,
                excluded_dirs: 0,
                excluded_files: 0,
            },
            started: Instant::now(),
        }
    }

    fn pick_folder(&mut self) {
        let folder = rfd::FileDialog::new().pick_folder();
        if let Some(path) = folder {
            self.start_scan(path);
        }
    }

    fn start_scan(&mut self, path: PathBuf) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        self.root = Some(path.clone());
        self.scanning = true;
        self.tree = None;
        self.recent_dirs.clear();
        self.legend.clear();
        self.stats = ScanStats {
            paths: 0,
            files: 0,
            excluded_dirs: 0,
            excluded_files: 0,
        };
        self.started = Instant::now();
        self.selected_path = None;
        self.hover_path = None;
        self.status = format!("Scanning {}…", path.display());
        let (tx, rx) = unbounded();
        self.scan_rx = Some(rx);
        scan_root_async(path, Arc::clone(&self.filter), tx, cancel);
    }

    fn poll_scan(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.scan_rx.take() else {
            return;
        };
        let mut keep = true;
        let mut processed = 0usize;
        let mut backlog = false;
        loop {
            if processed >= 8_000 {
                backlog = true;
                break;
            }
            match rx.try_recv() {
                Ok(ScanUpdate::Started { root_name }) => {
                    self.tree = Some(TreeNode::new_dir(root_name));
                    self.recent_dirs.clear();
                    self.legend.clear();
                    self.stats = ScanStats {
                        paths: 0,
                        files: 0,
                        excluded_dirs: 0,
                        excluded_files: 0,
                    };
                }
                Ok(ScanUpdate::Batch(entries)) => {
                    processed += entries.len();
                    self.apply_batch(entries);
                }
                Ok(ScanUpdate::Finished) => {
                    self.finish_scan();
                    keep = false;
                    break;
                }
                Ok(ScanUpdate::Failed(message)) => {
                    self.scanning = false;
                    self.status = format!("Scan failed: {message}");
                    keep = false;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.scanning {
                        self.finish_scan();
                    }
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            self.scan_rx = Some(rx);
        }
        if self.scanning {
            self.refresh_status();
            if backlog {
                ctx.request_repaint();
            } else {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
    }

    fn apply_batch(&mut self, entries: Vec<rsyncstat_scan::ScanEntry>) {
        if self.tree.is_none() {
            let name = self
                .root
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "scan".to_string());
            self.tree = Some(TreeNode::new_dir(name));
        }
        let Some(tree) = self.tree.as_mut() else {
            return;
        };
        for entry in entries {
            tree.insert_scanned(
                &entry.rel,
                entry.size,
                entry.is_dir,
                entry.excluded,
                entry.rule.clone(),
            );
            self.stats.paths += 1;
            if entry.is_dir {
                if entry.excluded {
                    self.stats.excluded_dirs += 1;
                }
                let label = entry
                    .rel
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                if let Some(pos) = self.recent_dirs.iter().position(|dir| dir == &label) {
                    self.recent_dirs.remove(pos);
                }
                self.recent_dirs.push_front(label);
                while self.recent_dirs.len() > 12 {
                    self.recent_dirs.pop_back();
                }
            } else {
                self.stats.files += 1;
                if entry.excluded {
                    self.stats.excluded_files += 1;
                }
                if let Some(name) = entry.rel.file_name() {
                    let name = name.to_string_lossy();
                    *self.legend.entry(file_type_bucket(&name)).or_default() += entry.size;
                }
            }
        }
    }

    fn finish_scan(&mut self) {
        self.scanning = false;
        if let Some(tree) = self.tree.as_mut() {
            tree.sort_children_recursive();
            self.legend = ui::collect_type_sizes(tree);
        }
        let bytes = self
            .tree
            .as_ref()
            .map(|tree| tree.total_size())
            .unwrap_or(0);
        let elapsed = self.started.elapsed().as_secs_f64().max(0.001);
        let rate = self.stats.files as f64 / elapsed;
        self.status = format!(
            "Scan complete — {} · {} files in {elapsed:.1}s ({rate:.0}/s). {} excluded directories were not entered.",
            format_bytes(bytes),
            self.stats.files,
            self.stats.excluded_dirs,
        );
    }

    fn refresh_status(&mut self) {
        let elapsed = self.started.elapsed().as_secs_f64().max(0.001);
        let rate = self.stats.paths as f64 / elapsed;
        let bytes = self.tree.as_ref().map(|tree| tree.size).unwrap_or(0);
        let current = self.recent_dirs.front().map(String::as_str).unwrap_or("…");
        let root = self
            .root
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        self.status = format!(
            "Scanning {root} — {} paths · {} · {rate:.0}/s · {} excluded dirs skipped · {current}",
            self.stats.paths,
            format_bytes(bytes),
            self.stats.excluded_dirs,
        );
    }
}

impl eframe::App for RsyncStatApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_scan(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("menu").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open folder…").clicked() {
                    self.pick_folder();
                }
                ui.separator();
                if ui.button("Rescan").clicked() {
                    if let Some(path) = self.root.clone() {
                        self.start_scan(path);
                    }
                }
                ui.separator();
                ui.label(
                    "Filters: default built-in presets (*.lancedb, node_modules/, target/, …)",
                );
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(&self.status);
                if let Some(path) = &self.hover_path {
                    if let Some(node) = self.tree.as_ref().and_then(|t| find_by_path(t, path)) {
                        ui.separator();
                        let rule = node.exclude_rule.as_deref().unwrap_or("(built-in preset)");
                        if node.excluded && node.is_dir {
                            ui.label(format!(
                                "Hover: {} — excluded ({rule}), not measured",
                                path.join("/")
                            ));
                        } else if node.excluded {
                            ui.label(format!(
                                "Hover: {} — excluded ({rule}), {}",
                                path.join("/"),
                                format_bytes(node.size)
                            ));
                        } else {
                            ui.label(format!(
                                "Hover: {} — {}",
                                path.join("/"),
                                format_bytes(node.size)
                            ));
                        }
                    }
                }
            });
        });

        egui::Panel::left("tree_panel")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| {
                if self.scanning {
                    let elapsed = self.started.elapsed().as_secs_f64().max(0.001);
                    let rate = self.stats.paths as f64 / elapsed;
                    ui::live_directories(
                        ui,
                        self.tree.as_ref(),
                        &self.recent_dirs,
                        self.stats.paths,
                        self.stats.files,
                        rate,
                    );
                    ui.separator();
                }
                ui.heading("Directory tree");
                ui.separator();
                if let Some(tree) = &self.tree {
                    let root_path = vec![tree.name.clone()];
                    let tree_height = (ui.available_height() - 150.0).max(120.0);
                    egui::ScrollArea::both()
                        .max_height(tree_height)
                        .show(ui, |ui| {
                            ui::tree_view_with_path(
                                ui,
                                tree,
                                &root_path,
                                &mut self.selected_path,
                                &mut self.hover_path,
                            );
                        });
                } else if self.scanning {
                    ui.label("Starting…");
                } else {
                    ui.label("No data yet.");
                }

                ui.separator();
                ui.heading("File types");
                if !self.legend.is_empty() {
                    ui::type_legend(ui, &self.legend);
                }
            });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Treemap");
                if self.scanning {
                    ui.weak("live");
                    if let Some(dir) = self.recent_dirs.front() {
                        ui.separator();
                        ui.label(dir);
                    }
                }
            });
            if let Some(tree) = &self.tree {
                if tree.children.is_empty() && self.scanning {
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.spinner();
                            ui.label("Directories show up here as soon as they have files.");
                        });
                    });
                } else {
                    ui::treemap_view(
                        ui,
                        tree,
                        &mut self.selected_path,
                        &mut self.hover_path,
                        self.scanning,
                    );
                }
            } else if self.scanning {
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("Open a folder to build the treemap.");
                });
            }
        });
    }
}

fn find_by_path<'a>(root: &'a TreeNode, path: &[String]) -> Option<&'a TreeNode> {
    if path.first()? != &root.name {
        return None;
    }
    let mut node = root;
    for part in path.iter().skip(1) {
        node = node.child(part)?;
    }
    Some(node)
}

fn format_bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let n = n as f64;
    if n >= K * K * K {
        format!("{:.2} GiB", n / (K * K * K))
    } else if n >= K * K {
        format!("{:.2} MiB", n / (K * K))
    } else if n >= K {
        format!("{:.2} KiB", n / K)
    } else {
        format!("{n:.0} B")
    }
}
