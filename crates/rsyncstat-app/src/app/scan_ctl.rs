//! Folder picking, scanning and live refresh of the receiving folder.

use super::*;

impl RsyncStatApp {
    pub(super) fn pick_folder(&mut self, ctx: &egui::Context, pane: ActivePane) {
        if self.folder_pick.is_some() {
            return;
        }
        let title = match pane {
            ActivePane::Source => "Select source (A)",
            ActivePane::Target => "Select target (B)",
        };
        let (tx, rx) = unbounded();
        self.folder_pick = Some((pane, rx));
        let ctx = ctx.clone();
        // The open panel runs a nested AppKit modal loop. Opening it from this
        // click keeps winit's event handler on the stack, and resizing the panel
        // then delivers another event into that handler and aborts the process.
        // Run it on the next turn of the main queue, after the handler returns.
        run_after_event(move || {
            let path = rfd::FileDialog::new().set_title(title).pick_folder();
            let _ = tx.send(path);
            ctx.request_repaint();
        });
    }

    pub(super) fn poll_folder_pick(&mut self, ctx: &egui::Context) {
        let Some((pane, rx)) = self.folder_pick.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Some(path)) => {
                self.start_scan(pane, path);
                ctx.request_repaint();
            }
            Ok(None) => {}
            Err(TryRecvError::Empty) => self.folder_pick = Some((pane, rx)),
            Err(TryRecvError::Disconnected) => {}
        }
    }

    pub(super) fn start_scan(&mut self, pane: ActivePane, path: PathBuf) {
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
        state.pass = state.pass.saturating_add(1);
        state.warnings.clear();
        state.refreshing = false;
        state.refreshed = false;
        state.idle_status = format!("Scanning {} · pass {}…", path.display(), state.pass);
        let (tx, rx) = unbounded();
        state.scan_rx = Some(rx);
        let same_file_system = state.volume.is_some();
        scan_root_async(path, Arc::clone(&self.filter), tx, cancel, same_file_system);
        self.active_pane = pane;
        self.compare = None;
        self.compare_dirty = true;
        self.preview = None;
    }

    pub(super) fn pane_mut(&mut self, pane: ActivePane) -> &mut PaneState {
        match pane {
            ActivePane::Source => &mut self.source,
            ActivePane::Target => &mut self.target,
        }
    }

    pub(super) fn pane(&self, pane: ActivePane) -> &PaneState {
        match pane {
            ActivePane::Source => &self.source,
            ActivePane::Target => &self.target,
        }
    }

    pub(super) fn poll_scans(&mut self, ctx: &egui::Context) {
        let mut repainting = false;
        repainting |= Self::poll_pane(&mut self.source, ctx);
        repainting |= Self::poll_pane(&mut self.target, ctx);
        if self.source.refreshed || self.target.refreshed {
            self.source.refreshed = false;
            self.target.refreshed = false;
            self.compare_dirty = true;
        }
        if self.compare_dirty && !self.source.scanning && !self.target.scanning {
            self.refresh_compare();
        }
        if self.poll_sync() {
            ctx.request_repaint();
        }
        self.poll_destination_refresh();
        // The scan and sync threads only wake the UI when something asks for
        // another frame. A Rescan click happens after this poll, so `ui` also
        // requests a frame while work is in flight.
        if self.source.scanning
            || self.target.scanning
            || self.source.refreshing
            || self.target.refreshing
            || self.syncing
        {
            ctx.request_repaint_after(Duration::from_millis(50));
        } else if repainting {
            ctx.request_repaint();
        }
    }

    pub(super) fn poll_pane(state: &mut PaneState, ctx: &egui::Context) -> bool {
        let mut processed = 0usize;
        let mut backlog = false;
        if let Some(rx) = state.scan_rx.take() {
            let mut keep = true;
            loop {
                if processed >= REFRESH_APPLY_PER_FRAME {
                    backlog = true;
                    break;
                }
                match rx.try_recv() {
                    Ok(ScanUpdate::Started { root_name }) => {
                        // A reread keeps the map that is already on screen.
                        if state.refreshing {
                            continue;
                        }
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
                        if state.refreshing {
                            state.apply_refresh_batch(entries);
                        } else {
                            state.apply_batch(entries);
                        }
                    }
                    Ok(ScanUpdate::Visiting(path)) => {
                        if !state.refreshing {
                            state.current_scan_path = Some(path);
                        }
                    }
                    Ok(ScanUpdate::Notice(message)) => {
                        if state.warnings.len() < 20 {
                            state.warnings.push(message);
                        }
                    }
                    Ok(ScanUpdate::Finished) => {
                        if state.refreshing {
                            state.finish_visible_refresh();
                        } else {
                            state.finish_scan(true);
                        }
                        keep = false;
                        break;
                    }
                    Ok(ScanUpdate::Failed(message)) => {
                        state.scanning = false;
                        state.scan_rx = None;
                        state.cancel = None;
                        state.current_scan_path = None;
                        if state.refreshing {
                            state.refreshing = false;
                            state.idle_status = format!("Refresh failed: {message}");
                        } else {
                            state.idle_status = format!("Scan failed: {message}");
                        }
                        keep = false;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        if state.refreshing {
                            state.finish_visible_refresh();
                        } else if state.scanning {
                            state.warnings.push(
                                "Scan stopped before it finished. Press Rescan to read the directories again."
                                    .into(),
                            );
                            state.finish_scan(false);
                        }
                        keep = false;
                        break;
                    }
                }
            }
            if keep {
                state.scan_rx = Some(rx);
            }
        }

        if backlog {
            ctx.request_repaint();
        }
        state.refreshed || ((state.scanning || state.refreshing) && (backlog || processed > 0))
    }

    pub(super) fn folders_idle(&self) -> bool {
        !self.source.scanning
            && !self.target.scanning
            && !self.source.refreshing
            && !self.target.refreshing
    }

    pub(super) fn poll_destination_refresh(&mut self) {
        if !self.syncing || self.sync_stop_requested {
            if !self.syncing {
                self.dest_refresh_at = None;
            }
            return;
        }
        if self.source.refreshing || self.target.refreshing {
            return;
        }
        let now = Instant::now();
        match self.dest_refresh_at {
            Some(due) if now < due => return,
            Some(_) => {}
            None => {
                self.dest_refresh_at = Some(now + DEST_REFRESH_EVERY);
                return;
            }
        }
        let mut started = false;
        if self.target.root.is_some() && !self.target.scanning {
            self.start_refresh(ActivePane::Target);
            started = true;
        }
        if started {
            self.dest_refresh_at = None;
        } else {
            self.dest_refresh_at = Some(now + DEST_REFRESH_EVERY);
        }
    }

    pub(super) fn start_refresh(&mut self, pane: ActivePane) {
        let path = match self.pane(pane).root.clone() {
            Some(path) if !self.pane(pane).scanning && !self.pane(pane).refreshing => path,
            _ => return,
        };
        let same_file_system = self.pane(pane).volume.is_some();
        let state = self.pane_mut(pane);
        if let Some(cancel) = state.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        state.cancel = Some(Arc::clone(&cancel));
        state.refreshing = true;
        state.pass = state.pass.saturating_add(1);
        let (tx, rx) = unbounded();
        state.scan_rx = Some(rx);
        scan_root_async(path, Arc::clone(&self.filter), tx, cancel, same_file_system);
    }

    /// Scan every open folder again without changing which tree is on screen.
    /// A single scan selects its pane, so scanning B last would steal Tree A.
    pub(super) fn rescan_open_folders(&mut self) {
        let viewing = self.active_pane;
        if let Some(path) = self.source.root.clone() {
            self.start_scan(ActivePane::Source, path);
        }
        if let Some(path) = self.target.root.clone() {
            self.start_scan(ActivePane::Target, path);
        }
        self.active_pane = viewing;
    }
}
