//! RSyncStat M0/M1 UI: dual-root treemap comparison.

mod ui;

use crossbeam_channel::{unbounded, Receiver, TryRecvError};
use eframe::egui::{self, Sense, Vec2};
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivePane {
    Source,
    Target,
}

struct ScanStats {
    paths: u64,
    files: u64,
    excluded_dirs: u64,
    excluded_files: u64,
}

struct PaneState {
    root: Option<PathBuf>,
    tree: Option<TreeNode>,
    scanning: bool,
    scan_rx: Option<Receiver<ScanUpdate>>,
    cancel: Option<Arc<AtomicBool>>,
    current_scan_path: Option<PathBuf>,
    selected_path: Option<Vec<String>>,
    hover_path: Option<Vec<String>>,
    recent_dirs: VecDeque<String>,
    legend: HashMap<&'static str, u64>,
    stats: ScanStats,
    started: Instant,
    idle_status: String,
}

impl PaneState {
    fn new(idle: &str) -> Self {
        Self {
            root: None,
            tree: None,
            scanning: false,
            scan_rx: None,
            cancel: None,
            current_scan_path: None,
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
            idle_status: idle.to_string(),
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
        self.scan_rx = None;
        self.cancel = None;
        self.current_scan_path = None;
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
        self.idle_status = format!(
            "Scan complete — {} · {} files in {elapsed:.1}s ({rate:.0}/s). {} excluded directories were not entered.",
            format_bytes(bytes),
            self.stats.files,
            self.stats.excluded_dirs,
        );
    }
}

struct RsyncStatApp {
    filter: Arc<FilterSet>,
    source: PaneState,
    target: PaneState,
    dual_pane: bool,
    split_fraction: f32,
    active_pane: ActivePane,
}

impl RsyncStatApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            filter: Arc::new(FilterSet::default_presets()),
            source: PaneState::new("Source (A): pick a folder to scan."),
            target: PaneState::new("Target (B): pick a folder to scan."),
            dual_pane: true,
            split_fraction: 0.5,
            active_pane: ActivePane::Source,
        }
    }

    fn pick_folder(&mut self, pane: ActivePane) {
        let title = match pane {
            ActivePane::Source => "Select source (A)",
            ActivePane::Target => "Select target (B)",
        };
        let folder = rfd::FileDialog::new().set_title(title).pick_folder();
        if let Some(path) = folder {
            self.start_scan(pane, path);
        }
    }

    fn start_scan(&mut self, pane: ActivePane, path: PathBuf) {
        let state = self.pane_mut(pane);
        if let Some(cancel) = state.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        state.cancel = Some(Arc::clone(&cancel));
        state.root = Some(path.clone());
        state.scanning = true;
        state.tree = None;
        state.recent_dirs.clear();
        state.legend.clear();
        state.stats = ScanStats {
            paths: 0,
            files: 0,
            excluded_dirs: 0,
            excluded_files: 0,
        };
        state.started = Instant::now();
        state.selected_path = None;
        state.hover_path = None;
        state.current_scan_path = Some(path.clone());
        state.idle_status = format!("Scanning {}…", path.display());
        let (tx, rx) = unbounded();
        state.scan_rx = Some(rx);
        scan_root_async(path, Arc::clone(&self.filter), tx, cancel);
        self.active_pane = pane;
    }

    fn pane_mut(&mut self, pane: ActivePane) -> &mut PaneState {
        match pane {
            ActivePane::Source => &mut self.source,
            ActivePane::Target => &mut self.target,
        }
    }

    fn pane(&self, pane: ActivePane) -> &PaneState {
        match pane {
            ActivePane::Source => &self.source,
            ActivePane::Target => &self.target,
        }
    }

    fn poll_scans(&mut self, ctx: &egui::Context) {
        let mut repainting = false;
        repainting |= Self::poll_pane(&mut self.source, ctx);
        repainting |= Self::poll_pane(&mut self.target, ctx);
        if repainting && (self.source.scanning || self.target.scanning) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn poll_pane(state: &mut PaneState, ctx: &egui::Context) -> bool {
        let Some(rx) = state.scan_rx.take() else {
            return false;
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
                    state.tree = Some(TreeNode::new_dir(root_name));
                    state.recent_dirs.clear();
                    state.legend.clear();
                    state.stats = ScanStats {
                        paths: 0,
                        files: 0,
                        excluded_dirs: 0,
                        excluded_files: 0,
                    };
                }
                Ok(ScanUpdate::Batch(entries)) => {
                    processed += entries.len();
                    state.apply_batch(entries);
                }
                Ok(ScanUpdate::Visiting(path)) => {
                    state.current_scan_path = Some(path);
                }
                Ok(ScanUpdate::Finished) => {
                    state.finish_scan();
                    keep = false;
                    break;
                }
                Ok(ScanUpdate::Failed(message)) => {
                    state.scanning = false;
                    state.scan_rx = None;
                    state.cancel = None;
                    state.current_scan_path = None;
                    state.idle_status = format!("Scan failed: {message}");
                    keep = false;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if state.scanning {
                        state.finish_scan();
                    }
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            state.scan_rx = Some(rx);
        }
        if backlog {
            ctx.request_repaint();
        }
        state.scanning && (backlog || processed > 0)
    }

    fn partner_highlight_rel(&self, pane: ActivePane) -> Option<Vec<String>> {
        let other = match pane {
            ActivePane::Source => ActivePane::Target,
            ActivePane::Target => ActivePane::Source,
        };
        let other_state = self.pane(other);
        other_state
            .hover_path
            .as_ref()
            .or(other_state.selected_path.as_ref())
            .map(|p| ui::rel_within_root(p))
    }
}

