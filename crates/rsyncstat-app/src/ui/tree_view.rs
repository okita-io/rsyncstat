//! Directory tree view and the live directory list.

use super::*;

/// Vertical padding around one line of text in a directory tree row.
const TREE_ROW_PAD: f32 = 4.0;

/// Height of one directory tree row: a single line of text plus a little padding.
pub(super) fn tree_row_height(text_height: f32) -> f32 {
    text_height + TREE_ROW_PAD
}

pub fn tree_view_with_path(
    ui: &mut egui::Ui,
    node: &TreeNode,
    path: &[String],
    selected: &mut Option<Vec<String>>,
    hover: &mut Option<Vec<String>>,
    partner_highlight_rel: Option<&[String]>,
    other_files: Option<&HashMap<String, u64>>,
) {
    let tree_id = ui.scope_id().with("directory_tree");
    // The app sets `interact_size.y` to the 30 px control height and widens
    // `item_spacing.y` for focus rings. egui_ltreeview builds each row from
    // both (and ignores its own `default_node_height`), which made rows
    // double spaced. Size rows to one line of text instead.
    let row_h = tree_row_height(ui.text_style_height(&egui::TextStyle::Body));
    let spacing = ui.spacing_mut();
    spacing.interact_size.y = row_h;
    spacing.item_spacing.y = 0.0;
    let mut state: TreeViewState<Vec<String>> =
        TreeViewState::load(ui, tree_id).unwrap_or_default();
    match selected {
        Some(path) if state.selected().first() != Some(path) => {
            state.set_one_selected(path.clone());
        }
        None if !state.selected().is_empty() => state.set_selected(Vec::new()),
        _ => {}
    }

    // The tree remembers the widest row and allocates that on its parent.
    // The clip rect can be the whole window, so a long name would become the
    // column's minimum and push the left edge right. Stay inside the width
    // this column was given.
    let allotted = ui.max_rect().width();
    let visible = ui.clip_rect().width();
    let column = match (allotted.is_finite(), visible.is_finite()) {
        (true, true) => allotted.min(visible),
        (true, false) => allotted,
        (false, true) => visible,
        (false, false) => 280.0,
    };
    let min_width = column.max(80.0);
    let clip_h = ui.clip_rect().height();
    let min_height = if clip_h.is_finite() {
        clip_h.clamp(0.0, 8_000.0)
    } else {
        0.0
    };
    let (_response, actions) = TreeView::new(tree_id)
        .allow_drag_and_drop(false)
        .allow_multi_selection(false)
        .fill_available_width(false)
        .fill_available_height(false)
        .min_width(min_width)
        .max_width(min_width)
        .min_height(min_height)
        .row_layout(RowLayout::Compact)
        .override_striped(Some(false))
        .show_state(ui, &mut state, |builder| {
            let mut rows = TreeRows {
                hover,
                partner_highlight_rel,
                other_files,
            };
            add_tree_node(builder, node, path, &mut rows, true);
        });
    state.store(ui, tree_id);

    for action in actions {
        if let Action::SetSelected(ids) = action {
            *selected = ids.into_iter().next();
        }
    }
}

pub(super) struct TreeRows<'a> {
    pub(super) hover: &'a mut Option<Vec<String>>,
    pub(super) partner_highlight_rel: Option<&'a [String]>,
    pub(super) other_files: Option<&'a HashMap<String, u64>>,
}

pub(super) struct TreeRowLabel {
    pub(super) text: String,
    pub(super) mark: Option<Color32>,
    pub(super) excluded: bool,
    pub(super) partner: bool,
}

