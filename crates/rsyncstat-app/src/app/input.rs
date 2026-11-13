//! Keyboard actions, focus handling and recovery-log lines.

use super::*;

impl RsyncStatApp {
    pub(super) fn apply_keyboard_action(
        &mut self,
        ctx: &egui::Context,
        action: keyboard::KeyAction,
    ) {
        use keyboard::FocusId;
        match action {
            keyboard::KeyAction::None => {}
            keyboard::KeyAction::OpenFolder(pane) => self.pick_folder(ctx, pane),
            keyboard::KeyAction::Rescan(pane) => {
                if let Some(path) = self.pane(pane).root.clone() {
                    self.start_scan(pane, path);
                }
            }
            keyboard::KeyAction::SetActivePane(pane) => self.active_pane = pane,
            keyboard::KeyAction::FocusTreeTab(pane) => {
                self.active_pane = pane;
                self.kbd.focused = match pane {
                    ActivePane::Source => FocusId::TreeTabA,
                    ActivePane::Target => FocusId::TreeTabB,
                };
            }
            keyboard::KeyAction::TreemapParent(pane) => {
                let state = self.pane_mut(pane);
                keyboard::treemap_nav_parent(&mut state.selected_path);
            }
            keyboard::KeyAction::TreemapChild(pane) => {
                if let Some(tree) = self.pane(pane).tree.clone() {
                    let state = self.pane_mut(pane);
                    keyboard::treemap_nav_child(&tree, &mut state.selected_path);
                }
            }
            keyboard::KeyAction::RefreshCompare => {
                self.compare_dirty = true;
                self.refresh_compare();
            }
            keyboard::KeyAction::StartSync => {
                if !self.syncing && self.folders_idle() {
                    self.start_sync();
                }
            }
            keyboard::KeyAction::ApplyExcludes => {
                if !self.syncing {
                    self.apply_excludes();
                }
            }
            keyboard::KeyAction::CancelScan(pane) => {
                if let Some(cancel) = self.pane(pane).cancel.clone() {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
            keyboard::KeyAction::CancelSync => {
                if self.syncing {
                    self.stop_sync();
                }
            }
            keyboard::KeyAction::ActivateFocused => self.activate_focused_control(ctx),
            keyboard::KeyAction::ToggleHelp => self.kbd.help_open = !self.kbd.help_open,
        }
    }

    pub(super) fn activate_focused_control(&mut self, ctx: &egui::Context) {
        use keyboard::{raw_label_for, FocusId};
        let slot = self.kbd.focused;
        match slot {
            FocusId::OpenSourceA => self.pick_folder(ctx, ActivePane::Source),
            FocusId::OpenTargetB => self.pick_folder(ctx, ActivePane::Target),
            FocusId::ApplyRescan => {
                if !self.syncing {
                    self.apply_excludes();
                }
            }
            FocusId::RunSync => {
                if self.syncing {
                    if !self.sync_stop_requested {
                        self.stop_sync();
                    }
                } else if self.folders_idle() {
                    self.start_sync();
                }
            }
            FocusId::TreeTabA => self.active_pane = ActivePane::Source,
            FocusId::TreeTabB => self.active_pane = ActivePane::Target,
            FocusId::PaneAChoose => self.pick_folder(ctx, ActivePane::Source),
            FocusId::PaneBChoose => self.pick_folder(ctx, ActivePane::Target),
            FocusId::PaneARescan => {
                if let Some(path) = self.source.root.clone() {
                    self.start_scan(ActivePane::Source, path);
                }
            }
            FocusId::PaneBRescan => {
                if let Some(path) = self.target.root.clone() {
                    self.start_scan(ActivePane::Target, path);
                }
            }
            FocusId::ExcludeTypes
            | FocusId::ExcludeDirs
            | FocusId::DirectoryTree
            | FocusId::TreemapA
            | FocusId::TreemapB
            | FocusId::Splitter => {}
        }
        let _ = raw_label_for(slot);
    }

    pub(super) fn paint_focus_if(&self, ui: &egui::Ui, rect: egui::Rect, slot: keyboard::FocusId) {
        keyboard::paint_focus_ring(ui, rect, keyboard::is_focused(&self.kbd, slot));
    }

    pub(super) fn push_recovery(&mut self, line: String) {
        self.recovery_lines.push(line);
        if self.recovery_lines.len() > 80 {
            self.recovery_lines.remove(0);
        }
    }
}
