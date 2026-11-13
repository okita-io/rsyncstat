//! Tree, treemap, legend, and status widgets.

use crate::sync::{self, MatchKind};
use crate::volume::SpaceTile;
use eframe::egui::{self, Color32, FontId, Mesh, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use egui_ltreeview::{Action, NodeBuilder, RowLayout, TreeView, TreeViewState};
use rsyncstat_filter::{PatternOptions, PatternToggle};
use rsyncstat_model::{
    elide_path_for_width, file_color_rgb, file_type_id, readable_color_rgb, FileTypeId, TreeNode,
};
use rsyncstat_treemap::squarify_indices;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

/// Path segments relative to the scanned root (excludes the root folder name).
pub fn rel_within_root(path: &[String]) -> Vec<String> {
    path.iter().skip(1).cloned().collect()
}

pub struct TreemapCtx<'a> {
    pub selected: &'a mut Option<Vec<String>>,
    pub hover: &'a mut Option<Vec<String>>,
    /// Caption for the free-space or drive-gap tile under the pointer.
    pub space_hover: &'a mut Option<String>,
    /// Relative path highlighted because the user hovered the matching entry in the other pane.
    pub partner_highlight_rel: Option<Vec<String>>,
}

const FREE_COLOR: Color32 = Color32::from_rgb(118, 130, 146);
/// Leaf tiles stay inset by this fixed amount. Glows paint inside it, so a
/// match or hover never grows or shrinks the box.
const TILE_INSET: f32 = 1.0;
/// Nested directory contents keep this pad. It is the grey frame around the
/// files and does not change when a glow is shown.
const DIR_PAD: f32 = 2.0;
/// Bottom edge of each tile is this fraction of the top color, so the box
/// shades 30% darker toward its own bottom edge.
const TILE_SHADE: f32 = 0.7;
/// Same-file match. The outline is a solid line of this color.
const MATCH_GREEN: Color32 = Color32::from_rgb(90, 220, 130);
/// Share of cells filled by a hovered match. Flat across the box.
const HOVER_DITHER: f32 = 0.25;
/// Hover cells are the box color multiplied by this, so they are 30% brighter.
const HOVER_BRIGHT: f32 = 1.3;
const NO_GLOW: DitherGlows = DitherGlows {
    outline: None,
    hover: None,
};

fn color_from_rgb(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
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
    let mut state: TreeViewState<Vec<String>> =
        TreeViewState::load(ui, tree_id).unwrap_or_default();
    match selected {
        Some(path) if state.selected().first() != Some(path) => {
            state.set_one_selected(path.clone());
        }
        None if !state.selected().is_empty() => state.set_selected(Vec::new()),
        _ => {}
    }

    let clip = ui.clip_rect().size();
    let min_width = if clip.x.is_finite() {
        clip.x.max(80.0)
    } else {
        280.0
    };
    let min_height = if clip.y.is_finite() {
        clip.y.clamp(0.0, 8_000.0)
    } else {
        0.0
    };
    let (_response, actions) = TreeView::new(tree_id)
        .allow_drag_and_drop(false)
        .allow_multi_selection(false)
        .fill_available_width(false)
        .fill_available_height(false)
        .min_width(min_width)
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

struct TreeRows<'a> {
    hover: &'a mut Option<Vec<String>>,
    partner_highlight_rel: Option<&'a [String]>,
    other_files: Option<&'a HashMap<String, u64>>,
}

struct TreeRowLabel {
    text: String,
    mark: Option<Color32>,
    excluded: bool,
    partner: bool,
}

fn add_tree_node(
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
            .and_then(|files| sync::match_file(&rel, node.size, files))
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

fn tree_node<'a>(
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

fn paint_tree_row(
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
    let response = ui.add(egui::Label::new(text).selectable(false));
    if response.hovered() || pointer_on_row(ui, response.rect) {
        *hover = Some(path.to_vec());
    }
    if label.partner {
        ui.painter().rect_stroke(
            response.rect.expand(1.0),
            2.0,
            Stroke::new(1.5, Color32::from_rgb(255, 210, 80)),
            StrokeKind::Outside,
        );
    }
}

fn pointer_on_row(ui: &egui::Ui, label: Rect) -> bool {
    let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) else {
        return false;
    };
    let clip = ui.clip_rect();
    clip.contains(pointer)
        && label.y_range().contains(pointer.y)
        && pointer.x + 32.0 >= label.left()
}

fn tree_row_text(node: &TreeNode) -> String {
    let suffix = if node.is_dir { "/" } else { "" };
    // An excluded directory is not measured, so it has no size to show.
    // The row is drawn with a strikethrough instead of an "(excluded)" note.
    if node.excluded && node.is_dir {
        format!("{}{suffix}", node.name)
    } else {
        format!("{}{suffix}  {}", node.name, format_size(node.size))
    }
}

/// Sizes of files the include and exclude rules kept. Excluded files are omitted.
pub fn collect_type_sizes(root: &TreeNode) -> HashMap<FileTypeId, u64> {
    let mut buckets = HashMap::new();
    collect_buckets(root, &mut buckets);
    buckets
}

/// File types for the tree on screen. Tree B with no chosen target is empty,
/// so Tree A's types do not show in that view. Otherwise sizes from the open
/// panes are added together, largest first.
pub fn type_rows_for_view(
    source: &HashMap<FileTypeId, u64>,
    target: Option<&HashMap<FileTypeId, u64>>,
    tree_b_without_target: bool,
) -> Vec<(FileTypeId, u64)> {
    if tree_b_without_target {
        return Vec::new();
    }
    let mut legends = vec![source];
    if let Some(target) = target {
        legends.push(target);
    }
    merged_type_rows(&legends)
}

/// Extensions from every pane, sizes added together, largest first.
pub fn merged_type_rows(legends: &[&HashMap<FileTypeId, u64>]) -> Vec<(FileTypeId, u64)> {
    let mut merged: HashMap<FileTypeId, u64> = HashMap::new();
    for legend in legends {
        for (id, bytes) in *legend {
            *merged.entry(id.clone()).or_default() += *bytes;
        }
    }
    let mut items: Vec<_> = merged.into_iter().collect();
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.label().cmp(&b.0.label())));
    items
}

pub fn type_legend(ui: &mut egui::Ui, rows: &[(FileTypeId, u64)]) {
    for (id, bytes) in rows {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(12.0, 12.0), Sense::hover());
            ui.painter()
                .rect_filled(rect, 0.0, color_from_rgb(id.color_rgb()));
            ui.label(format!("{}  {}", id.label(), format_size(*bytes)));
        });
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
                if i == 0 {
                    ui.label(
                        egui::RichText::new(format!("▸ {line}"))
                            .strong()
                            .color(Color32::from_rgb(150, 200, 255)),
                    );
                } else {
                    ui.label(egui::RichText::new(line).small().weak());
                }
            }
        });
}

