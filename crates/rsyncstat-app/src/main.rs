//! RSyncStat M0/M1 UI: dual-root treemap comparison.

mod sync;
mod ui;
mod volume;

use crossbeam_channel::{unbounded, Receiver, TryRecvError};
use eframe::egui::{self, Sense, Vec2};
use rsyncstat_filter::{FilterSet, RuleLevel};
use rsyncstat_model::{file_type_bucket, TreeNode};
use rsyncstat_scan::{scan_root_async, ScanUpdate};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use sync::{CompareSummary, SyncEvent, SyncMethod, SyncPlan};

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
    /// Relative path → size for files that are in scope. Used to highlight matches.
    file_sizes: Arc<HashMap<String, u64>>,
    /// Set when `root` is the mount point of a whole drive.
    volume: Option<volume::VolumeSpace>,
    /// Hover text for a free-space or drive-gap tile. Not a path in the tree.
    space_hover: Option<String>,
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
            file_sizes: Arc::new(HashMap::new()),
            volume: None,
            space_hover: None,
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
                if !entry.excluded {
                    let key = entry
                        .rel
                        .components()
                        .map(|component| component.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/");
                    Arc::make_mut(&mut self.file_sizes).insert(key, entry.size);
                }
            }
        }
    }

    fn finish_scan(&mut self) {
        self.scanning = false;
        self.scan_rx = None;
        self.cancel = None;
        self.current_scan_path = None;
        if let Some(root) = self.root.clone() {
            if self.volume.is_some() {
                self.volume = volume::volume_root(&root);
            }
        }
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
        if let Some(vol) = self.volume {
            self.idle_status
                .push_str(&format!(" {} free.", format_bytes(vol.available)));
        }
    }
}

