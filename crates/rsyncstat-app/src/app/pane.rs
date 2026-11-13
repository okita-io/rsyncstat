//! State of one folder pane: its scan, tree and refresh bookkeeping.

use super::*;

pub(super) struct ScanStats {
    pub(super) paths: u64,
    pub(super) files: u64,
    pub(super) excluded_dirs: u64,
    pub(super) excluded_files: u64,
}

pub(super) fn is_copy_temp(rel: &std::path::Path) -> bool {
    rel.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(rsyncstat_exec::COPY_TEMP_SUFFIX))
}

pub(super) struct PaneState {
    pub(super) root: Option<PathBuf>,
    pub(super) tree: Option<TreeNode>,
    pub(super) scanning: bool,
    pub(super) scan_rx: Option<Receiver<ScanUpdate>>,
    pub(super) cancel: Option<Arc<AtomicBool>>,
    pub(super) current_scan_path: Option<PathBuf>,
    pub(super) selected_path: Option<Vec<String>>,
    pub(super) hover_path: Option<Vec<String>>,
    pub(super) recent_dirs: VecDeque<String>,
    pub(super) legend: HashMap<FileTypeId, u64>,
    /// Relative path → size for files that are in scope. Used to highlight matches.
    pub(super) file_sizes: Arc<HashMap<String, u64>>,
    /// When set, excluded files are drawn on this pane's map. Off, their space
    /// goes back to the files that remain.
    pub(super) show_excluded: bool,
    /// Set when `root` is the mount point of a whole drive.
    pub(super) volume: Option<volume::VolumeSpace>,
    /// Hover text for a free-space or drive-gap tile. Not a path in the tree.
    pub(super) space_hover: Option<String>,
    pub(super) stats: ScanStats,
    pub(super) started: Instant,
    pub(super) idle_status: String,
    /// How many times this pane has been scanned. Each scan increments it.
    pub(super) pass: u32,
    /// Directory reads that failed and were retried.
    pub(super) warnings: Vec<String>,
    /// Reread that folds into the tree already on screen.
    pub(super) refreshing: bool,
    /// A refresh published new sizes. Cleared once the compare is rebuilt.
    pub(super) refreshed: bool,
}