fn find_rel<'a>(root: &'a TreeNode, rel: &str) -> Option<&'a TreeNode> {
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

fn text_width(ui: &egui::Ui, text: &str, font_id: &FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font_id.clone(), Color32::PLACEHOLDER)
        .size()
        .x
}

fn collect_buckets(node: &TreeNode, out: &mut HashMap<FileTypeId, u64>) {
    if node.is_dir {
        if node.excluded {
            return;
        }
        for child in &node.children {
            collect_buckets(child, out);
        }
    } else if !node.excluded {
        let id = file_type_id(&node.name);
        *out.entry(id).or_default() += node.size;
    }
}

pub struct TileActions<'a> {
    pub root: &'a std::path::Path,
    /// Path and whether it is a directory, set when Delete is chosen.
    pub delete: &'a mut Option<(PathBuf, bool)>,
    pub notice: &'a mut Option<String>,
}

pub fn treemap_view(
    ui: &mut egui::Ui,
    root: &TreeNode,
    ctx: TreemapCtx<'_>,
    pane_id: egui::Id,
    live: bool,
    other_files: Option<&HashMap<String, u64>>,
    space: &[SpaceTile],
    show_excluded: bool,
    actions: &mut TileActions<'_>,
) {
    let visible = ui.available_rect_before_wrap().intersect(ui.clip_rect());
    if visible.width() < 2.0 || visible.height() < 2.0 {
        return;
    }
    let (response, painter) = ui.allocate_painter(visible.size(), Sense::click_and_drag());
    let rect = response.rect.intersect(ui.clip_rect());
    if rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }
    // While the walk is running, draw two levels so the map can refresh many
    // times a second. The finished scan paints the full nesting.
    let levels = if live { 2 } else { 32 };
    let mut path = vec![root.name.clone()];
    let partner = ctx.partner_highlight_rel.as_deref();
    *ctx.space_hover = None;
    // The scanned root is the box top-level files sit in. Grey, no outline.
    painter.rect_filled(rect, 0.0, directory_grey(ui.visuals().dark_mode, 0));
    let grid = map_grid(rect, painter.pixels_per_point());
    paint_children(
        ui,
        &painter,
        &grid,
        root,
        &mut path,
        rect,
        ctx.selected,
        ctx.hover,
        ctx.space_hover,
        pane_id.with(response.id),
        levels,
        live,
        partner,
        other_files,
        space,
        1,
        show_excluded,
        actions,
    );
}

fn layout_values(children: &[TreeNode], live: bool, show_excluded: bool) -> Vec<f64> {
    let mut values: Vec<f64> = children
        .iter()
        .map(|child| layout_bytes(child, show_excluded) as f64)
        .collect();
    // A single huge directory would otherwise be re-laid-out in full on every
    // live frame. Keep the largest tiles; the finished scan draws everything.
    const LIVE_TILES: usize = 800;
    if live && values.len() > LIVE_TILES {
        let mut ranked = values.clone();
        ranked.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let cutoff = ranked[LIVE_TILES - 1];
        for value in &mut values {
            if *value < cutoff {
                *value = 0.0;
            }
        }
    }
    values
}

/// Weight used to place a tile. Hidden excluded files contribute nothing, and
/// a directory shrinks by the excluded files inside it so siblings stay proportional.
fn layout_bytes(node: &TreeNode, show_excluded: bool) -> u64 {
    if node.excluded {
        return if show_excluded && !node.is_dir {
            node.size
        } else {
            0
        };
    }
    if show_excluded || !node.is_dir {
        return node.size;
    }
    node.size.saturating_sub(hidden_excluded_bytes(node))
}

fn hidden_excluded_bytes(node: &TreeNode) -> u64 {
    let mut total = 0u64;
    for child in &node.children {
        if child.excluded {
            if !child.is_dir {
                total = total.saturating_add(child.size);
            }
        } else if child.is_dir {
            total = total.saturating_add(hidden_excluded_bytes(child));
        }
    }
    total
}

fn paint_children(
    ui: &egui::Ui,
    painter: &egui::Painter,
    grid: &MapGrid,
    parent: &TreeNode,
    path: &mut Vec<String>,
    rect: Rect,
    selected: &mut Option<Vec<String>>,
    hover: &mut Option<Vec<String>>,
    space_hover: &mut Option<String>,
    parent_id: egui::Id,
    levels: usize,
    live: bool,
    partner_highlight_rel: Option<&[String]>,
    other_files: Option<&HashMap<String, u64>>,
    extras: &[SpaceTile],
    depth: usize,
    show_excluded: bool,
    actions: &mut TileActions<'_>,
) {
    let mut values = layout_values(&parent.children, live, show_excluded);
    for extra in extras {
        values.push(extra.bytes() as f64);
    }
    let layout = squarify_indices(
        &values,
        rect.left(),
        rect.top(),
        rect.width(),
        rect.height(),
    );
    for item in layout {
        let tile = Rect::from_min_size(
            Pos2::new(item.rect.x, item.rect.y),
            Vec2::new(item.rect.w, item.rect.h),
        );
        if item.item >= parent.children.len() {
            paint_space_tile(
                ui,
                painter,
                grid,
                tile,
                extras[item.item - parent.children.len()],
                parent_id,
                hover,
                space_hover,
            );
            continue;
        }
        let child = &parent.children[item.item];
        path.push(child.name.clone());
        paint_node(
            ui,
            painter,
            grid,
            child,
            path,
            tile,
            selected,
            hover,
            space_hover,
            parent_id,
            levels,
            live,
            partner_highlight_rel,
            other_files,
            depth,
            show_excluded,
            actions,
        );
        path.pop();
    }
}

