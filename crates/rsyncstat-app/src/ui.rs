//! Tree, treemap, legend, and status widgets.

use crate::sync::{self, MatchKind};
use crate::volume::SpaceTile;
use eframe::egui::{
    self, Color32, Direction, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Vec2,
};
use egui_ltreeview::{Action, NodeBuilder, RowLayout, TreeView, TreeViewState};
use rsyncstat_model::{
    elide_path_for_width, file_color_rgb, file_type_bucket, readable_color_rgb, TreeNode,
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
/// Match glow reaches this far in from the tile edge, then fades out.
const MATCH_GLOW_PX: f32 = 3.0;
/// Bottom edge of each tile is this fraction of the top color, so the box
/// shades 30% darker toward its own bottom edge.
const TILE_SHADE: f32 = 0.7;
/// Hover partner glow, drawn from the center of the box. Quieter than the match rim.
const HOVER_GLOW: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 88);

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

pub fn collect_type_sizes(root: &TreeNode) -> HashMap<&'static str, u64> {
    let mut buckets = HashMap::new();
    collect_buckets(root, &mut buckets);
    buckets
}

pub fn type_legend(ui: &mut egui::Ui, buckets: &HashMap<&'static str, u64>) {
    let mut items: Vec<_> = buckets
        .iter()
        .map(|(name, bytes)| (*name, *bytes))
        .collect();
    items.sort_by(|a, b| b.1.cmp(&a.1));
    for (name, bytes) in items {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(12.0, 12.0), Sense::hover());
            ui.painter()
                .rect_filled(rect, 0.0, color_from_rgb(readable_color_rgb(name)));
            ui.label(format!("{name}: {}", format_size(bytes)));
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

fn collect_buckets(node: &TreeNode, out: &mut HashMap<&'static str, u64>) {
    if node.is_dir {
        if node.excluded {
            return;
        }
        for child in &node.children {
            collect_buckets(child, out);
        }
    } else {
        let bucket = file_type_bucket(&node.name);
        *out.entry(bucket).or_default() += node.size;
    }
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
    paint_children(
        ui,
        &painter,
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
        );
        path.pop();
    }
}

fn paint_node(
    ui: &egui::Ui,
    painter: &egui::Painter,
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
) {
    if rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }

    let too_wide = live && node.children.len() > 400;
    let nest =
        node.is_dir && !node.excluded && !node.children.is_empty() && levels > 1 && !too_wide;
    let rel = rel_within_root(path).join("/");
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
        );
        if inner.is_positive() {
            paint_children(
                ui,
                painter,
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
            );
        }
    } else if node.is_dir {
        paint_tile_gradient(
            painter,
            tile,
            color_from_rgb(readable_color_rgb(&node.name)).gamma_multiply(0.85),
        );
    } else {
        paint_tile_gradient(painter, tile, color_from_rgb(file_color_rgb(&node.name)));
        if let Some(kind) = mark {
            if let Some(mesh) = inset_glow_mesh(tile, match_color(kind), MATCH_GLOW_PX) {
                painter.add(mesh);
            }
        }
    }

    let glow_rect = if nest { rect } else { tile };
    maybe_partner_glow(
        painter,
        glow_rect,
        partner_highlight_rel,
        rel_within_root(path),
    );
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
        );
    }
}

