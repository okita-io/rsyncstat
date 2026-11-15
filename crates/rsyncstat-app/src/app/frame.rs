//! Per-frame logic and the main window layout.

use super::*;

const SECTION_BAR_H: f32 = 8.0;

/// Horizontal drag handle between two stacked sections.
fn section_split(ui: &mut egui::Ui, tip: &str) -> egui::Response {
    let (split_rect, split_resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), SECTION_BAR_H),
        Sense::click_and_drag(),
    );
    let stroke_color = if split_resp.dragged() || split_resp.hovered() {
        ui.visuals().selection.stroke.color
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke.color
    };
    ui.painter().hline(
        split_rect.x_range(),
        split_rect.center().y,
        (2.0, stroke_color),
    );
    if split_resp.hovered() || split_resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        split_resp.clone().on_hover_text(tip);
    }
    split_resp
}

fn section_height(span: f32, fraction: f32) -> f32 {
    let min_section = 64.0;
    let usable = (span - SECTION_BAR_H).max(1.0);
    if usable <= min_section * 2.0 {
        usable * 0.5
    } else {
        (usable * fraction).clamp(min_section, usable - min_section)
    }
}

/// Smallest height the file types list gets before the information section above it.
const TYPE_LIST_MIN_H: f32 = 36.0;

/// Height the information section and its split bar may share once the file
/// types section below has `types_min`.
fn info_span_after_types(available: f32, types_min: f32) -> f32 {
    (available - types_min).max(0.0)
}