fn paint_node(
    ui: &egui::Ui,
    painter: &egui::Painter,
    grid: &MapGrid,
    node: &TreeNode,
    path: &mut Vec<String>,
    rect: Rect,
    selected: &mut Option<Vec<String>>,
    hover: &mut Option<Vec<String>>,
    space_hover: &mut Option<String>,
    parent_id: egui::Id,
    levels: usize,
    live: bool,
    partner_highlight_rel: Option<&[String]>,
    other_files: Option<&HashMap<String, u64>>,
    depth: usize,
    show_excluded: bool,
    actions: &mut TileActions<'_>,
) {
    if rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }

    let too_wide = live && node.children.len() > 400;
    let nest =
        node.is_dir && !node.excluded && !node.children.is_empty() && levels > 1 && !too_wide;
    let this_rel = rel_within_root(path);
    let rel = this_rel.join("/");
    let partner_here = partner_highlight_rel == Some(this_rel.as_slice());
    let mark = if node.is_dir {
        None
    } else {
        other_files.and_then(|files| sync::match_file(&rel, node.size, files))
    };
    // Glows use this same rect as the fill. Nothing here insets or expands it.
    let tile = rect.shrink(TILE_INSET);

    if node.excluded {
        hatch_rect(painter, rect, ui.visuals().weak_text_color());
    } else if nest {
        painter.rect_filled(rect, 0.0, directory_grey(ui.visuals().dark_mode, depth));
        let inner = rect.shrink(DIR_PAD);
        // Register this folder before its children so the tile under the pointer
        // is the one that shows the tooltip.
        track_pointer(
            ui,
            rect,
            path,
            parent_id,
            selected,
            hover,
            space_hover,
            &rel,
            node,
            mark,
            other_files.is_some(),
            actions,
        );
        if inner.is_positive() {
            paint_children(
                ui,
                painter,
                grid,
                node,
                path,
                inner,
                selected,
                hover,
                space_hover,
                parent_id,
                levels - 1,
                live,
                partner_highlight_rel,
                other_files,
                &[],
                depth + 1,
                show_excluded,
                actions,
            );
        }
    } else if node.is_dir {
        let color = color_from_rgb(readable_color_rgb(&node.name)).gamma_multiply(0.85);
        paint_tile_gradient(
            painter,
            grid,
            tile,
            color,
            &DitherGlows {
                outline: None,
                hover: partner_here.then_some(color),
            },
        );
    } else {
        let color = color_from_rgb(file_color_rgb(&node.name));
        paint_tile_gradient(
            painter,
            grid,
            tile,
            color,
            &DitherGlows {
                outline: mark.map(match_color),
                hover: partner_here.then_some(color),
            },
        );
    }

    // A nested folder and an excluded file have no dither fill of their own.
    // The hover still stipples the same Bayer cells, in that box's color.
    if (nest || node.excluded) && partner_here {
        let color = if node.excluded {
            ui.visuals().weak_text_color()
        } else {
            directory_grey(ui.visuals().dark_mode, depth)
        };
        paint_dither_overlay(
            painter,
            grid,
            if nest { rect } else { tile },
            &DitherGlows {
                outline: None,
                hover: Some(color),
            },
        );
    }
    if !nest {
        track_pointer(
            ui,
            rect,
            path,
            parent_id,
            selected,
            hover,
            space_hover,
            &rel,
            node,
            mark,
            other_files.is_some(),
            actions,
        );
    }
}

pub fn size_diff_view(ui: &mut egui::Ui, diff: &crate::sync::DirectoryDiff) {
    let net = diff.a_minus_b();
    let headline = if net > 0 {
        format!("A is {} larger than B", format_size(net as u64))
    } else if net < 0 {
        format!("B is {} larger than A", format_size((-net) as u64))
    } else {
        "A and B are the same size".to_string()
    };
    ui.label(headline);
    ui.label(format!(
        "A {} · B {}",
        format_size(diff.bytes_a),
        format_size(diff.bytes_b)
    ));
    if diff.entries.is_empty() {
        ui.weak(if diff.balanced {
            "Every top-level folder matches."
        } else {
            "Child sizes do not add up to the folder totals."
        });
        return;
    }
    let shown = diff.entries.len().min(40);
    egui::ScrollArea::vertical()
        .id_salt("size_diff")
        .max_height(168.0)
        .show(ui, |ui| {
            for entry in diff.entries.iter().take(shown) {
                ui.label(format!("{}  {}", entry.name, on_side(entry.a_minus_b())));
            }
        });
    if diff.entries.len() > shown {
        let rest: i128 = diff
            .entries
            .iter()
            .skip(shown)
            .map(crate::sync::DirDelta::a_minus_b)
            .sum();
        ui.weak(format!(
            "{} more · {}",
            diff.entries.len() - shown,
            on_side(rest)
        ));
    }
    ui.weak(if diff.balanced {
        "Top-level folders add up to the totals."
    } else {
        "Child sizes do not add up to the folder totals."
    });
}

fn on_side(delta: i128) -> String {
    if delta > 0 {
        format!("+{} on A", format_size(delta as u64))
    } else if delta < 0 {
        format!("+{} on B", format_size((-delta) as u64))
    } else {
        "same".to_string()
    }
}

pub fn space_legend(ui: &mut egui::Ui, tiles: &[SpaceTile]) {
    for tile in tiles {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(12.0, 12.0), Sense::hover());
            match tile {
                SpaceTile::Free(_) => {
                    ui.painter().rect_filled(rect, 0.0, FREE_COLOR);
                }
                SpaceTile::Gap(_) | SpaceTile::ContentGap(_) => {
                    ui.painter().rect_stroke(
                        rect,
                        0.0,
                        Stroke::new(1.0, Color32::from_gray(160)),
                        StrokeKind::Inside,
                    );
                }
            }
            ui.label(space_caption(*tile));
        });
    }
}

fn paint_space_tile(
    ui: &egui::Ui,
    painter: &egui::Painter,
    grid: &MapGrid,
    rect: Rect,
    tile: SpaceTile,
    parent_id: egui::Id,
    hover: &mut Option<Vec<String>>,
    space_hover: &mut Option<String>,
) {
    if rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }
    match tile {
        SpaceTile::Free(_) => {
            paint_tile_gradient(painter, grid, rect.shrink(TILE_INSET), FREE_COLOR, &NO_GLOW);
        }
        SpaceTile::Gap(_) | SpaceTile::ContentGap(_) => {
            painter.rect_stroke(
                rect.shrink(2.0),
                0.0,
                Stroke::new(1.0, Color32::from_gray(90)),
                StrokeKind::Inside,
            );
        }
    }
    let id = parent_id.with(match tile {
        SpaceTile::Free(_) => "free-space",
        SpaceTile::Gap(_) => "size-gap",
        SpaceTile::ContentGap(_) => "content-gap",
    });
    let interact = ui.interact(rect, id, Sense::hover());
    if interact.hovered() {
        *hover = None;
        let caption = space_caption(tile);
        *space_hover = Some(caption.clone());
        interact.on_hover_text(caption);
    }
}

fn space_caption(tile: SpaceTile) -> String {
    match tile {
        SpaceTile::Free(bytes) => format!("Free space — {} remaining", format_size(bytes)),
        SpaceTile::Gap(bytes) => {
            format!(
                "Empty — {} smaller than the other drive",
                format_size(bytes)
            )
        }
        SpaceTile::ContentGap(bytes) => {
            format!(
                "Empty — {} less data than the other folder",
                format_size(bytes)
            )
        }
    }
}

fn match_color(kind: MatchKind) -> Color32 {
    match kind {
        MatchKind::Same => MATCH_GREEN,
        MatchKind::Different => Color32::from_rgb(240, 170, 60),
    }
}