pub(super) fn add_tree_node(
    builder: &mut egui_ltreeview::TreeViewBuilder<'_, Vec<String>>,
    node: &TreeNode,
    path: &[String],
    rows: &mut TreeRows<'_>,
    open_by_default: bool,
) {
    let id = path.to_vec();
    let mark = if node.is_dir {
        None
    } else {
        let rel = rel_within_root(path).join("/");
        rows.other_files
            .and_then(|files| rsyncstat_plan::match_file(&rel, node.size, files))
            .map(match_color)
    };
    let label = TreeRowLabel {
        text: tree_row_text(node),
        mark,
        excluded: node.excluded,
        partner: rows
            .partner_highlight_rel
            .is_some_and(|partner| partner == rel_within_root(path).as_slice()),
    };
    let branch = node.is_dir && !node.children.is_empty();
    let opened = builder.node(tree_node(id, label, rows.hover, branch, open_by_default));
    if branch {
        if opened {
            for child in &node.children {
                let mut child_path = path.to_vec();
                child_path.push(child.name.clone());
                add_tree_node(builder, child, &child_path, rows, false);
            }
        }
        builder.close_dir();
    }
}

pub(super) fn tree_node<'a>(
    id: Vec<String>,
    label: TreeRowLabel,
    hover: &'a mut Option<Vec<String>>,
    branch: bool,
    open_by_default: bool,
) -> NodeBuilder<'a, Vec<String>> {
    let path = id.clone();
    let node = if branch {
        NodeBuilder::dir(id)
            .default_open(open_by_default)
            .drop_allowed(false)
    } else {
        NodeBuilder::leaf(id)
    };
    node.label_ui(move |ui| paint_tree_row(ui, &label, &path, hover))
}

pub(super) fn paint_tree_row(
    ui: &mut egui::Ui,
    label: &TreeRowLabel,
    path: &[String],
    hover: &mut Option<Vec<String>>,
) {
    let mut text = egui::RichText::new(&label.text);
    if let Some(color) = label.mark {
        text = text.color(color);
    } else if label.excluded {
        text = text.strikethrough().color(ui.visuals().weak_text_color());
    }
    // Ellipsize inside the row. Hover still shows the full line when it is cut.
    let response = ui.add(egui::Label::new(text).selectable(false).truncate());
    if response.hovered() || pointer_on_row(ui, response.rect) {
        *hover = Some(path.to_vec());
    }
    if label.partner {
        ui.painter().rect_stroke(
            response.rect.expand(1.0),
            2.0,
            Stroke::new(1.5, ui.visuals().selection.stroke.color),
            StrokeKind::Outside,
        );
    }
}

pub(super) fn pointer_on_row(ui: &egui::Ui, label: Rect) -> bool {
    let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) else {
        return false;
    };
    let clip = ui.clip_rect();
    clip.contains(pointer)
        && label.y_range().contains(pointer.y)
        && pointer.x + 32.0 >= label.left()
}

pub(super) fn tree_row_text(node: &TreeNode) -> String {
    let suffix = if node.is_dir { "/" } else { "" };
    // An excluded directory is not measured, so it has no size to show.
    // The row is drawn with a strikethrough instead of an "(excluded)" note.
    if node.excluded && node.is_dir {
        format!("{}{suffix}", node.name)
    } else {
        format!("{}{suffix}  {}", node.name, format_size(node.size))
    }
}

/// Directories the walk is inside right now. Sizes are read from the live tree,
/// so the numbers keep moving as files in those directories are measured.
pub fn live_directories(
    ui: &mut egui::Ui,
    root: Option<&TreeNode>,
    recent: &VecDeque<String>,
    paths: u64,
    files: u64,
    rate: f64,
) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.heading("Scanning");
    });
    ui.label(format!("{paths} paths · {files} files · {rate:.0}/s"));
    ui.add_space(4.0);
    if recent.is_empty() {
        ui.weak("Waiting for the first directory…");
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("live_dirs")
        .max_height(168.0)
        .show(ui, |ui| {
            for (i, rel) in recent.iter().enumerate() {
                let node = root.and_then(|tree| find_rel(tree, rel));
                let excluded = node.map(|n| n.excluded).unwrap_or(false);
                let detail = if excluded {
                    "excluded".to_string()
                } else {
                    format_size(node.map(|n| n.size).unwrap_or(0))
                };
                let line = format!("{rel}  {detail}");
                // Same rule as the directory tree: a path has no spaces, so an
                // unwrapped label would become the column's minimum width.
                let text = if i == 0 {
                    egui::RichText::new(format!("▸ {line}"))
                        .strong()
                        .color(ui.visuals().hyperlink_color)
                } else {
                    egui::RichText::new(line).small().weak()
                };
                ui.add(egui::Label::new(text).truncate());
            }
        });
}