impl eframe::App for RsyncStatApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.theme.poll() {
            self.theme.apply(ctx);
        }
        // Idle windows still wake so a theme switch shows up without a click.
        ctx.request_repaint_after(crate::omarchy_theme::POLL_INTERVAL);
        self.poll_folder_pick(ctx);
        self.poll_scans(ctx);
        let has_directory_tree = self.pane(self.active_pane).tree.is_some();
        keyboard::handle_frame_input(
            ctx,
            &mut self.kbd,
            has_directory_tree,
            self.active_pane,
            self.source.scanning,
            self.target.scanning,
            self.syncing,
        );
        let action = keyboard::take_pending(&mut self.kbd);
        self.apply_keyboard_action(ctx, action);
        keyboard::draw_help_window(ctx, &mut self.kbd.help_open);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let content_diff = self.content_diff();
        let show_mnemonic = true;
        let pad = keyboard::FOCUS_PAD;
        {
            let spacing = ui.spacing_mut();
            spacing.item_spacing.x = spacing.item_spacing.x.max(pad);
            spacing.item_spacing.y = spacing.item_spacing.y.max(pad);
            spacing.button_padding.x = spacing.button_padding.x.max(pad + 2.0);
            spacing.button_padding.y = spacing.button_padding.y.max(pad);
            // One height for every button, checkbox row and text field.
            spacing.interact_size.y = ui::CONTROL_HEIGHT;
        }
        // Menus, dialogs and popups read the context style, not this panel's.
        ui.ctx().all_styles_mut(|style| {
            style.spacing.interact_size.y = ui::CONTROL_HEIGHT;
            let radius = egui::CornerRadius::same(self.theme.corner_radius());
            style.visuals.window_corner_radius = radius;
            style.visuals.menu_corner_radius = radius;
            let widgets = &mut style.visuals.widgets;
            widgets.noninteractive.corner_radius = radius;
            widgets.inactive.corner_radius = radius;
            widgets.hovered.corner_radius = radius;
            widgets.active.corner_radius = radius;
            widgets.open.corner_radius = radius;
        });
        // egui margins are i8; keep ~5–9 px so focus rings clear panel edges.
        let panel_frame =
            egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(9, 7));
        // Same top margin as the side panel so the first row of buttons in
        // every column starts on one line.
        let central_frame = egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin {
            left: 8,
            right: 8,
            top: 7,
            bottom: 8,
        });

        egui::Panel::top("menu").frame(panel_frame).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.weak(&self.sync_status);
            });
            if !self.mismatched_files.is_empty() {
                let title = match rsyncstat_exec::mismatch_list_path() {
                    Some(path) => format!(
                        "Mismatched files not overwritten ({}) — {}",
                        self.mismatched_files.len(),
                        path.display()
                    ),
                    None => format!(
                        "Mismatched files not overwritten ({})",
                        self.mismatched_files.len()
                    ),
                };
                egui::CollapsingHeader::new(title)
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.label(
                            "These already existed with a different hash. The destination was left as it was.",
                        );
                        egui::ScrollArea::vertical()
                            .max_height(140.0)
                            .show(ui, |ui| {
                                for item in &self.mismatched_files {
                                    ui.label(format!("{} {}", item.direction, item.rel));
                                }
                            });
                    });
            }
            if !self.recovery_lines.is_empty() {
                let log_path = rsyncstat_scan::recovery_log_path();
                let title = match &log_path {
                    Some(path) => format!("Recovery log ({})", path.display()),
                    None => "Recovery log".to_string(),
                };
                egui::CollapsingHeader::new(title)
                    .default_open(true)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(140.0)
                            .show(ui, |ui| {
                                for line in &self.recovery_lines {
                                    ui.label(line);
                                }
                            });
                    });
            }
        });

        egui::Panel::bottom("status")
            .frame(panel_frame)
            .show(ui, |ui| {
                if self.source.scanning {
                    ui::scan_status_line(ui, "A", self.source.current_scan_path.as_ref());
                } else if self.source.refreshing {
                    ui.label(format!("A refreshing · {}", self.source.idle_status));
                } else {
                    ui.label(&self.source.idle_status);
                }
                if self.target.scanning {
                    ui::scan_status_line(ui, "B", self.target.current_scan_path.as_ref());
                } else if self.target.refreshing {
                    ui.label(format!("B refreshing · {}", self.target.idle_status));
                } else {
                    ui.label(&self.target.idle_status);
                }
                if let Some(warning) = self.source.warnings.last() {
                    ui.weak(format!("A: {warning}"));
                }
                if let Some(warning) = self.target.warnings.last() {
                    ui.weak(format!("B: {warning}"));
                }
            });

        // Drop last frame's hover so a pointer that has left the map does not
        // keep the last box glowing. This frame's tree and map write a fresh
        // hover while they are painted.
        let source_hover = self.source.hover_path.clone();
        let target_hover = self.target.hover_path.clone();
        self.source.hover_path = None;
        self.target.hover_path = None;

        let active = self.active_pane;
        let tree = self.pane(active).tree.clone();
        let active_scanning = self.pane(active).scanning;
        let active_recent = self.pane(active).recent_dirs.clone();
        let active_stats_paths = self.pane(active).stats.paths;
        let active_stats_files = self.pane(active).stats.files;
        let active_started = self.pane(active).started;
        let type_rows = ui::type_rows_for_view(
            &self.source.legend,
            Some(&self.target.legend),
            active == ActivePane::Target && self.target.root.is_none(),
        );
        let partner_rel = Self::partner_highlight_rel(match active {
            ActivePane::Source => &target_hover,
            ActivePane::Target => &source_hover,
        });
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
            .frame(panel_frame)
            .show(ui, |ui| {
                ui.columns(3, |cols| {
                    {
                        let ui = &mut cols[0];
                        let r = keyboard::mnemonic_selectable(
                            ui,
                            keyboard::FocusId::TreeTabA,
                            keyboard::raw_label_for(keyboard::FocusId::TreeTabA),
                            active == ActivePane::Source,
                            show_mnemonic,
                        );
                        self.paint_focus_if(ui, r.rect, keyboard::FocusId::TreeTabA);
                        if r.clicked() {
                            self.active_pane = ActivePane::Source;
                            self.kbd.focused = keyboard::FocusId::TreeTabA;
                        }
                    }
                    {
                        let ui = &mut cols[1];
                        let r = keyboard::mnemonic_selectable(
                            ui,
                            keyboard::FocusId::TreeTabB,
                            keyboard::raw_label_for(keyboard::FocusId::TreeTabB),
                            active == ActivePane::Target,
                            show_mnemonic,
                        );
                        self.paint_focus_if(ui, r.rect, keyboard::FocusId::TreeTabB);
                        if r.clicked() {
                            self.active_pane = ActivePane::Target;
                            self.kbd.focused = keyboard::FocusId::TreeTabB;
                        }
                    }
                    self.draw_run_sync_button(&mut cols[2], show_mnemonic);
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
                ui.add_space(pad);
                let include = ui::pattern_edit(
                    ui,
                    "files-include",
                    "files to include",
                    "*.rs, src/**",
                    &mut self.files_include,
                    &self.include_options,
                    ui::PatternIcon::Presets {
                        items: &self.named_presets,
                    },
                    "Named packs (git, node, vite, …). + appends that pack to the fields. Empty includes every file that is not excluded. Aa matches case, ab matches a whole word, .* is a regular expression.",
                    keyboard::is_focused(&self.kbd, keyboard::FocusId::ExcludeTypes),
                );
                self.paint_focus_if(ui, include.rect, keyboard::FocusId::ExcludeTypes);
                if include.text_gained_focus {
                    self.kbd.focused = keyboard::FocusId::ExcludeTypes;
                }
                ui.add_space(pad + 4.0);
                let gear_tip = if self.use_preset_excludes {
                    "Built-in excludes are on (.DS_Store, node_modules, target, …). Click to turn them off."
                } else {
                    "Built-in excludes are off. Click to turn them on."
                };
                let exclude = ui::pattern_edit(
                    ui,
                    "files-exclude",
                    "files to exclude",
                    "*.log, node_modules",
                    &mut self.files_exclude,
                    &self.exclude_options,
                    ui::PatternIcon::Gear {
                        slashed: !self.use_preset_excludes,
                    },
                    gear_tip,
                    keyboard::is_focused(&self.kbd, keyboard::FocusId::ExcludeDirs),
                );
                self.paint_focus_if(ui, exclude.rect, keyboard::FocusId::ExcludeDirs);
                if exclude.text_gained_focus {
                    self.kbd.focused = keyboard::FocusId::ExcludeDirs;
                }
                ui.add_space(pad + 2.0);
                let apply = ui
                    .push_id(
                        keyboard::focus_egui_id(keyboard::FocusId::ApplyRescan),
                        |ui| {
                            ui.add_enabled(
                                !self.syncing,
                                egui::Button::new(keyboard::layout_job_for_label(
                                    ui,
                                    &keyboard::parse_access_label(keyboard::raw_label_for(
                                        keyboard::FocusId::ApplyRescan,
                                    )),
                                    show_mnemonic,
                                    egui::TextStyle::Button,
                                )),
                            )
                        },
                    )
                    .inner;
                self.paint_focus_if(ui, apply.rect, keyboard::FocusId::ApplyRescan);
                if apply.clicked() {
                    self.kbd.focused = keyboard::FocusId::ApplyRescan;
                }
                if !self.syncing {
                    let mut refresh = false;
                    if let Some(toggle) = include.toggled {
                        self.include_options.flip(toggle);
                        refresh = true;
                    }
                    if let Some(toggle) = exclude.toggled {
                        self.exclude_options.flip(toggle);
                        refresh = true;
                    }
                    if exclude.icon_clicked {
                        self.use_preset_excludes = !self.use_preset_excludes;
                        refresh = true;
                    }
                    if apply.clicked() || include.submitted || exclude.submitted {
                        refresh = true;
                    }
                    match &include.preset_action {
                        ui::PresetMenuAction::None => {}
                        ui::PresetMenuAction::Apply(name) => {
                            self.apply_named_preset(name);
                            refresh = false;
                        }
                        ui::PresetMenuAction::SaveNew => {
                            self.preset_name_draft = Some(String::new());
                            self.preset_dialog_error.clear();
                            self.preset_dialog_focus = true;
                        }
                    }
                    if refresh {
                        self.apply_excludes();
                    }
                }
                if !self.pattern_warning.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(&self.pattern_warning)
                            .small()
                            .color(ui.visuals().warn_fg_color),
                    );
                }
                ui.separator();
                let files_top = ui.cursor().min.y;
                let files_span = ui.available_height();
                let usable = (files_span - SECTION_BAR_H).max(1.0);
                let tree_h = section_height(files_span, self.tree_fraction);
                let (slot, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), tree_h),
                    Sense::hover(),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(slot), |ui| {
                    if let Some(ref t) = tree {
                        let root_path = vec![t.name.clone()];
                        let (selected, hover) = match active {
                            ActivePane::Source => (
                                &mut self.source.selected_path,
                                &mut self.source.hover_path,
                            ),
                            ActivePane::Target => (
                                &mut self.target.selected_path,
                                &mut self.target.hover_path,
                            ),
                        };
                        let tree_salt = match active {
                            ActivePane::Source => "tree-a",
                            ActivePane::Target => "tree-b",
                        };
                        let tree_scroll = ui
                            .push_id(tree_salt, |ui| {
                                egui::ScrollArea::both()
                                    .id_salt("directory-tree")
                                    .auto_shrink([false, false])
                                    .max_height(ui.available_height())
                                    .show(ui, |ui| {
                                        ui.push_id(
                                            keyboard::focus_egui_id(
                                                keyboard::FocusId::DirectoryTree,
                                            ),
                                            |ui| {
                                                ui::tree_view_with_path(
                                                    ui,
                                                    t,
                                                    &root_path,
                                                    selected,
                                                    hover,
                                                    partner_rel.as_deref(),
                                                    other_ready.then_some(other_files.as_ref()),
                                                );
                                            },
                                        );
                                    })
                            })
                            .inner;
                        self.paint_focus_if(
                            ui,
                            tree_scroll.inner_rect,
                            keyboard::FocusId::DirectoryTree,
                        );
                        if ui.input(|i| {
                            i.pointer.primary_clicked()
                                && i.pointer
                                    .interact_pos()
                                    .is_some_and(|pos| tree_scroll.inner_rect.contains(pos))
                        }) {
                            self.kbd.focused = keyboard::FocusId::DirectoryTree;
                        }
                    } else if active_scanning {
                        ui.label("Starting…");
                    } else {
                        ui.label("No scan data for this pane.");
                    }
                });
                let split_resp = section_split(ui, "Drag to resize the file list and information.");
                if split_resp.dragged() {
                    if let Some(pos) = split_resp.interact_pointer_pos() {
                        self.tree_fraction = ((pos.y - files_top) / usable).clamp(0.12, 0.88);
                    }
                }
                // Size the information section from what is left after the file
                // types section has its minimum. Otherwise the types heading and
                // list spill past the panel and its edge line runs through the
                // status bar.
                let space_reserve = if space_tiles.is_empty() {
                    0.0
                } else {
                    4.0 + 20.0 * space_tiles.len() as f32
                };
                let item_gap = ui.spacing().item_spacing.y;
                let types_min = ui.text_style_height(&egui::TextStyle::Heading)
                    + item_gap
                    + if type_rows.is_empty() {
                        0.0
                    } else {
                        TYPE_LIST_MIN_H + item_gap
                    }
                    + space_reserve;
                let info_top = ui.cursor().min.y;
                let info_span = info_span_after_types(ui.available_height(), types_min);
                let info_usable = (info_span - SECTION_BAR_H).max(1.0);
                let info_h = section_height(info_span, self.info_fraction);
                let (info_slot, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), info_h),
                    Sense::hover(),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(info_slot), |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("information")
                        .auto_shrink([false, false])
                        .max_height(ui.available_height())
                        .show(ui, |ui| {
                            ui.heading("Information");
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
                                ui.label(format!(
                                    "Plan: {} source → target, {} same size, {} excluded{stale}",
                                    plan.a_to_b.len(),
                                    plan.same,
                                    plan.excluded,
                                ));
                            }
                            if let Some(diff) = &content_diff {
                                ui.add_space(4.0);
                                ui::size_diff_view(ui, diff);
                            } else if self.source.scanning || self.target.scanning {
                                ui.weak("Waiting for both scans to finish.");
                            } else {
                                ui.weak("Open two folders to compare their size.");
                            }
                        });
                });
                let types_split = section_split(ui, "Drag to resize information and file types.");
                if types_split.dragged() {
                    if let Some(pos) = types_split.interact_pointer_pos() {
                        self.info_fraction =
                            ((pos.y - info_top) / info_usable).clamp(0.12, 0.88);
                    }
                }
                ui.heading("File types");
                if !type_rows.is_empty() {
                    let max_h = (ui.available_height() - space_reserve).max(TYPE_LIST_MIN_H);
                    egui::ScrollArea::vertical()
                        .id_salt("file-types")
                        .max_height(max_h)
                        .show(ui, |ui| {
                            ui::type_legend(ui, &type_rows);
                        });
                }
                if !space_tiles.is_empty() {
                    ui.add_space(4.0);
                    ui::space_legend(ui, &space_tiles);
                }
            });

        egui::CentralPanel::default()
            .frame(central_frame)
            .show(ui, |ui| {
                let height = ui.available_height();
                let total_w = ui.available_width();
                let splitter_w = 6.0 + pad;
                let left_w = (total_w - splitter_w) * self.split_fraction.clamp(0.15, 0.85);

                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = pad;
                    ui.allocate_ui_with_layout(
                        Vec2::new(left_w, height),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| {
                            self.draw_pane(
                                ui,
                                ActivePane::Source,
                                Vec2::new(left_w, height),
                                ui.scope_id().with("pane_a"),
                                show_mnemonic,
                                &target_hover,
                            );
                        },
                    );

                    let (split_rect, split_resp) = ui.allocate_exact_size(
                        Vec2::new(splitter_w, height),
                        Sense::click_and_drag(),
                    );
                    let stroke_color = if split_resp.dragged() || split_resp.hovered() {
                        ui.visuals().selection.stroke.color
                    } else {
                        ui.visuals().widgets.noninteractive.bg_stroke.color
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
                    self.paint_focus_if(ui, split_rect, keyboard::FocusId::Splitter);
                    if split_resp.clicked() || split_resp.drag_started() {
                        self.kbd.focused = keyboard::FocusId::Splitter;
                    }
                    // Gaps between the panes and the splitter are not part of
                    // `total_w`. Taking the width still free keeps the target
                    // toolbar inside the window.
                    let right_w = ui.available_width().max(0.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(right_w, height),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| {
                            self.draw_pane(
                                ui,
                                ActivePane::Target,
                                Vec2::new(right_w, height),
                                ui.scope_id().with("pane_b"),
                                show_mnemonic,
                                &source_hover,
                            );
                        },
                    );
                });
            });

        if self.source.scanning || self.target.scanning || self.syncing {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        self.confirm_delete_modal(ui.ctx());
        self.save_preset_modal(ui.ctx());
    }
}