fn track_pointer(
    ui: &egui::Ui,
    rect: Rect,
    path: &[String],
    parent_id: egui::Id,
    selected: &mut Option<Vec<String>>,
    hover: &mut Option<Vec<String>>,
    space_hover: &mut Option<String>,
    rel: &str,
    node: &TreeNode,
    mark: Option<MatchKind>,
    other_compared: bool,
    actions: &mut TileActions<'_>,
) -> bool {
    let id = parent_id.with(path.join("/"));
    let interact = ui.interact(rect, id, Sense::click());
    if interact.hovered() {
        *hover = Some(path.to_vec());
        *space_hover = None;
        show_tile_tip(&interact, hover_tip(rel, node, mark, other_compared));
    }
    if interact.clicked() || interact.secondary_clicked() {
        *selected = Some(path.to_vec());
    }
    let disk = crate::files::disk_path(actions.root, path);
    interact.context_menu(|ui| {
        if ui.button("Open File Location").clicked() {
            if let Err(err) = crate::files::reveal(&disk) {
                *actions.notice = Some(format!("Could not open {}: {err}", disk.display()));
            }
            ui.close();
        }
        if ui.button("Copy File Path").clicked() {
            ui.ctx().copy_text(disk.display().to_string());
            ui.close();
        }
        ui.separator();
        if ui.button("Delete").clicked() {
            *selected = Some(path.to_vec());
            *actions.delete = Some((disk.clone(), node.is_dir));
            ui.close();
        }
    });
    interact.hovered()
}

fn show_tile_tip(response: &egui::Response, text: impl Into<egui::WidgetText>) {
    let mut tip = egui::Tooltip::for_enabled(response);
    tip.popup = tip.popup.align(egui::RectAlign::TOP);
    tip.show(|ui| {
        ui.set_max_width(ui.spacing().tooltip_width);
        ui.label(text);
    });
}

fn hover_tip(rel: &str, node: &TreeNode, mark: Option<MatchKind>, other_compared: bool) -> String {
    if node.is_dir {
        let name = if rel.is_empty() {
            format!("{}/", node.name)
        } else {
            format!("{rel}/")
        };
        let detail = if node.excluded {
            "excluded, not measured".to_string()
        } else {
            format_size(node.size)
        };
        return format!("{name}\n{detail}");
    }
    let size = format_size(node.size);
    if node.excluded {
        return format!("{rel}\nexcluded · {size}");
    }
    let compare = match mark {
        Some(MatchKind::Same) => "same file on the other side",
        Some(MatchKind::Different) => "same path, different size",
        None if other_compared => "only in this folder",
        None => "",
    };
    if compare.is_empty() {
        format!("{rel}\n{size}")
    } else {
        format!("{rel}\n{size} · {compare}")
    }
}

/// Ordered-dither steps between the tile color and a 30% darker shade.
const DITHER_LEVELS: u8 = 5;
/// One rendered pixel is this many device pixels in every box.
const MAP_PIXEL_PX: i32 = 6;
/// On a large pane the pixel grows so the shorter side stays near this many
/// rendered pixels, stretched to the pane.
const MAP_SHORT_SIDE_PX: f32 = 800.0;

/// Shared pixel lattice for one file map. Every box steps by `cell_px`.
struct MapGrid {
    pixels_per_point: f32,
    cell_px: i32,
    origin_x: i32,
    origin_y: i32,
}

fn map_grid(rect: Rect, pixels_per_point: f32) -> MapGrid {
    let pixels_per_point = pixels_per_point.max(1.0);
    let short_side = rect.width().min(rect.height()) * pixels_per_point;
    MapGrid {
        pixels_per_point,
        cell_px: map_cell_px(short_side),
        origin_x: (rect.left() * pixels_per_point).round() as i32,
        origin_y: (rect.top() * pixels_per_point).round() as i32,
    }
}

fn map_cell_px(short_side_px: f32) -> i32 {
    let stretched = (short_side_px / MAP_SHORT_SIDE_PX).round().max(1.0) as i32;
    stretched.max(MAP_PIXEL_PX)
}
/// 4×4 Bayer matrix. A cell uses the high color when its mix clears this threshold.
const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Match outline is a solid edge. Hover is the box color, drawn on the same
/// quarter of the Bayer cells.
struct DitherGlows {
    outline: Option<Color32>,
    hover: Option<Color32>,
}

/// Each box shades from its own color at the top edge to 30% darker at the
/// bottom edge. The fade is a pixel-aligned Bayer stipple, so neighboring
/// boxes do not share one smooth gradient. A match outline is a solid frame
/// one map pixel thick. A hover fills that same quarter of the cells with
/// this box's color, 30% brighter.
fn paint_tile_gradient(
    painter: &egui::Painter,
    grid: &MapGrid,
    rect: Rect,
    color: Color32,
    glows: &DitherGlows,
) {
    paint_dither(painter, grid, rect, Some(color), glows);
}

/// Stippled glow on top of a folder frame or an excluded hatch, which have no
/// shade fill of their own.
fn paint_dither_overlay(painter: &egui::Painter, grid: &MapGrid, rect: Rect, glows: &DitherGlows) {
    paint_dither(painter, grid, rect, None, glows);
}

fn paint_dither(
    painter: &egui::Painter,
    grid: &MapGrid,
    rect: Rect,
    fill: Option<Color32>,
    glows: &DitherGlows,
) {
    let cells = DitherGlows {
        outline: None,
        hover: glows.hover,
    };
    if let Some(mesh) = dither_mesh(rect, grid, fill, &cells) {
        painter.add(mesh);
    }
    if let Some(color) = glows.outline {
        for bar in outline_frame(rect, grid) {
            painter.rect_filled(bar, 0.0, color);
        }
    }
}

fn dither_mesh(
    rect: Rect,
    grid: &MapGrid,
    fill: Option<Color32>,
    glows: &DitherGlows,
) -> Option<Mesh> {
    if !rect.is_positive() {
        return None;
    }
    let ppp = grid.pixels_per_point.max(1.0);
    let cell = grid.cell_px.max(1);
    let left = (rect.left() * ppp).round() as i32;
    let top = (rect.top() * ppp).round() as i32;
    let right = (rect.right() * ppp).round() as i32;
    let bottom = (rect.bottom() * ppp).round() as i32;
    if right <= left || bottom <= top {
        return None;
    }
    let height = (bottom - top) as f32;
    let mut mesh = Mesh::default();
    let mut x = left - (left - grid.origin_x).rem_euclid(cell);
    while x < right {
        let x1 = x + cell;
        let vis_x0 = x.max(left);
        let vis_x1 = x1.min(right);
        let gx = (x - grid.origin_x).div_euclid(cell);
        let mut y = top - (top - grid.origin_y).rem_euclid(cell);
        while y < bottom {
            let y1 = y + cell;
            let vis_y0 = y.max(top);
            let vis_y1 = y1.min(bottom);
            let gy = (y - grid.origin_y).div_euclid(cell);
            let bayer = BAYER4[gy.rem_euclid(4) as usize][gx.rem_euclid(4) as usize];
            let t = if height <= 1.0 {
                0.0
            } else {
                ((vis_y0 + vis_y1) as f32 * 0.5 - top as f32) / height
            };
            let mut color = fill.map(|top_color| {
                lerp_color(
                    top_color,
                    shade_darker(top_color, TILE_SHADE),
                    dither_mix(t, bayer, DITHER_LEVELS),
                )
            });
            if glows
                .hover
                .is_some_and(|_| dither_lit(HOVER_DITHER, bayer))
            {
                color = glows.hover.map(|base| shade_brighter(base, HOVER_BRIGHT));
            }
            if let Some(color) = color {
                push_solid_quad(
                    &mut mesh,
                    Rect::from_min_max(
                        Pos2::new(vis_x0 as f32 / ppp, vis_y0 as f32 / ppp),
                        Pos2::new(vis_x1 as f32 / ppp, vis_y1 as f32 / ppp),
                    ),
                    color,
                );
            }
            y = y1;
        }
        x = x1;
    }
    if mesh.is_empty() {
        None
    } else {
        Some(mesh)
    }
}