impl eframe::App for RsyncStatApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_scans(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("menu").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open source (A)…").clicked() {
                    self.pick_folder(ActivePane::Source);
                }
                if ui.button("Open target (B)…").clicked() {
                    self.pick_folder(ActivePane::Target);
                }
                ui.separator();
                ui.checkbox(&mut self.dual_pane, "Side-by-side A | B");
                ui.separator();
                ui.label(
                    "Filters: default presets (*.lancedb, node_modules/, target/, …). \
                     File-type colors match across panes; gold outline marks the counterpart path.",
                );
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            if self.source.scanning {
                ui::scan_status_line(ui, "A", self.source.current_scan_path.as_ref());
            } else {
                ui.label(&self.source.idle_status);
            }
            if self.dual_pane && self.target.scanning {
                ui::scan_status_line(ui, "B", self.target.current_scan_path.as_ref());
            } else if self.dual_pane {
                ui.label(&self.target.idle_status);
            }
        });

        let active = self.active_pane;
        let tree = self.pane(active).tree.clone();
        let active_scanning = self.pane(active).scanning;
        let active_recent = self.pane(active).recent_dirs.clone();
        let active_stats_paths = self.pane(active).stats.paths;
        let active_stats_files = self.pane(active).stats.files;
        let active_started = self.pane(active).started;
        let active_legend = self.pane(active).legend.clone();
        let partner_rel = self
            .pane(match active {
                ActivePane::Source => ActivePane::Target,
                ActivePane::Target => ActivePane::Source,
            })
            .hover_path
            .as_ref()
            .map(|p| ui::rel_within_root(p));

        egui::Panel::left("tree_panel")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(active == ActivePane::Source, "Tree A")
                        .clicked()
                    {
                        self.active_pane = ActivePane::Source;
                    }
                    if ui
                        .selectable_label(active == ActivePane::Target, "Tree B")
                        .clicked()
                    {
                        self.active_pane = ActivePane::Target;
                    }
                });
                ui.separator();
                if active_scanning {
                    let elapsed = active_started.elapsed().as_secs_f64().max(0.001);
                    let rate = active_stats_paths as f64 / elapsed;
                    ui::live_directories(
                        ui,
                        tree.as_ref(),
                        &active_recent,
                        active_stats_paths,
                        active_stats_files,
                        rate,
                    );
                    ui.separator();
                }
                ui.heading("Directory tree");
                ui.separator();
                if let Some(ref t) = tree {
                    let root_path = vec![t.name.clone()];
                    let (selected, hover) = match active {
                        ActivePane::Source => {
                            (&mut self.source.selected_path, &mut self.source.hover_path)
                        }
                        ActivePane::Target => {
                            (&mut self.target.selected_path, &mut self.target.hover_path)
                        }
                    };
                    let tree_height = (ui.available_height() - 150.0).max(120.0);
                    egui::ScrollArea::both()
                        .max_height(tree_height)
                        .show(ui, |ui| {
                            ui::tree_view_with_path(
                                ui,
                                t,
                                &root_path,
                                selected,
                                hover,
                                partner_rel.as_deref(),
                            );
                        });
                } else if active_scanning {
                    ui.label("Starting…");
                } else {
                    ui.label("No scan data for this pane.");
                }
                ui.separator();
                ui.heading("File types");
                if !active_legend.is_empty() {
                    ui::type_legend(ui, &active_legend);
                }
            });

        egui::CentralPanel::default().show(ui, |ui| {
            if !self.dual_pane {
                self.draw_pane(
                    ui,
                    ActivePane::Source,
                    ui.available_size(),
                    ui.scope_id().with("solo"),
                );
                return;
            }

            let height = ui.available_height();
            let total_w = ui.available_width();
            let splitter_w = 6.0;
            let left_w = (total_w - splitter_w) * self.split_fraction.clamp(0.15, 0.85);

            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(left_w, height),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        self.draw_pane(
                            ui,
                            ActivePane::Source,
                            Vec2::new(left_w, height),
                            ui.scope_id().with("pane_a"),
                        );
                    },
                );

                let (split_rect, split_resp) =
                    ui.allocate_exact_size(Vec2::new(splitter_w, height), Sense::click_and_drag());
                let stroke_color = if split_resp.dragged() || split_resp.hovered() {
                    egui::Color32::from_rgb(120, 150, 220)
                } else {
                    egui::Color32::from_gray(80)
                };
                ui.painter().vline(
                    split_rect.center().x,
                    split_rect.y_range(),
                    (2.0, stroke_color),
                );
                if split_resp.dragged() {
                    if let Some(pos) = split_resp.interact_pointer_pos() {
                        let parent = ui.max_rect();
                        self.split_fraction =
                            ((pos.x - parent.left()) / parent.width()).clamp(0.15, 0.85);
                    }
                }

                let right_w = total_w - left_w - splitter_w;
                ui.allocate_ui_with_layout(
                    Vec2::new(right_w, height),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        self.draw_pane(
                            ui,
                            ActivePane::Target,
                            Vec2::new(right_w, height),
                            ui.scope_id().with("pane_b"),
                        );
                    },
                );
            });
        });
    }
}

