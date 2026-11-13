//! Compare, plan and run a sync; space summaries.

use super::*;

impl RsyncStatApp {
    /// Match glow for `pane`, from the other pane's hover on the previous frame.
    /// Selection does not keep the glow after the pointer leaves.
    pub(super) fn partner_highlight_rel(other_hover: &Option<Vec<String>>) -> Option<Vec<String>> {
        other_hover.as_ref().map(|path| ui::rel_within_root(path))
    }

    pub(super) fn rules_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.files_include,
            pattern_options_key(self.include_options),
            self.files_exclude,
            pattern_options_key(self.exclude_options),
            self.use_preset_excludes
        )
    }

    pub(super) fn refresh_compare(&mut self) {
        self.compare_dirty = false;
        if self.source.tree.is_none() || self.target.tree.is_none() {
            self.compare = None;
            return;
        }
        self.compare = Some(rsyncstat_plan::compare_indexes(
            &self.source.file_sizes,
            &self.target.file_sizes,
        ));
        self.preview = Some(self.build_plan());
        self.preview_key = self.rules_key();
    }

    pub(super) fn build_plan(&self) -> SyncPlan {
        match (self.source.tree.as_ref(), self.target.tree.as_ref()) {
            (Some(a), Some(b)) => rsyncstat_plan::plan_sync(
                a,
                b,
                &rsyncstat_plan::rules_from_patterns(
                    &self.files_include,
                    self.include_options,
                    &self.files_exclude,
                    self.exclude_options,
                    self.use_preset_excludes,
                ),
            ),
            _ => SyncPlan::default(),
        }
    }

    pub(super) fn apply_excludes(&mut self) {
        if self.syncing {
            return;
        }
        let (filter, errors) = FilterSet::from_rules(
            &self.files_include,
            self.include_options,
            &self.files_exclude,
            self.exclude_options,
            self.use_preset_excludes,
        );
        self.filter = Arc::new(filter);
        self.pattern_warning = errors.join("\n");
        self.rescan_open_folders();
        self.sync_status =
            "Include and exclude patterns applied. Open folders are scanning again.".to_string();
    }

    pub(super) fn start_sync(&mut self) {
        if self.syncing || !self.folders_idle() {
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
        self.sync_stop_requested = false;
        self.dest_refresh_at = Some(Instant::now() + DEST_REFRESH_EVERY);
        self.recovery_lines.clear();
        self.mismatched_files.clear();
        let (tx, rx) = unbounded();
        self.sync_rx = Some(rx);
        self.sync_status = format!(
            "Copying {} files ({})…",
            plan.copy_count(),
            format_bytes(plan.copy_bytes())
        );
        rsyncstat_exec::spawn_sync(
            root_a,
            root_b,
            plan,
            cancel,
            rsyncstat_exec::mismatch_list_path(),
            tx,
        );
    }

    pub(super) fn stop_sync(&mut self) {
        let Some(cancel) = &self.sync_cancel else {
            return;
        };
        cancel.store(true, Ordering::Relaxed);
        self.sync_stop_requested = true;
        self.sync_status = "Stopping after the current file…".to_string();
    }

    pub(super) fn poll_sync(&mut self) -> bool {
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
                Ok(SyncEvent::Log(line)) => {
                    active = true;
                    self.sync_status = line.clone();
                    self.push_recovery(line);
                }
                Ok(SyncEvent::Finished(report)) => {
                    active = true;
                    keep = false;
                    self.syncing = false;
                    self.sync_cancel = None;
                    let stopped = report.stopped || self.sync_stop_requested;
                    self.sync_stop_requested = false;
                    let failed = if report.errors.is_empty() {
                        String::new()
                    } else {
                        let detail = report
                            .errors
                            .first()
                            .map(|err| format!(" {} {}: {}.", err.direction, err.rel, err.message))
                            .unwrap_or_default();
                        format!(" {} still failing.{detail}", report.errors.len())
                    };
                    let recovered = if report.recovered > 0 {
                        format!(" Recovered {} on retry.", report.recovered)
                    } else {
                        String::new()
                    };
                    let rescan = if report.copied > 0 {
                        " Rescanning both folders."
                    } else {
                        ""
                    };
                    let skipped = if report.skipped > 0 {
                        format!(" Skipped {} with the same hash.", report.skipped)
                    } else {
                        String::new()
                    };
                    let mismatched = if report.mismatched.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " {} differed and were not overwritten.",
                            report.mismatched.len()
                        )
                    };
                    self.mismatched_files = report.mismatched.clone();
                    self.sync_status = if stopped {
                        format!(
                            "Sync stopped — {} files, {}.{skipped}{mismatched}{rescan} Run sync to copy what is left.",
                            report.copied,
                            format_bytes(report.bytes),
                        )
                    } else {
                        format!(
                            "Sync finished — {} files, {}.{recovered}{skipped}{mismatched}{failed}{rescan}",
                            report.copied,
                            format_bytes(report.bytes),
                        )
                    };
                    if report.copied > 0 {
                        self.rescan_open_folders();
                    }
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.syncing = false;
                    self.sync_cancel = None;
                    self.sync_status = if self.sync_stop_requested {
                        "Sync stopped. Run sync to copy what is left.".to_string()
                    } else {
                        "Sync stopped before it finished. Run sync again to retry.".to_string()
                    };
                    self.sync_stop_requested = false;
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

    pub(super) fn content_diff(&self) -> Option<rsyncstat_plan::DirectoryDiff> {
        if self.source.scanning || self.target.scanning {
            return None;
        }
        Some(rsyncstat_plan::directory_diff(
            self.source.tree.as_ref()?,
            self.target.tree.as_ref()?,
        ))
    }

    pub(super) fn pane_space_tiles(&self, pane: ActivePane) -> Vec<volume::SpaceTile> {
        let other = match pane {
            ActivePane::Source => ActivePane::Target,
            ActivePane::Target => ActivePane::Source,
        };
        let mut tiles = volume::space_tiles(self.pane(pane).volume, self.pane(other).volume, true);
        let both_drives = self.source.volume.is_some() && self.target.volume.is_some();
        if !both_drives && !self.source.scanning && !self.target.scanning {
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
}
