//! Drawing one folder pane and the delete confirmation dialog.

use super::*;

impl RsyncStatApp {
    pub(super) fn draw_pane(
        &mut self,
        ui: &mut egui::Ui,
        pane: ActivePane,
        size: Vec2,
        pane_id: egui::Id,
        show_mnemonic: bool,
        other_hover: &Option<Vec<String>>,
    ) {
        let scanning = self.pane(pane).scanning;
        ui.set_max_size(size);
        let bounds = ui.max_rect();
        // Leave room for focus rings around toolbar buttons at the pane edge.
        ui.set_clip_rect(
            ui.clip_rect()
                .intersect(bounds)
                .expand(keyboard::FOCUS_RING_OUTSET),
        );
        let (choose_id, open_id, rescan_id, treemap_id) = match pane {
            ActivePane::Source => (
                keyboard::FocusId::PaneAChoose,
                keyboard::FocusId::OpenSourceA,
                keyboard::FocusId::PaneARescan,
                keyboard::FocusId::TreemapA,
            ),
            ActivePane::Target => (
                keyboard::FocusId::PaneBChoose,
                keyboard::FocusId::OpenTargetB,
                keyboard::FocusId::PaneBRescan,
                keyboard::FocusId::TreemapB,
            ),
        };
        ui.horizontal(|ui| {
            let r = keyboard::mnemonic_button(
                ui,
                choose_id,
                keyboard::raw_label_for(choose_id),
                show_mnemonic,
            );
            self.paint_focus_if(ui, r.rect, choose_id);
            if r.clicked() {
                self.kbd.focused = choose_id;
                self.pick_folder(ui.ctx(), pane);
            }
            let mut path = self
                .pane(pane)
                .root
                .as_ref()
                .map(|root| root.display().to_string())
                .unwrap_or_default();
            let colors = ui::FieldColors::for_visuals(ui.visuals());
            let (field, _) = ui.allocate_exact_size(
                egui::Vec2::new(ui.available_width(), ui::CONTROL_HEIGHT),
                egui::Sense::hover(),
            );
            let radius = f32::from(ui::chrome_radius(ui.visuals()));
            ui.painter().rect_filled(field, radius, colors.fill);
            ui.painter().rect_stroke(
                field,
                radius,
                egui::Stroke::new(1.0, colors.border),
                egui::StrokeKind::Inside,
            );
            let edit = ui.put(
                field,
                egui::TextEdit::singleline(&mut path)
                    .id_source(pane_id.with("folder_path"))
                    .desired_width(f32::INFINITY)
                    .interactive(false)
                    .frame(ui::field_text_frame())
                    .align(egui::Align2::LEFT_CENTER)
                    .text_color(colors.icon_hover)
                    .hint_text(egui::RichText::new("No folder selected").color(colors.toggle_text)),
            );
            if !path.is_empty() {
                edit.on_hover_text(path);
            }
        });
        ui.horizontal(|ui| {
            let r = keyboard::mnemonic_button(
                ui,
                open_id,
                keyboard::raw_label_for(open_id),
                show_mnemonic,
            );
            self.paint_focus_if(ui, r.rect, open_id);
            if r.clicked() {
                self.kbd.focused = open_id;
                self.pick_folder(ui.ctx(), pane);
            }
            let r = keyboard::mnemonic_button(
                ui,
                rescan_id,
                keyboard::raw_label_for(rescan_id),
                show_mnemonic,
            );
            self.paint_focus_if(ui, r.rect, rescan_id);
            if r.clicked() {
                self.kbd.focused = rescan_id;
                if let Some(path) = self.pane(pane).root.clone() {
                    self.start_scan(pane, path);
                    ui.ctx().request_repaint();
                }
            }
            if scanning {
                let pass = self.pane(pane).pass;
                ui.weak(format!("reading directories · pass {pass}"));
            } else if self.pane(pane).refreshing {
                ui.weak("refreshing");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut self.pane_mut(pane).show_excluded, "Show excluded")
                    .on_hover_text("Striped tiles for files the filter skipped.");
            });
        });
        ui.separator();

        let tree = self.pane(pane).tree.clone();
        let partner_rel = Self::partner_highlight_rel(other_hover);
        let other_files = match pane {
            ActivePane::Source => Arc::clone(&self.target.file_sizes),
            ActivePane::Target => Arc::clone(&self.source.file_sizes),
        };
        let other_ready = match pane {
            ActivePane::Source => self.target.tree.is_some(),
            ActivePane::Target => self.source.tree.is_some(),
        };
        let space_tiles = self.pane_space_tiles(pane);
        let show_excluded = self.pane(pane).show_excluded;
        let root = self.pane(pane).root.clone().unwrap_or_default();

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
                let treemap_focused = keyboard::is_focused(&self.kbd, treemap_id);
                // Do not allocate this rect: treemap_view paints into the same
                // space. Allocating it first left the map with no room and a
                // blank (near-black) panel. Inset so the focus ring clears the
                // pane edge and neighboring toolbar.
                let map_rect = ui.available_rect_before_wrap().shrink(keyboard::FOCUS_PAD);
                let map_focus = keyboard::focus_region_at(
                    ui,
                    treemap_id,
                    map_rect,
                    egui::Sense::focusable_noninteractive(),
                );
                if ui.input(|i| {
                    i.pointer.primary_clicked()
                        && i.pointer
                            .interact_pos()
                            .is_some_and(|pos| map_rect.contains(pos))
                }) {
                    self.kbd.focused = treemap_id;
                }
                keyboard::paint_focus_ring(ui, map_focus.rect, treemap_focused);
                let ctx = ui::TreemapCtx {
                    selected,
                    hover,
                    space_hover,
                    partner_highlight_rel: partner_rel,
                };
                let mut delete = None;
                let mut notice = None;
                let mut actions = ui::TileActions {
                    root: &root,
                    delete: &mut delete,
                    notice: &mut notice,
                };
                ui::treemap_view(
                    ui,
                    t,
                    ctx,
                    pane_id,
                    scanning,
                    other_ready.then_some(other_files.as_ref()),
                    &space_tiles,
                    show_excluded,
                    &mut actions,
                );
                if let Some(message) = notice {
                    self.sync_status = message;
                }
                if let Some((path, is_dir)) = delete {
                    self.confirm_delete = Some(PendingDelete { pane, path, is_dir });
                }
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

    pub(super) fn confirm_delete_modal(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.confirm_delete.clone() else {
            return;
        };
        let name = pending
            .path
            .file_name()
            .map(|part| part.to_string_lossy().into_owned())
            .unwrap_or_else(|| pending.path.display().to_string());
        let response = egui::Modal::new(egui::Id::unique("delete_file")).show(ctx, |ui| {
            ui.set_min_width(320.0);
            if pending.is_dir {
                ui.heading("Delete folder");
                ui.label(format!(
                    "Move \"{name}\" and everything inside it to the Trash?"
                ));
            } else {
                ui.heading("Delete file");
                ui.label(format!("Move \"{name}\" to the Trash?"));
            }
            ui.weak(pending.path.display().to_string());
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    ui.close();
                    return Some(false);
                }
                if ui.button("Delete").clicked() {
                    ui.close();
                    return Some(true);
                }
                None
            })
            .inner
        });
        if response.inner == Some(true) {
            self.confirm_delete = None;
            self.delete_trashed(pending);
        } else if response.inner == Some(false) || response.should_close() {
            self.confirm_delete = None;
        }
    }

    pub(super) fn delete_trashed(&mut self, pending: PendingDelete) {
        if self.syncing || self.pane(pending.pane).scanning {
            self.sync_status = "Wait for the scan or sync to finish before deleting.".to_string();
            return;
        }
        let root = self.pane(pending.pane).root.clone();
        if root.as_ref().is_some_and(|root| root == &pending.path) {
            self.sync_status = "The scanned folder itself stays put.".to_string();
            return;
        }
        match files::move_to_trash(&pending.path) {
            Ok(()) => {
                let kind = if pending.is_dir { "folder" } else { "file" };
                self.sync_status = format!("Moved {kind} {} to the Trash.", pending.path.display());
                if let Some(root) = root {
                    self.start_scan(pending.pane, root);
                }
            }
            Err(err) => {
                self.sync_status = format!("Could not delete {}: {err}", pending.path.display());
            }
        }
    }
}