struct RsyncStatApp {
    filter: Arc<FilterSet>,
    source: PaneState,
    target: PaneState,
    dual_pane: bool,
    split_fraction: f32,
    active_pane: ActivePane,
    sync_method: SyncMethod,
    exclude_types: String,
    exclude_dirs: String,
    compare: Option<CompareSummary>,
    compare_dirty: bool,
    preview: Option<SyncPlan>,
    preview_key: String,
    sync_rx: Option<Receiver<SyncEvent>>,
    sync_cancel: Option<Arc<AtomicBool>>,
    syncing: bool,
    sync_status: String,
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
            sync_method: SyncMethod::AtoB,
            exclude_types: String::new(),
            exclude_dirs: String::new(),
            compare: None,
            compare_dirty: false,
            preview: None,
            preview_key: String::new(),
            sync_rx: None,
            sync_cancel: None,
            syncing: false,
            sync_status: "Sync copies files. It does not delete extras.".to_string(),
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
        state.file_sizes = Arc::new(HashMap::new());
        state.volume = volume::volume_root(&path);
        state.space_hover = None;
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
        let same_file_system = state.volume.is_some();
        scan_root_async(path, Arc::clone(&self.filter), tx, cancel, same_file_system);
        self.active_pane = pane;
        self.compare = None;
        self.compare_dirty = true;
        self.preview = None;
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
        if self.compare_dirty && !self.source.scanning && !self.target.scanning {
            self.refresh_compare();
        }
        if self.poll_sync() {
            ctx.request_repaint();
        }
        if repainting && (self.source.scanning || self.target.scanning) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if self.syncing {
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
                    state.file_sizes = Arc::new(HashMap::new());
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

    fn rules_key(&self) -> String {
        format!(
            "{}|{}|{}",
            self.sync_method.label(),
            self.exclude_types,
            self.exclude_dirs
        )
    }

    fn refresh_compare(&mut self) {
        self.compare_dirty = false;
        if self.source.tree.is_none() || self.target.tree.is_none() {
            self.compare = None;
            return;
        }
        self.compare = Some(sync::compare_indexes(
            &self.source.file_sizes,
            &self.target.file_sizes,
        ));
        self.preview = Some(self.build_plan());
        self.preview_key = self.rules_key();
    }

    fn build_plan(&self) -> SyncPlan {
        match (self.source.tree.as_ref(), self.target.tree.as_ref()) {
            (Some(a), Some(b)) => sync::plan_sync(
                a,
                b,
                self.sync_method,
                &sync::rules_from_text(&self.exclude_types, &self.exclude_dirs),
            ),
            _ => SyncPlan::default(),
        }
    }

    fn apply_excludes(&mut self) {
        if self.syncing {
            return;
        }
        let lines = sync::filter_lines(&self.exclude_types, &self.exclude_dirs);
        let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
        self.filter = Arc::new(FilterSet::from_lines(&borrowed, RuleLevel::Profile));
        if let Some(path) = self.source.root.clone() {
            self.start_scan(ActivePane::Source, path);
        }
        if let Some(path) = self.target.root.clone() {
            self.start_scan(ActivePane::Target, path);
        }
        self.sync_status = "Excludes applied. Open folders are scanning again.".to_string();
    }

    fn start_sync(&mut self) {
        if self.syncing || self.source.scanning || self.target.scanning {
            return;
        }
        let (Some(root_a), Some(root_b)) = (self.source.root.clone(), self.target.root.clone())
        else {
            self.sync_status = "Choose both folders before syncing.".to_string();
            return;
        };
        if self.source.tree.is_none() || self.target.tree.is_none() {
            self.sync_status = "Scan both folders before syncing.".to_string();
            return;
        }
        let plan = self.build_plan();
        self.preview = Some(plan.clone());
        self.preview_key = self.rules_key();
        if plan.copy_count() == 0 {
            self.sync_status = "Nothing to copy with these settings.".to_string();
            return;
        }
        if let Some(cancel) = self.sync_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.sync_cancel = Some(Arc::clone(&cancel));
        self.syncing = true;
        let (tx, rx) = unbounded();
        self.sync_rx = Some(rx);
        self.sync_status = format!(
            "Copying {} files ({})…",
            plan.copy_count(),
            format_bytes(plan.copy_bytes())
        );
        sync::spawn_sync(root_a, root_b, plan, cancel, tx);
    }

    fn poll_sync(&mut self) -> bool {
        let Some(rx) = self.sync_rx.take() else {
            return false;
        };
        let mut keep = true;
        let mut active = false;
        loop {
            match rx.try_recv() {
                Ok(SyncEvent::Progress { done, total, rel }) => {
                    active = true;
                    self.sync_status = format!("Syncing {done}/{total} — {rel}");
                }
                Ok(SyncEvent::Finished(report)) => {
                    active = true;
                    keep = false;
                    self.syncing = false;
                    self.sync_cancel = None;
                    let errors = if report.errors.is_empty() {
                        String::new()
                    } else {
                        format!(" {} errors.", report.errors.len())
                    };
                    self.sync_status = format!(
                        "Sync finished — {} files, {}.{} Rescanning both folders.",
                        report.copied,
                        format_bytes(report.bytes),
                        errors
                    );
                    if report.copied > 0 {
                        if let Some(path) = self.source.root.clone() {
                            self.start_scan(ActivePane::Source, path);
                        }
                        if let Some(path) = self.target.root.clone() {
                            self.start_scan(ActivePane::Target, path);
                        }
                    }
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.syncing = false;
                    self.sync_cancel = None;
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            self.sync_rx = Some(rx);
        }
        active || self.syncing
    }

    fn content_diff(&self) -> Option<sync::DirectoryDiff> {
        if !self.dual_pane || self.source.scanning || self.target.scanning {
            return None;
        }
        Some(sync::directory_diff(
            self.source.tree.as_ref()?,
            self.target.tree.as_ref()?,
        ))
    }

    fn pane_space_tiles(&self, pane: ActivePane) -> Vec<volume::SpaceTile> {
        let other = match pane {
            ActivePane::Source => ActivePane::Target,
            ActivePane::Target => ActivePane::Source,
        };
        let mut tiles = volume::space_tiles(
            self.pane(pane).volume,
            self.pane(other).volume,
            self.dual_pane,
        );
        let both_drives = self.source.volume.is_some() && self.target.volume.is_some();
        if self.dual_pane && !both_drives && !self.source.scanning && !self.target.scanning {
            if let (Some(this), Some(that)) = (
                self.pane(pane).tree.as_ref(),
                self.pane(other).tree.as_ref(),
            ) {
                let gap = that.total_size().saturating_sub(this.total_size());
                if gap > 0 {
                    tiles.push(volume::SpaceTile::ContentGap(gap));
                }
            }
        }
        tiles
    }

    fn hover_status(&self) -> Option<String> {
        if let Some(text) = self.source.space_hover.as_ref() {
            if self.source.hover_path.is_none() {
                return Some(format!("Hover A: {text}"));
            }
        }
        if self.dual_pane {
            if let Some(text) = self.target.space_hover.as_ref() {
                if self.target.hover_path.is_none() && self.source.hover_path.is_none() {
                    return Some(format!("Hover B: {text}"));
                }
            }
        }
        let (label, pane, other, other_ready) = if self.source.hover_path.is_some() {
            (
                "A",
                &self.source,
                &self.target.file_sizes,
                self.target.tree.is_some(),
            )
        } else if self.target.hover_path.is_some() {
            (
                "B",
                &self.target,
                &self.source.file_sizes,
                self.source.tree.is_some(),
            )
        } else {
            return None;
        };
        let path = pane.hover_path.as_ref()?;
        let node = pane
            .tree
            .as_ref()
            .and_then(|tree| find_by_path(tree, path))?;
        let rel = path.iter().skip(1).cloned().collect::<Vec<_>>().join("/");
        if node.is_dir {
            return Some(format!("Hover {label}: {rel}/"));
        }
        let mark = if other_ready {
            sync::match_file(&rel, node.size, other)
        } else {
            None
        };
        let compare = match mark {
            Some(sync::MatchKind::Same) => "same file on the other side",
            Some(sync::MatchKind::Different) => "same path, different size",
            None if other_ready => "only in this folder",
            None => "not compared yet",
        };
        Some(format!(
            "Hover {label}: {rel} — {} · {compare}",
            format_bytes(node.size)
        ))
    }
}

impl eframe::App for RsyncStatApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_scans(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let content_diff = self.content_diff();
        egui::Panel::top("menu").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Open source (A)…").clicked() {
                    self.pick_folder(ActivePane::Source);
                }
                if ui.button("Open target (B)…").clicked() {
                    self.pick_folder(ActivePane::Target);
                }
                ui.separator();
                ui.checkbox(&mut self.dual_pane, "Side-by-side A | B");
                ui.separator();
                ui.colored_label(egui::Color32::from_rgb(90, 220, 130), "■");
                ui.label("same file");
                ui.colored_label(egui::Color32::from_rgb(240, 170, 60), "■");
                ui.label("different size");
                ui.separator();
                ui.weak("Names and sizes appear on hover.");
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Sync");
                egui::ComboBox::from_id_salt("sync_method")
                    .selected_text(self.sync_method.label())
                    .show_ui(ui, |ui| {
                        for method in SyncMethod::all() {
                            ui.selectable_value(&mut self.sync_method, method, method.label());
                        }
                    });
                ui.label("Exclude types");
                ui.add(
                    egui::TextEdit::singleline(&mut self.exclude_types)
                        .hint_text("log, tmp, jpg")
                        .desired_width(150.0),
                );
                ui.label("Exclude dirs");
                ui.add(
                    egui::TextEdit::singleline(&mut self.exclude_dirs)
                        .hint_text("node_modules, .git, target")
                        .desired_width(190.0),
                );
                if ui
                    .add_enabled(!self.syncing, egui::Button::new("Apply & rescan"))
                    .clicked()
                {
                    self.apply_excludes();
                }
                if ui
                    .add_enabled(
                        !self.syncing && !self.source.scanning && !self.target.scanning,
                        egui::Button::new("Run sync"),
                    )
                    .clicked()
                {
                    self.start_sync();
                }
                ui.weak(&self.sync_status);
            });
            if let Some(compare) = &self.compare {
                ui.label(format!(
                    "Compare: {} same · {} different size · {} only in A · {} only in B",
                    compare.same, compare.different, compare.only_a, compare.only_b
                ));
            }
            if let Some(diff) = &content_diff {
                ui.label(space_summary(diff));
            }
            if let Some(plan) = &self.preview {
                let stale = self.preview_key != self.rules_key();
                let stale = if stale { " · preview out of date" } else { "" };
                let held = if self.sync_method == SyncMethod::BothWays {
                    format!(", {} size mismatches left as-is", plan.different)
                } else {
                    String::new()
                };
                ui.label(format!(
                    "Plan: {} A→B, {} B→A, {} same{held}, {} excluded{stale}",
                    plan.a_to_b.len(),
                    plan.b_to_a.len(),
                    plan.same,
                    plan.excluded,
                ));
            }
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
            if let Some(hover) = self.hover_status() {
                ui.label(hover);
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
        let other_files = match active {
            ActivePane::Source => Arc::clone(&self.target.file_sizes),
            ActivePane::Target => Arc::clone(&self.source.file_sizes),
        };
        let other_ready = match active {
            ActivePane::Source => self.target.tree.is_some(),
            ActivePane::Target => self.source.tree.is_some(),
        };
        let space_tiles = self.pane_space_tiles(active);

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
                    let tree_height = (ui.available_height() - 320.0).max(100.0);
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
                                other_ready.then_some(other_files.as_ref()),
                            );
                        });
                } else if active_scanning {
                    ui.label("Starting…");
                } else {
                    ui.label("No scan data for this pane.");
                }
                ui.separator();
                ui.heading("Size difference");
                if let Some(diff) = &content_diff {
                    ui::size_diff_view(ui, diff);
                } else if self.dual_pane && (self.source.scanning || self.target.scanning) {
                    ui.weak("Waiting for both scans to finish.");
                } else {
                    ui.weak("Open two folders to compare their size.");
                }
                ui.separator();
                ui.heading("File types");
                if !active_legend.is_empty() {
                    ui::type_legend(ui, &active_legend);
                }
                if !space_tiles.is_empty() {
                    ui.add_space(4.0);
                    ui::space_legend(ui, &space_tiles);
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
                ui.spacing_mut().item_spacing.x = 0.0;
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
        ui.set_max_size(size);
        let bounds = ui.max_rect();
        ui.set_clip_rect(ui.clip_rect().intersect(bounds));
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
                let full = root.display().to_string();
                ui.add(egui::Label::new(&full).truncate())
                    .on_hover_text(full);
            }
            if scanning {
                ui.weak("live");
            }
        });
        ui.separator();

        let tree = self.pane(pane).tree.clone();
        let partner_rel = self.partner_highlight_rel(pane);
        let other_files = match pane {
            ActivePane::Source => Arc::clone(&self.target.file_sizes),
            ActivePane::Target => Arc::clone(&self.source.file_sizes),
        };
        let other_ready = match pane {
            ActivePane::Source => self.target.tree.is_some(),
            ActivePane::Target => self.source.tree.is_some(),
        };
        let space_tiles = self.pane_space_tiles(pane);

        if let Some(ref t) = tree {
            let (selected, hover, space_hover) = match pane {
                ActivePane::Source => (
                    &mut self.source.selected_path,
                    &mut self.source.hover_path,
                    &mut self.source.space_hover,
                ),
                ActivePane::Target => (
                    &mut self.target.selected_path,
                    &mut self.target.hover_path,
                    &mut self.target.space_hover,
                ),
            };
            if t.children.is_empty() && scanning && space_tiles.is_empty() {
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
                    space_hover,
                    partner_highlight_rel: partner_rel,
                };
                ui::treemap_view(
                    ui,
                    t,
                    ctx,
                    pane_id,
                    scanning,
                    other_ready.then_some(other_files.as_ref()),
                    &space_tiles,
                );
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

fn space_summary(diff: &sync::DirectoryDiff) -> String {
    let net = diff.a_minus_b();
    let relation = if net > 0 {
        format!("A is {} larger", format_bytes(net as u64))
    } else if net < 0 {
        format!("B is {} larger", format_bytes((-net) as u64))
    } else {
        "same size".to_string()
    };
    let check = if diff.balanced {
        ""
    } else {
        " · child sizes do not add up"
    };
    format!(
        "Space: A {} · B {} · {relation}{check}",
        format_bytes(diff.bytes_a),
        format_bytes(diff.bytes_b)
    )
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
