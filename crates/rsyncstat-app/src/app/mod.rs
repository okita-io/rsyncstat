//! The RSyncStat window: two folder panes, a treemap each, and a sync bar.
//!
//! State lives here. Behaviour is split by concern in the child modules:
//! `scan_ctl` (folders and scans), `sync_ctl` (compare, plan, sync),
//! `input` (keyboard), `frame` (window layout) and `pane_view` (one pane).

mod frame;
mod input;
mod pane;
mod pane_view;
mod scan_ctl;
mod sync_ctl;

use crate::platform::run_after_event;
use crate::{files, keyboard, presets, ui, volume};
use crossbeam_channel::{unbounded, Receiver, TryRecvError};
use eframe::egui::{self, Sense, Vec2};
use pane::{PaneState, ScanStats};
use rsyncstat_exec::SyncEvent;
use rsyncstat_filter::{FilterSet, NamedPreset, PatternOptions};
use rsyncstat_model::{file_type_id, FileTypeId, TreeNode};
use rsyncstat_plan::{CompareSummary, SyncPlan};
use rsyncstat_scan::{scan_root_async, ScanUpdate};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Reread the folder that is receiving copies while a sync is running.
const DEST_REFRESH_EVERY: Duration = Duration::from_secs(3);
/// Fold at most this many paths into the map on one frame.
const REFRESH_APPLY_PER_FRAME: usize = 8_000;

pub fn run() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("RSyncStat"),
        // Quantized tile colors sit on whole 8-bit values. This keeps the GPU
        // pass from smoothing the in-between samples of the file-map dither.
        dithering: true,
        ..Default::default()
    };
    eframe::run_native(
        "RSyncStat",
        native_options,
        Box::new(|cc| Ok(Box::new(RsyncStatApp::new(cc)))),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivePane {
    Source,
    Target,
}

struct RsyncStatApp {
    filter: Arc<FilterSet>,
    source: PaneState,
    target: PaneState,
    split_fraction: f32,
    /// Share of the left panel, below the filters, given to the file tree.
    /// The rest is Information and File types.
    tree_fraction: f32,
    /// Share of the area under the file tree given to Information.
    /// The rest is File types.
    info_fraction: f32,
    active_pane: ActivePane,
    /// Comma-separated globs. Empty keeps every file that is not excluded.
    files_include: String,
    include_options: PatternOptions,
    /// Comma-separated globs. These replace the old type and directory exclude fields.
    files_exclude: String,
    exclude_options: PatternOptions,
    /// Set when a regular expression in either field could not be compiled.
    pattern_warning: String,
    /// Built-in excludes such as `.DS_Store`, `node_modules`, and `target`.
    use_preset_excludes: bool,
    /// Named packs from `include-exclude.yaml` next to the binary.
    named_presets: Vec<NamedPreset>,
    preset_file: PathBuf,
    /// Open "Save preset" dialog; the string is the name being typed.
    preset_name_draft: Option<String>,
    preset_dialog_error: String,
    preset_dialog_focus: bool,
    compare: Option<CompareSummary>,
    compare_dirty: bool,
    preview: Option<SyncPlan>,
    preview_key: String,
    sync_rx: Option<Receiver<SyncEvent>>,
    sync_cancel: Option<Arc<AtomicBool>>,
    syncing: bool,
    /// Set when the user clicks Stop. Cleared when the copy thread finishes.
    sync_stop_requested: bool,
    /// When to reread the target while it receives copies. `None` while a reread is in flight.
    dest_refresh_at: Option<Instant>,
    sync_status: String,
    /// Tail of the recovery log for the current sync (also written to disk).
    recovery_lines: Vec<String>,
    /// Destination files whose hash differed. They were not overwritten.
    mismatched_files: Vec<rsyncstat_exec::Mismatch>,
    /// Folder dialog started from a click. The panel itself runs after this
    /// event returns; see `pick_folder`.
    folder_pick: Option<(ActivePane, Receiver<Option<PathBuf>>)>,
    /// File or folder the user asked to delete. The dialog confirms it.
    confirm_delete: Option<PendingDelete>,
    kbd: keyboard::KeyboardState,
}

#[derive(Clone)]
struct PendingDelete {
    pane: ActivePane,
    path: PathBuf,
    is_dir: bool,
}

impl RsyncStatApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let preset_file = presets::default_path();
        let (named_presets, preset_notice) = presets::load_or_create(&preset_file);
        let pattern_warning = preset_notice.unwrap_or_default();
        Self {
            filter: Arc::new(FilterSet::default_presets()),
            source: PaneState::new("Source (A): pick a folder to scan."),
            target: PaneState::new("Target (B): pick a folder to scan."),
            split_fraction: 0.5,
            tree_fraction: 0.45,
            info_fraction: 0.55,
            active_pane: ActivePane::Source,
            files_include: String::new(),
            include_options: PatternOptions::default(),
            files_exclude: String::new(),
            exclude_options: PatternOptions::default(),
            pattern_warning,
            use_preset_excludes: true,
            named_presets,
            preset_file,
            preset_name_draft: None,
            preset_dialog_error: String::new(),
            preset_dialog_focus: false,
            compare: None,
            compare_dirty: false,
            preview: None,
            preview_key: String::new(),
            sync_rx: None,
            sync_cancel: None,
            syncing: false,
            sync_stop_requested: false,
            dest_refresh_at: None,
            sync_status: "Copies files from source to target. It does not delete extras."
                .to_string(),
            recovery_lines: Vec::new(),
            mismatched_files: Vec::new(),
            folder_pick: None,
            confirm_delete: None,
            kbd: keyboard::KeyboardState::default(),
        }
    }
}

fn pattern_options_key(options: PatternOptions) -> String {
    format!(
        "{}{}{}",
        u8::from(options.match_case),
        u8::from(options.whole_word),
        u8::from(options.regex)
    )
}

fn format_bytes(n: u64) -> String {
    rsyncstat_model::format_bytes(n, 2)
}