/// Where `t` sits between `levels` bands. Bayer values of 0 take the lighter
/// band first; 15 stays on the darker band until the mix is almost complete.
fn dither_mix(t: f32, bayer: u8, levels: u8) -> f32 {
    let bands = u8::max(levels, 2) as f32;
    let scaled = t.clamp(0.0, 1.0) * (bands - 1.0);
    let lower = scaled.floor();
    let frac = scaled - lower;
    let step = if frac * 16.0 > f32::from(bayer) {
        lower + 1.0
    } else {
        lower
    };
    (step / (bands - 1.0)).clamp(0.0, 1.0)
}

/// Solid match frame. Every side is `grid.cell_px` device pixels, so a box
/// that lands between grid lines gets the same line as one that lands on them.
/// A tile smaller than that pixel is filled solid instead of growing a thinner line.
fn outline_frame(rect: Rect, grid: &MapGrid) -> Vec<Rect> {
    let ppp = grid.pixels_per_point.max(1.0);
    let left = (rect.left() * ppp).round() as i32;
    let top = (rect.top() * ppp).round() as i32;
    let right = (rect.right() * ppp).round() as i32;
    let bottom = (rect.bottom() * ppp).round() as i32;
    let width = right - left;
    let height = bottom - top;
    if width <= 0 || height <= 0 {
        return Vec::new();
    }
    let thickness = grid.cell_px.max(1).min(width).min(height);
    let to_points = |x0: i32, y0: i32, x1: i32, y1: i32| {
        Rect::from_min_max(
            Pos2::new(x0 as f32 / ppp, y0 as f32 / ppp),
            Pos2::new(x1 as f32 / ppp, y1 as f32 / ppp),
        )
    };
    let mut bars = vec![to_points(left, top, right, (top + thickness).min(bottom))];
    let bottom_y = (bottom - thickness).max(top);
    if bottom_y > top {
        bars.push(to_points(left, bottom_y, right, bottom));
    }
    let side_top = top + thickness;
    let side_bottom = bottom - thickness;
    if side_bottom > side_top {
        bars.push(to_points(
            left,
            side_top,
            (left + thickness).min(right),
            side_bottom,
        ));
        let right_x = (right - thickness).max(left);
        if right_x > left {
            bars.push(to_points(right_x, side_top, right, side_bottom));
        }
    }
    bars
}

fn dither_lit(coverage: f32, bayer: u8) -> bool {
    coverage > 0.0 && coverage * 16.0 > f32::from(bayer)
}

fn lerp_color(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |start: u8, end: u8| (start as f32 + (end as f32 - start as f32) * t).round() as u8;
    let [r1, g1, b1, a1] = from.to_array();
    let [r2, g2, b2, a2] = to.to_array();
    Color32::from_rgba_unmultiplied(mix(r1, r2), mix(g1, g2), mix(b1, b2), mix(a1, a2))
}

fn push_solid_quad(mesh: &mut Mesh, rect: Rect, color: Color32) {
    if !rect.is_positive() {
        return;
    }
    let index = mesh.vertices.len() as u32;
    mesh.colored_vertex(rect.left_top(), color);
    mesh.colored_vertex(rect.right_top(), color);
    mesh.colored_vertex(rect.right_bottom(), color);
    mesh.colored_vertex(rect.left_bottom(), color);
    mesh.add_triangle(index, index + 1, index + 2);
    mesh.add_triangle(index, index + 2, index + 3);
}

/// Keep hue and alpha. Multiply the color channels so the result is darker.
fn shade_darker(color: Color32, factor: f32) -> Color32 {
    scale_channels(color, factor)
}

/// Keep hue and alpha. Multiply the color channels, clamped, so the result is brighter.
fn shade_brighter(color: Color32, factor: f32) -> Color32 {
    scale_channels(color, factor)
}

fn scale_channels(color: Color32, factor: f32) -> Color32 {
    let [r, g, b, a] = color.to_array();
    let scale = |channel: u8| (channel as f32 * factor).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(scale(r), scale(g), scale(b), a)
}

/// Directory frame. Adjacent depths alternate so the parent's grey shows as
/// the spacing around the box inside it. No stroke.
fn directory_grey(dark: bool, depth: usize) -> Color32 {
    let tone = if dark {
        if depth % 2 == 0 {
            68
        } else {
            98
        }
    } else if depth % 2 == 0 {
        214
    } else {
        188
    };
    Color32::from_gray(tone)
}

pub fn hatch_rect(painter: &egui::Painter, rect: Rect, color: Color32) {
    for shape in hatch_shapes(rect, color, 0.45) {
        painter.add(shape);
    }
}

/// Vertical stripes in `color`. `fill` is the opacity of the wash behind them.
///
/// The wash follows the theme color instead of a fixed near-black, so excluded
/// tiles stay visible in light mode without covering labels.
fn hatch_shapes(rect: Rect, color: Color32, fill: f32) -> Vec<egui::Shape> {
    if !rect.is_positive() || !rect.width().is_finite() {
        return Vec::new();
    }
    let mut shapes = vec![egui::Shape::rect_filled(
        rect,
        0.0,
        color.gamma_multiply(fill),
    )];
    let spacing = 6.0;
    let mut x = rect.left();
    while x < rect.right() {
        shapes.push(egui::Shape::line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0, color.gamma_multiply(0.55)),
        ));
        x += spacing;
    }
    shapes
}

pub enum PatternIcon {
    Book,
    Gear { slashed: bool },
}

pub struct PatternEditResult {
    pub submitted: bool,
    pub icon_clicked: bool,
    pub toggled: Option<PatternToggle>,
    pub rect: Rect,
    /// True when the text field took egui keyboard focus this frame (click or Tab).
    pub text_gained_focus: bool,
}