fn maybe_partner_glow(
    painter: &egui::Painter,
    rect: Rect,
    partner_highlight_rel: Option<&[String]>,
    this_rel: Vec<String>,
) {
    if partner_highlight_rel == Some(this_rel.as_slice()) {
        if let Some(mesh) = radial_glow_mesh(rect, HOVER_GLOW) {
            painter.add(mesh);
        }
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
            paint_tile_gradient(painter, rect.shrink(TILE_INSET), FREE_COLOR);
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
        MatchKind::Same => Color32::from_rgb(90, 220, 130),
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
) -> bool {
    let id = parent_id.with(path.join("/"));
    let interact = ui.interact(rect, id, Sense::click());
    if interact.hovered() {
        *hover = Some(path.to_vec());
        *space_hover = None;
        interact
            .clone()
            .on_hover_text(hover_tip(rel, node, mark, other_compared));
    }
    if interact.clicked() {
        *selected = Some(path.to_vec());
    }
    interact.hovered()
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

/// Each box shades from its own color at the top edge to 30% darker at the
/// bottom edge. The fade belongs to that rectangle, so neighboring boxes
/// do not share one gradient.
fn paint_tile_gradient(painter: &egui::Painter, rect: Rect, color: Color32) {
    if !rect.is_positive() {
        return;
    }
    painter.add(Shape::gradient_rect(
        rect,
        Direction::TopDown,
        [color, shade_darker(color, TILE_SHADE)],
    ));
}

/// Keep hue and alpha. Multiply the color channels so the result is darker.
fn shade_darker(color: Color32, factor: f32) -> Color32 {
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

/// Green (or amber) rim: solid at the outer edge, gone `depth` pixels inward.
/// Every vertex stays on or inside `rect`.
fn inset_glow_mesh(rect: Rect, color: Color32, depth: f32) -> Option<Mesh> {
    if !rect.is_positive() || depth <= 0.0 {
        return None;
    }
    let depth = depth.min(rect.width() * 0.5).min(rect.height() * 0.5);
    let inner = rect.shrink(depth);
    if !inner.is_positive() {
        return None;
    }
    let outer = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    let inner_pts = [
        inner.left_top(),
        inner.right_top(),
        inner.right_bottom(),
        inner.left_bottom(),
    ];
    let mut mesh = Mesh::default();
    for pos in outer {
        mesh.colored_vertex(pos, color);
    }
    for pos in inner_pts {
        mesh.colored_vertex(pos, Color32::TRANSPARENT);
    }
    for k in 0..4 {
        let o0 = k as u32;
        let o1 = ((k + 1) % 4) as u32;
        let i0 = 4 + k as u32;
        let i1 = 4 + ((k + 1) % 4) as u32;
        mesh.add_triangle(o0, o1, i1);
        mesh.add_triangle(o0, i1, i0);
    }
    Some(mesh)
}

/// Soft glow peaked at the center and gone at the box edge.
fn radial_glow_mesh(rect: Rect, color: Color32) -> Option<Mesh> {
    if !rect.is_positive() {
        return None;
    }
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.center(), color);
    let steps = 4;
    let rim = perimeter(rect, steps);
    for pos in &rim {
        mesh.colored_vertex(*pos, Color32::TRANSPARENT);
    }
    let count = rim.len() as u32;
    for i in 0..count {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % count);
    }
    Some(mesh)
}

fn perimeter(rect: Rect, steps_per_side: usize) -> Vec<Pos2> {
    let mut pts = Vec::with_capacity(steps_per_side * 4);
    let t_at = |i: usize| i as f32 / steps_per_side as f32;
    for i in 0..steps_per_side {
        let t = t_at(i);
        pts.push(Pos2::new(rect.left() + rect.width() * t, rect.top()));
    }
    for i in 0..steps_per_side {
        let t = t_at(i);
        pts.push(Pos2::new(rect.right(), rect.top() + rect.height() * t));
    }
    for i in 0..steps_per_side {
        let t = t_at(i);
        pts.push(Pos2::new(rect.right() - rect.width() * t, rect.bottom()));
    }
    for i in 0..steps_per_side {
        let t = t_at(i);
        pts.push(Pos2::new(rect.left(), rect.bottom() - rect.height() * t));
    }
    pts
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
    pub rect: Rect,
}

/// One VS Code-style pattern row: a label, a full-width field, and an icon
/// sitting inside the field's right edge.
pub fn pattern_edit(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    hint: &str,
    text: &mut String,
    icon: PatternIcon,
    icon_tip: &str,
) -> PatternEditResult {
    ui.label(egui::RichText::new(label).color(Color32::from_gray(204)));
    ui.add_space(3.0);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 28.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, 4.0, Color32::from_rgb(60, 60, 60));
    ui.painter().rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, Color32::from_rgb(78, 78, 78)),
        StrokeKind::Inside,
    );

    let icon_w = 28.0;
    let text_rect = Rect::from_min_max(
        rect.left_top(),
        Pos2::new(rect.right() - icon_w, rect.bottom()),
    );
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
    let submitted =
        text_response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));

    let icon_rect = Rect::from_min_max(
        Pos2::new(rect.right() - icon_w, rect.top()),
        rect.right_bottom(),
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
    PatternEditResult {
        submitted,
        icon_clicked: icon_response.clicked(),
        rect,
    }
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
    fn tile_bottom_is_thirty_percent_darker() {
        let color = Color32::from_rgba_unmultiplied(200, 80, 40, 255);
        let bottom = shade_darker(color, TILE_SHADE);
        assert_eq!(bottom, Color32::from_rgba_unmultiplied(140, 56, 28, 255));
        assert_eq!(shade_darker(color, 1.0), color);
    }

    #[test]
    fn glows_stay_inside_the_tile_rect() {
        let rect = Rect::from_min_max(Pos2::new(10.0, 20.0), Pos2::new(50.0, 80.0));
        let edge = inset_glow_mesh(rect, Color32::from_rgb(90, 220, 130), MATCH_GLOW_PX).unwrap();
        let bounds = edge.calc_bounds();
        assert_eq!(bounds.min, rect.min);
        assert_eq!(bounds.max, rect.max);
        let inner = rect.shrink(MATCH_GLOW_PX);
        let inner_corners = [
            inner.left_top(),
            inner.right_top(),
            inner.right_bottom(),
            inner.left_bottom(),
        ];
        for (vertex, corner) in edge.vertices.iter().skip(4).zip(inner_corners) {
            assert_eq!(vertex.pos, corner);
        }

        let hover = radial_glow_mesh(rect, HOVER_GLOW).unwrap();
        let hover_bounds = hover.calc_bounds();
        assert!(hover_bounds.min.x >= rect.left() - 0.01);
        assert!(hover_bounds.min.y >= rect.top() - 0.01);
        assert!(hover_bounds.max.x <= rect.right() + 0.01);
        assert!(hover_bounds.max.y <= rect.bottom() + 0.01);
    }

    #[test]
    fn directory_frames_alternate_grey_without_depending_on_glow() {
        assert_ne!(directory_grey(true, 0), directory_grey(true, 1));
        assert_eq!(directory_grey(true, 0), directory_grey(true, 2));
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(40.0, 30.0));
        let inner = rect.shrink(DIR_PAD);
        assert_eq!(inner.left(), rect.left() + DIR_PAD);
        assert_eq!(inner.top(), rect.top() + DIR_PAD);
        // The pad is the frame. A glow must not be what produces it.
        assert_ne!(DIR_PAD, MATCH_GLOW_PX);
    }
}