pub(super) fn find_rel<'a>(root: &'a TreeNode, rel: &str) -> Option<&'a TreeNode> {
    let mut node = root;
    for part in rel.split('/') {
        if part.is_empty() {
            continue;
        }
        node = node.child(part)?;
    }
    Some(node)
}

pub fn scan_status_line(ui: &mut egui::Ui, label: &str, path: Option<&PathBuf>) {
    ui.horizontal(|ui| {
        ui.monospace(format!("{label}:"));
        let Some(path) = path else {
            ui.label("—");
            return;
        };
        let full = path.display().to_string();
        let max_w = ui.available_width();
        let font = FontId::proportional(12.0);
        let measure = |s: &str| text_width(ui, s, &font);
        let shown = elide_path_for_width(&full, max_w, measure);
        let resp = ui.label(egui::RichText::new(shown).monospace());
        resp.on_hover_text(full);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_file_tree() -> TreeNode {
        let mut root = TreeNode::new_dir("stories");
        root.children
            .push(TreeNode::file("component_".repeat(40), 12));
        root
    }

    /// The directory tree used to adopt the widest row. A long name then became
    /// the minimum width of whatever column held the tree.
    #[test]
    fn long_file_names_do_not_widen_the_directory_tree() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 900.0));
        let root = long_file_tree();
        let mut selected = None;
        let mut hover = None;
        let mut widths = Vec::new();
        for step in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                time: Some(f64::from(step) / 60.0),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                let (slot, _) = ui.allocate_exact_size(Vec2::new(280.0, 220.0), Sense::hover());
                let inner = ui.scope_builder(egui::UiBuilder::new().max_rect(slot), |ui| {
                    tree_view_with_path(
                        ui,
                        &root,
                        &["stories".to_string()],
                        &mut selected,
                        &mut hover,
                        None,
                        None,
                    );
                });
                widths.push(inner.response.rect.width());
            });
            output.drop_without_applying_deltas();
        }
        assert!(
            widths.iter().all(|width| *width <= 280.0 + 4.0),
            "directory tree widened its column across frames: {widths:?}"
        );
    }

    #[test]
    fn scanning_paths_do_not_widen_the_left_panel() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 900.0));
        let mut recent = VecDeque::new();
        recent.push_front(format!(
            "projects/{}/src/{}/very_long_file_name.rs",
            "component_".repeat(16),
            "module_".repeat(12),
        ));
        let root = long_file_tree();
        let mut widths = Vec::new();
        for step in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                time: Some(f64::from(step) / 60.0),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                let panel = egui::Panel::left("tree_panel")
                    .resizable(true)
                    .default_size(280.0)
                    .show(ui, |ui| {
                        live_directories(ui, Some(&root), &recent, 40, 12, 80.0);
                        ui.separator();
                        let (slot, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 160.0),
                            Sense::hover(),
                        );
                        ui.scope_builder(egui::UiBuilder::new().max_rect(slot), |ui| {
                            egui::ScrollArea::both()
                                .id_salt("directory-tree")
                                .auto_shrink([false, false])
                                .max_height(ui.available_height())
                                .show(ui, |ui| {
                                    let mut selected = None;
                                    let mut hover = None;
                                    tree_view_with_path(
                                        ui,
                                        &root,
                                        &["stories".to_string()],
                                        &mut selected,
                                        &mut hover,
                                        None,
                                        None,
                                    );
                                });
                        });
                    });
                widths.push(panel.response.rect.width());
            });
            output.drop_without_applying_deltas();
        }
        assert!(
            widths.iter().all(|width| *width <= 280.0 + 4.0),
            "left panel grew across frames: {widths:?}"
        );
    }
}