impl PaneState {
    pub(super) fn new(idle: &str) -> Self {
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
            show_excluded: false,
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
            pass: 0,
            warnings: Vec::new(),
            refreshing: false,
            refreshed: false,
        }
    }

    pub(super) fn apply_batch(&mut self, entries: Vec<rsyncstat_scan::ScanEntry>) {
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
                let key = entry
                    .rel
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                let seen = if entry.excluded {
                    false
                } else {
                    Arc::make_mut(&mut self.file_sizes)
                        .insert(key, entry.size)
                        .is_some()
                };
                // A retry can report a path the first walk already counted.
                if seen {
                    continue;
                }
                self.stats.files += 1;
                if entry.excluded {
                    self.stats.excluded_files += 1;
                } else if let Some(name) = entry.rel.file_name() {
                    let name = name.to_string_lossy();
                    *self.legend.entry(file_type_id(&name)).or_default() += entry.size;
                }
            }
        }
    }

    pub(super) fn finish_scan(&mut self, clean: bool) {
        self.scanning = false;
        self.scan_rx = None;
        self.cancel = None;
        self.current_scan_path = None;
        self.refresh_volume();
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
        if clean {
            self.idle_status = format!(
                "Scan complete (pass {}) — {} · {} files in {elapsed:.1}s ({rate:.0}/s). {} excluded directories were not entered.",
                self.pass,
                format_bytes(bytes),
                self.stats.files,
                self.stats.excluded_dirs,
            );
        } else {
            self.idle_status = format!(
                "Scan stopped early (pass {}) — {}. Press Rescan to read the directories again.",
                self.pass,
                format_bytes(bytes),
            );
        }
        if let Some(vol) = self.volume {
            self.idle_status
                .push_str(&format!(" {} free.", format_bytes(vol.available)));
        }
        let unread = self
            .warnings
            .iter()
            .filter(|line| line.starts_with("gave up"))
            .count();
        if unread > 0 {
            self.idle_status.push_str(&format!(
                " {unread} director{} could not be read.",
                if unread == 1 { "y" } else { "ies" }
            ));
        }
    }

    pub(super) fn refresh_volume(&mut self) {
        let Some(root) = self.root.clone() else {
            return;
        };
        if self.volume.is_some() {
            self.volume = volume::volume_root(&root);
        }
    }

    pub(super) fn refreshed_status(&self) -> String {
        let bytes = self
            .tree
            .as_ref()
            .map(|tree| tree.total_size())
            .unwrap_or(0);
        let mut status = format!(
            "Refreshed — {} · {} files.",
            format_bytes(bytes),
            self.file_sizes.len(),
        );
        if let Some(vol) = self.volume {
            status.push_str(&format!(" {} free.", format_bytes(vol.available)));
        }
        status
    }

    pub(super) fn mark_refreshed(&mut self) {
        if let Some(tree) = self.tree.as_ref() {
            self.legend = ui::collect_type_sizes(tree);
        }
        self.refresh_volume();
        self.idle_status = self.refreshed_status();
        self.refreshed = true;
    }

    pub(super) fn finish_visible_refresh(&mut self) {
        if let Some(tree) = self.tree.as_mut() {
            tree.sort_children_recursive();
        }
        self.mark_refreshed();
        self.refreshing = false;
        self.cancel = None;
        self.current_scan_path = None;
    }

    /// Add paths from a reread. In-progress copy files are skipped so they do not stick.
    pub(super) fn apply_refresh_batch(&mut self, entries: Vec<rsyncstat_scan::ScanEntry>) {
        let entries = entries
            .into_iter()
            .filter(|entry| !is_copy_temp(&entry.rel))
            .collect();
        self.apply_batch(entries);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_entry(rel: &str, size: u64) -> rsyncstat_scan::ScanEntry {
        rsyncstat_scan::ScanEntry {
            rel: PathBuf::from(rel),
            size,
            is_dir: false,
            excluded: false,
            rule: None,
        }
    }

    #[test]
    fn refresh_keeps_files_already_on_the_map() {
        let mut pane = PaneState::new("idle");
        pane.tree = Some(TreeNode::new_dir("target"));
        pane.apply_batch(vec![file_entry("keep.txt", 5)]);
        pane.apply_refresh_batch(vec![
            file_entry("keep.txt.rsyncstat-tmp", 9),
            file_entry("copied.txt", 4),
        ]);
        let tree = pane.tree.as_ref().unwrap();
        assert_eq!(tree.child("keep.txt").map(|node| node.size), Some(5));
        assert!(tree.child("keep.txt.rsyncstat-tmp").is_none());
        assert_eq!(tree.child("copied.txt").map(|node| node.size), Some(4));
        assert_eq!(tree.size, 9);
    }

    #[test]
    fn refresh_scan_does_not_replace_the_visible_tree() {
        let mut pane = PaneState::new("idle");
        pane.tree = Some(TreeNode::new_dir("target"));
        pane.apply_batch(vec![file_entry("keep.txt", 5)]);
        pane.refreshing = true;
        let (tx, rx) = unbounded();
        tx.send(ScanUpdate::Started {
            root_name: "replaced".into(),
        })
        .unwrap();
        tx.send(ScanUpdate::Batch(vec![file_entry("copied.txt", 2)]))
            .unwrap();
        tx.send(ScanUpdate::Finished).unwrap();
        drop(tx);
        pane.scan_rx = Some(rx);
        let ctx = egui::Context::default();
        assert!(RsyncStatApp::poll_pane(&mut pane, &ctx));
        let tree = pane.tree.as_ref().unwrap();
        assert_eq!(tree.name, "target");
        assert_eq!(tree.child("keep.txt").map(|node| node.size), Some(5));
        assert_eq!(tree.child("copied.txt").map(|node| node.size), Some(2));
        assert!(!pane.refreshing);
        assert!(pane.refreshed);
        assert!(pane.scan_rx.is_none());
    }
}