impl RsyncStatApp {
    fn draw_pane(&mut self, ui: &mut egui::Ui, pane: ActivePane, size: Vec2, pane_id: egui::Id) {
        let title = match pane {
            ActivePane::Source => "Source (A)",
            ActivePane::Target => "Target (B)",
        };
        let scanning = self.pane(pane).scanning;
        ui.set_min_size(size);
        ui.heading(title);
        ui.horizontal(|ui| {
            if ui.button("Choose folder…").clicked() {
                self.pick_folder(pane);
            }
            if ui.button("Rescan").clicked() {
                if let Some(path) = self.pane(pane).root.clone() {
                    self.start_scan(pane, path);
                }
            }
            if let Some(root) = self.pane(pane).root.as_ref() {
                ui.label(format!("{}", root.display()));
            }
            if scanning {
                ui.weak("live");
            }
        });
        ui.separator();

        let tree = self.pane(pane).tree.clone();
        let partner_rel = self.partner_highlight_rel(pane);

        if let Some(ref t) = tree {
            let (selected, hover) = match pane {
                ActivePane::Source => (&mut self.source.selected_path, &mut self.source.hover_path),
                ActivePane::Target => (&mut self.target.selected_path, &mut self.target.hover_path),
            };
            if t.children.is_empty() && scanning {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.spinner();
                        ui.label("Directories show up here as soon as they have files.");
                    });
                });
            } else {
                let ctx = ui::TreemapCtx {
                    selected,
                    hover,
                    partner_highlight_rel: partner_rel,
                };
                ui::treemap_view(ui, t, ctx, pane_id, scanning);
            }
        } else if scanning {
            ui.centered_and_justified(|ui| {
                ui.spinner();
            });
        } else {
            ui.centered_and_justified(|ui| {
                ui.label("Choose a folder to build the treemap.");
            });
        }
    }
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