/// One pattern row: a label, a full-width field, match toggles, and an icon
/// sitting inside the field's right edge.
pub fn pattern_edit(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    hint: &str,
    text: &mut String,
    options: &PatternOptions,
    icon: PatternIcon,
    icon_tip: &str,
    want_keyboard_focus: bool,
) -> PatternEditResult {
    ui.label(egui::RichText::new(label).color(Color32::from_gray(204)));
    ui.add_space(4.0);
    // Inset the field so the focus ring clears the panel edge.
    let inset = 4.0;
    let mut result = PatternEditResult {
        submitted: false,
        icon_clicked: false,
        toggled: None,
        rect: Rect::NOTHING,
        text_gained_focus: false,
    };
    ui.horizontal(|ui| {
        ui.add_space(inset);
        let width = (ui.available_width() - inset).max(32.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, 4.0, Color32::from_rgb(60, 60, 60));
        ui.painter().rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, Color32::from_rgb(78, 78, 78)),
            StrokeKind::Inside,
        );

        let icon_w = 26.0;
        let toggle_w = 24.0;
        let toggles_w = toggle_w * 3.0;
        let icon_rect = Rect::from_min_max(
            Pos2::new(rect.right() - icon_w, rect.top()),
            rect.right_bottom(),
        );
        let toggles_rect = Rect::from_min_max(
            Pos2::new(icon_rect.left() - toggles_w, rect.top()),
            Pos2::new(icon_rect.left(), rect.bottom()),
        );
        let text_rect =
            Rect::from_min_max(rect.left_top(), Pos2::new(toggles_rect.left(), rect.bottom()));
        let text_response = ui.put(
            text_rect,
            egui::TextEdit::singleline(text)
                .id_source(id)
                .hint_text(hint)
                .frame(egui::Frame::NONE)
                .margin(egui::Margin::symmetric(8, 4))
                .align(egui::Align2::LEFT_CENTER)
                .desired_width(text_rect.width()),
        );
        if want_keyboard_focus {
            if !text_response.has_focus() {
                text_response.request_focus();
            }
        } else if text_response.has_focus() {
            text_response.surrender_focus();
        }
        let submitted =
            text_response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        let text_gained_focus = text_response.gained_focus() || text_response.clicked();

        let toggled = paint_pattern_toggles(ui, id, toggles_rect, options);
        ui.painter().vline(
            icon_rect.left(),
            icon_rect.y_range().shrink(6.0),
            Stroke::new(1.0, Color32::from_rgb(90, 90, 90)),
        );

        let clickable = matches!(icon, PatternIcon::Gear { .. });
        let icon_response = ui.interact(
            icon_rect,
            ui.scope_id().with((id, "icon")),
            if clickable {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        if icon_response.hovered() {
            icon_response.clone().on_hover_text(icon_tip);
            if clickable {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
        let color = if icon_response.hovered() {
            Color32::from_gray(235)
        } else {
            Color32::from_gray(176)
        };
        match icon {
            PatternIcon::Book => paint_book(ui.painter(), icon_rect, color),
            PatternIcon::Gear { slashed } => paint_gear(ui.painter(), icon_rect, color, slashed),
        }
        result = PatternEditResult {
            submitted,
            icon_clicked: icon_response.clicked(),
            toggled,
            rect,
            text_gained_focus,
        };
    });
    result
}

fn paint_pattern_toggles(
    ui: &mut egui::Ui,
    id: &str,
    rect: Rect,
    options: &PatternOptions,
) -> Option<PatternToggle> {
    let buttons = [
        (PatternToggle::Case, "Aa", false, options.match_case, "Match Case"),
        (
            PatternToggle::Word,
            "ab",
            true,
            options.whole_word,
            "Match Whole Word",
        ),
        (
            PatternToggle::Regex,
            ".*",
            false,
            options.regex,
            "Use Regular Expression",
        ),
    ];
    let mut toggled = None;
    let width = rect.width() / buttons.len() as f32;
    for (index, (kind, label, underline, on, tip)) in buttons.into_iter().enumerate() {
        let button = Rect::from_min_size(
            Pos2::new(rect.left() + width * index as f32, rect.top()),
            Vec2::new(width, rect.height()),
        );
        let response = ui.interact(button, ui.scope_id().with((id, label)), Sense::click());
        let fill = if on {
            Color32::from_rgb(92, 92, 92)
        } else if response.hovered() {
            Color32::from_rgb(74, 74, 74)
        } else {
            Color32::TRANSPARENT
        };
        if fill != Color32::TRANSPARENT {
            ui.painter()
                .rect_filled(button.shrink2(Vec2::new(1.0, 4.0)), 3.0, fill);
        }
        let color = if on || response.hovered() {
            Color32::from_gray(235)
        } else {
            Color32::from_gray(150)
        };
        let text_rect = ui.painter().text(
            button.center(),
            egui::Align2::CENTER_CENTER,
            label,
            FontId::proportional(12.0),
            color,
        );
        if underline {
            ui.painter().hline(
                text_rect.x_range(),
                text_rect.bottom() + 1.0,
                Stroke::new(1.0, color),
            );
        }
        if response.hovered() {
            response.clone().on_hover_text(tip);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if response.clicked() {
            toggled = Some(kind);
        }
    }
    toggled
}

fn paint_book(painter: &egui::Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let stroke = Stroke::new(1.2, color);
    let top = c.y - 5.5;
    let bottom = c.y + 5.0;
    let left = c.x - 7.0;
    let right = c.x + 7.0;
    painter.line_segment([Pos2::new(c.x, top + 1.0), Pos2::new(c.x, bottom)], stroke);
    painter.line_segment(
        [Pos2::new(c.x, top + 1.0), Pos2::new(left, top + 3.0)],
        stroke,
    );
    painter.line_segment(
        [Pos2::new(left, top + 3.0), Pos2::new(left, bottom)],
        stroke,
    );
    painter.line_segment([Pos2::new(left, bottom), Pos2::new(c.x, bottom)], stroke);
    painter.line_segment(
        [Pos2::new(c.x, top + 1.0), Pos2::new(right, top + 3.0)],
        stroke,
    );
    painter.line_segment(
        [Pos2::new(right, top + 3.0), Pos2::new(right, bottom)],
        stroke,
    );
    painter.line_segment([Pos2::new(right, bottom), Pos2::new(c.x, bottom)], stroke);
}

fn paint_gear(painter: &egui::Painter, rect: Rect, color: Color32, slashed: bool) {
    let c = rect.center();
    let stroke = Stroke::new(1.15, color);
    painter.circle_stroke(c, 3.3, stroke);
    painter.circle_stroke(c, 1.35, stroke);
    for i in 0..8 {
        let angle = i as f32 * std::f32::consts::TAU / 8.0;
        let dir = Vec2::new(angle.cos(), angle.sin());
        painter.line_segment([c + dir * 3.1, c + dir * 5.5], stroke);
    }
    if slashed {
        painter.line_segment(
            [c + Vec2::new(-6.0, 6.0), c + Vec2::new(6.0, -6.0)],
            Stroke::new(1.5, color),
        );
    }
}

fn format_size(n: u64) -> String {
    const K: f64 = 1024.0;
    let n = n as f64;
    if n >= K * K * K {
        format!("{:.1} GiB", n / (K * K * K))
    } else if n >= K * K {
        format!("{:.1} MiB", n / (K * K))
    } else if n >= K {
        format!("{:.1} KiB", n / K)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> TreeNode {
        let mut root = TreeNode::new_dir("stories");
        root.children.push(TreeNode::file("one.txt", 12));
        let mut skipped = TreeNode::file("skip.bin", 40);
        skipped.excluded = true;
        root.children.push(skipped);
        let mut nested = TreeNode::new_dir("chapter");
        nested.children.push(TreeNode::file("page.txt", 3));
        root.children.push(nested);
        root.size = 55;
        root
    }

    #[test]
    fn file_types_merge_real_extensions_from_both_trees() {
        let mut source = TreeNode::new_dir("source");
        source.children.push(TreeNode::file("photo.PNG", 10));
        source.children.push(TreeNode::file("notes.txt", 4));
        let mut target = TreeNode::new_dir("target");
        target.children.push(TreeNode::file("other.png", 7));
        target.children.push(TreeNode::file("Makefile", 3));
        let rows = merged_type_rows(&[
            &collect_type_sizes(&source),
            &collect_type_sizes(&target),
        ]);
        assert_eq!(
            rows.iter()
                .map(|(id, bytes)| (id.label(), *bytes))
                .collect::<Vec<_>>(),
            vec![
                (".png".to_string(), 17),
                (".txt".to_string(), 4),
                ("makefile".to_string(), 3),
            ]
        );
        assert_eq!(rows[0].0.color_rgb(), file_color_rgb("photo.png"));
        assert_eq!(rows[2].0.color_rgb(), file_color_rgb("Makefile"));
    }

    #[test]
    fn file_types_stay_empty_on_tree_b_without_a_target() {
        let mut source = HashMap::new();
        source.insert(file_type_id("notes.txt"), 4);
        assert!(type_rows_for_view(&source, None, true).is_empty());
        assert_eq!(type_rows_for_view(&source, None, false).len(), 1);
    }

    #[test]
    fn file_types_omit_files_the_filter_excluded() {
        let rows = merged_type_rows(&[&collect_type_sizes(&sample_tree())]);
        assert_eq!(
            rows.iter()
                .map(|(id, bytes)| (id.label(), *bytes))
                .collect::<Vec<_>>(),
            vec![(".txt".to_string(), 15)]
        );
    }

    fn show_tree(
        ctx: &egui::Context,
        input: egui::RawInput,
        root: &TreeNode,
        selected: &mut Option<Vec<String>>,
        hover: &mut Option<Vec<String>>,
    ) {
        let output = ctx.run_ui(input, |ui| {
            egui::ScrollArea::both().max_height(400.0).show(ui, |ui| {
                tree_view_with_path(
                    ui,
                    root,
                    &["stories".to_string()],
                    selected,
                    hover,
                    Some(&["skip.bin".to_string()]),
                    None,
                );
            });
        });
        output.drop_without_applying_deltas();
    }

    #[test]
    fn directory_tree_selects_and_hovers_a_row() {
        let root = sample_tree();
        let ctx = egui::Context::default();
        let mut selected = None;
        let mut hover = None;
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(360.0, 480.0));
        let base = |time: f64, events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(screen),
            time: Some(time),
            events,
            ..Default::default()
        };

        show_tree(
            &ctx,
            base(0.0, Vec::new()),
            &root,
            &mut selected,
            &mut hover,
        );
        show_tree(
            &ctx,
            base(1.0 / 60.0, Vec::new()),
            &root,
            &mut selected,
            &mut hover,
        );

        let mut saw_hover = false;
        let mut saw_selection = false;
        for (step, y) in [10.0, 30.0, 50.0, 70.0].into_iter().enumerate() {
            let pos = Pos2::new(72.0, y);
            let time = 0.1 + step as f64 * 0.05;
            show_tree(
                &ctx,
                base(time, vec![egui::Event::PointerMoved(pos)]),
                &root,
                &mut selected,
                &mut hover,
            );
            saw_hover |= hover.is_some();
            show_tree(
                &ctx,
                base(
                    time + 0.01,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::default(),
                        },
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed: false,
                            modifiers: egui::Modifiers::default(),
                        },
                    ],
                ),
                &root,
                &mut selected,
                &mut hover,
            );
            saw_selection |= selected
                .as_ref()
                .is_some_and(|path| path.first().map(String::as_str) == Some("stories"));
        }

        assert!(saw_hover, "hovering a row should report a path");
        assert!(
            saw_selection,
            "clicking a row should select a path in the tree, last selection was {selected:?}"
        );
    }

    #[test]
    fn hidden_excluded_files_drop_out_of_the_layout() {
        let mut folder = TreeNode::new_dir("photos");
        folder.children.push(TreeNode::file("keep.jpg", 100));
        let mut junk = TreeNode::file(".DS_Store", 8);
        junk.excluded = true;
        folder.children.push(junk);
        folder.size = 108;

        let hidden = layout_values(&folder.children, false, false);
        assert_eq!(hidden, vec![100.0, 0.0]);
        assert_eq!(layout_bytes(&folder, false), 100);

        let shown = layout_values(&folder.children, false, true);
        assert_eq!(shown, vec![100.0, 8.0]);
        assert_eq!(layout_bytes(&folder, true), 108);
    }

    #[test]
    fn dither_holds_the_top_color_and_stipples_toward_the_bottom() {
        assert_eq!(dither_mix(0.0, 0, DITHER_LEVELS), 0.0);
        assert_eq!(dither_mix(1.0, 15, DITHER_LEVELS), 1.0);
        let early = dither_mix(0.5, 0, 2);
        let late = dither_mix(0.5, 15, 2);
        assert!(
            early > late,
            "a low Bayer threshold takes the darker band first"
        );
        assert_eq!(map_cell_px(400.0), MAP_PIXEL_PX);
        assert_eq!(map_cell_px(1_600.0), MAP_PIXEL_PX);
        assert_eq!(map_cell_px(8_000.0), 10);
    }

    #[test]
    fn every_box_uses_the_same_pixel() {
        let map = Rect::from_min_max(Pos2::ZERO, Pos2::new(400.0, 300.0));
        let grid = map_grid(map, 2.0);
        assert_eq!(grid.cell_px, MAP_PIXEL_PX);
        let fill = Color32::from_rgb(200, 80, 40);
        let big = dither_mesh(
            Rect::from_min_max(Pos2::ZERO, Pos2::new(40.0, 30.0)),
            &grid,
            Some(fill),
            &NO_GLOW,
        )
        .unwrap();
        let small = dither_mesh(
            Rect::from_min_max(Pos2::new(10.0, 4.0), Pos2::new(22.0, 16.0)),
            &grid,
            Some(fill),
            &NO_GLOW,
        )
        .unwrap();
        let pitch = MAP_PIXEL_PX as f32 / grid.pixels_per_point;
        assert_eq!(widest_cell(&big), pitch);
        assert_eq!(widest_cell(&small), pitch);
        for edge in full_cell_edges(&big, grid.pixels_per_point)
            .into_iter()
            .chain(full_cell_edges(&small, grid.pixels_per_point))
        {
            assert_eq!(edge.rem_euclid(MAP_PIXEL_PX), 0);
        }
    }

    #[test]
    fn tile_bottom_is_thirty_percent_darker() {
        let color = Color32::from_rgba_unmultiplied(200, 80, 40, 255);
        let bottom = shade_darker(color, TILE_SHADE);
        assert_eq!(bottom, Color32::from_rgba_unmultiplied(140, 56, 28, 255));
        assert_eq!(shade_darker(color, 1.0), color);
    }

    #[test]
    fn glows_stay_inside_the_tile_rect() {
        let rect = Rect::from_min_max(Pos2::new(10.0, 20.0), Pos2::new(50.0, 80.0));
        let mesh = dither_mesh(
            rect,
            &aligned_grid(rect, 2.0, MAP_PIXEL_PX),
            Some(Color32::from_rgb(20, 40, 60)),
            &DitherGlows {
                outline: Some(Color32::from_rgb(90, 220, 130)),
                hover: Some(Color32::from_rgb(20, 40, 60)),
            },
        )
        .unwrap();
        let bounds = mesh.calc_bounds();
        assert!(bounds.min.x >= rect.left() - 0.51);
        assert!(bounds.min.y >= rect.top() - 0.51);
        assert!(bounds.max.x <= rect.right() + 0.51);
        assert!(bounds.max.y <= rect.bottom() + 0.51);
    }

    #[test]
    fn outline_and_hover_recolor_bayer_cells() {
        assert!(dither_lit(HOVER_DITHER, 0));
        assert!(dither_lit(HOVER_DITHER, 3));
        assert!(!dither_lit(HOVER_DITHER, 4));
        assert!(!dither_lit(0.0, 0));

        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(40.0, 40.0));
        let fill = Color32::from_rgb(200, 80, 40);
        let grid = aligned_grid(rect, 1.0, 2);
        let plain = dither_mesh(rect, &grid, Some(fill), &NO_GLOW).unwrap();
        let hovered = dither_mesh(
            rect,
            &grid,
            Some(fill),
            &DitherGlows {
                outline: None,
                hover: Some(fill),
            },
        )
        .unwrap();
        let bright = shade_brighter(fill, HOVER_BRIGHT);
        assert_ne!(bright, MATCH_GREEN);
        assert_eq!(color_at(&hovered, 20.0, 20.0), bright);
        assert_eq!(color_at(&hovered, 6.0, 16.0), color_at(&plain, 6.0, 16.0));
        assert_ne!(color_at(&hovered, 6.0, 16.0), bright);
        let period = Rect::from_min_max(Pos2::ZERO, Pos2::new(8.0, 8.0));
        let hovered_period = dither_mesh(
            period,
            &aligned_grid(period, 1.0, 2),
            Some(fill),
            &DitherGlows {
                outline: None,
                hover: Some(fill),
            },
        )
        .unwrap();
        let bright_quads = hovered_period
            .vertices
            .chunks(4)
            .filter(|quad| quad[0].color == bright)
            .count();
        assert_eq!(bright_quads, 4);
    }

    #[test]
    fn match_outlines_share_one_thickness() {
        let grid = MapGrid {
            pixels_per_point: 2.0,
            cell_px: MAP_PIXEL_PX,
            origin_x: 0,
            origin_y: 0,
        };
        let aligned = outline_frame(Rect::from_min_max(Pos2::ZERO, Pos2::new(40.0, 30.0)), &grid);
        let offset = outline_frame(
            Rect::from_min_max(Pos2::new(1.3, 2.2), Pos2::new(33.7, 28.4)),
            &grid,
        );
        for bars in [&aligned, &offset] {
            assert!(bars.len() >= 4);
            assert_eq!(
                span_px(bars[0].height(), grid.pixels_per_point),
                MAP_PIXEL_PX
            );
            assert_eq!(
                span_px(bars[1].height(), grid.pixels_per_point),
                MAP_PIXEL_PX
            );
            assert_eq!(
                span_px(bars[2].width(), grid.pixels_per_point),
                MAP_PIXEL_PX
            );
            assert_eq!(
                span_px(bars[3].width(), grid.pixels_per_point),
                MAP_PIXEL_PX
            );
        }
    }

    #[test]
    fn directory_frames_alternate_grey_without_depending_on_glow() {
        assert_ne!(directory_grey(true, 0), directory_grey(true, 1));
        assert_eq!(directory_grey(true, 0), directory_grey(true, 2));
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(40.0, 30.0));
        let inner = rect.shrink(DIR_PAD);
        assert_eq!(inner.left(), rect.left() + DIR_PAD);
        assert_eq!(inner.top(), rect.top() + DIR_PAD);
        // The pad is the frame. The tile inset must not be what produces it.
        assert_ne!(DIR_PAD, TILE_INSET);
    }

    fn span_px(span: f32, pixels_per_point: f32) -> i32 {
        (span * pixels_per_point).round() as i32
    }

    fn color_at(mesh: &Mesh, x: f32, y: f32) -> Color32 {
        mesh.vertices
            .chunks(4)
            .find(|quad| (quad[0].pos.x - x).abs() < 0.1 && (quad[0].pos.y - y).abs() < 0.1)
            .unwrap_or_else(|| panic!("no cell at ({x}, {y})"))[0]
            .color
    }

    fn aligned_grid(rect: Rect, pixels_per_point: f32, cell_px: i32) -> MapGrid {
        MapGrid {
            pixels_per_point,
            cell_px,
            origin_x: (rect.left() * pixels_per_point).round() as i32,
            origin_y: (rect.top() * pixels_per_point).round() as i32,
        }
    }

    fn widest_cell(mesh: &Mesh) -> f32 {
        mesh.vertices
            .chunks(4)
            .map(|quad| quad[1].pos.x - quad[0].pos.x)
            .fold(0.0, f32::max)
    }

    fn full_cell_edges(mesh: &Mesh, pixels_per_point: f32) -> Vec<i32> {
        let pitch = MAP_PIXEL_PX as f32 / pixels_per_point;
        mesh.vertices
            .chunks(4)
            .filter(|quad| (quad[1].pos.x - quad[0].pos.x - pitch).abs() < 0.01)
            .map(|quad| (quad[0].pos.x * pixels_per_point).round() as i32)
            .collect()
    }
}