impl RsyncStatApp {
    fn draw_run_sync_button(&mut self, ui: &mut egui::Ui, show_mnemonic: bool) {
        let size = Vec2::new(ui.available_width(), ui::CONTROL_HEIGHT);
        let r = if self.syncing {
            ui.push_id(keyboard::focus_egui_id(keyboard::FocusId::RunSync), |ui| {
                ui.add_enabled(
                    !self.sync_stop_requested,
                    egui::Button::new(if self.sync_stop_requested {
                        "Stopping…"
                    } else {
                        "Stop"
                    })
                    .min_size(size),
                )
            })
            .inner
        } else {
            ui.push_id(keyboard::focus_egui_id(keyboard::FocusId::RunSync), |ui| {
                ui.add_enabled(
                    self.folders_idle(),
                    egui::Button::new(keyboard::layout_job_for_label(
                        ui,
                        &keyboard::parse_access_label(keyboard::raw_label_for(
                            keyboard::FocusId::RunSync,
                        )),
                        show_mnemonic,
                        egui::TextStyle::Button,
                    ))
                    .min_size(size),
                )
            })
            .inner
        };
        self.paint_focus_if(ui, r.rect, keyboard::FocusId::RunSync);
        if r.clicked() {
            self.kbd.focused = keyboard::FocusId::RunSync;
            if self.syncing {
                self.stop_sync();
            } else {
                self.start_sync();
            }
        }
    }
}

fn space_summary(diff: &rsyncstat_plan::DirectoryDiff) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The information section, its split bar and the file types minimum must
    /// fit in the space below the tree, or the left panel grows past the status
    /// bar and its edge line runs through it.
    #[test]
    fn info_section_leaves_room_for_file_types() {
        let types_min = 30.0 + TYPE_LIST_MIN_H;
        for available in [40.0_f32, 120.0, 180.0, 400.0, 900.0] {
            for fraction in [0.12_f32, 0.5, 0.88] {
                let span = info_span_after_types(available, types_min);
                let info_h = section_height(span, fraction);
                let used = info_h + SECTION_BAR_H + types_min;
                if available >= types_min + SECTION_BAR_H + 1.0 {
                    assert!(
                        used <= available + 0.5,
                        "available {available}, fraction {fraction}: used {used}"
                    );
                }
            }
        }
    }

    #[test]
    fn info_span_never_goes_negative() {
        assert_eq!(info_span_after_types(20.0, 66.0), 0.0);
        assert_eq!(info_span_after_types(166.0, 66.0), 100.0);
    }
}
