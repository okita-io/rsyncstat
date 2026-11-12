//! Tree, treemap, legend, and status widgets.

use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use rsyncstat_model::{elide_path_for_width, file_type_bucket, TreeNode};
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
    /// Relative path highlighted because the user hovered the matching entry in the other pane.
    pub partner_highlight_rel: Option<Vec<String>>,
}

const TYPE_COLORS: &[Color32] = &[
    Color32::from_rgb(80, 120, 200),
    Color32::from_rgb(90, 170, 110),
    Color32::from_rgb(200, 130, 70),
    Color32::from_rgb(170, 90, 170),
    Color32::from_rgb(200, 190, 80),
    Color32::from_rgb(120, 180, 200),
    Color32::from_rgb(200, 100, 100),
    Color32::from_rgb(140, 140, 140),
];

fn type_color(bucket: &str) -> Color32 {
    let h = bucket
        .bytes()
        .fold(0usize, |acc, b| acc.wrapping_add(b as usize));
    TYPE_COLORS[h % TYPE_COLORS.len()]
}

pub fn tree_view_with_path(
    ui: &mut egui::Ui,
    node: &TreeNode,
    path: &[String],
    selected: &mut Option<Vec<String>>,
    hover: &mut Option<Vec<String>>,
    partner_highlight_rel: Option<&[String]>,
) {
    let size_text = if node.excluded {
        "(excluded)".to_string()
    } else {
        format_size(node.size)
    };
    let suffix = if node.is_dir { "/" } else { "" };
    let is_partner = partner_highlight_rel.map(|p| p == rel_within_root(path).as_slice());
    let label = format!("{}{suffix}  {size_text}", node.name);

    let response = if node.is_dir && !node.children.is_empty() {
        ui.collapsing(label, |ui| {
            for child in &node.children {
                let mut child_path = path.to_vec();
                child_path.push(child.name.clone());
                tree_view_with_path(
                    ui,
                    child,
                    &child_path,
                    selected,
                    hover,
                    partner_highlight_rel,
                );
            }
        })
        .header_response
    } else {
        ui.selectable_label(selected.as_ref().map(|p| p == path).unwrap_or(false), label)
    };

    if response.hovered() {
        *hover = Some(path.to_vec());
    }
    if response.clicked() {
        *selected = Some(path.to_vec());
    }
    if node.excluded {
        hatch_rect(ui.painter(), response.rect, ui.visuals().weak_text_color());
    } else if is_partner == Some(true) {
        ui.painter().rect_stroke(
            response.rect.expand(1.0),
            2.0,
            Stroke::new(1.5, Color32::from_rgb(255, 210, 80)),
            StrokeKind::Outside,
        );
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
            ui.painter().rect_filled(rect, 2.0, type_color(name));
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
) {
    let available = ui.available_size();
    let (response, painter) = ui.allocate_painter(available, Sense::click_and_drag());
    let rect = response.rect;
    if rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }
    // While the walk is running, draw two levels so the map can refresh many
    // times a second. The finished scan paints the full nesting.
    let levels = if live { 2 } else { 32 };
    let mut path = vec![root.name.clone()];
    let partner = ctx.partner_highlight_rel.as_deref();
    paint_children(
        ui,
        &painter,
        root,
        &mut path,
        rect,
        ctx.selected,
        ctx.hover,
        pane_id.with(response.id),
        levels,
        live,
        partner,
    );
}

fn layout_values(children: &[TreeNode], live: bool) -> Vec<f64> {
    let mut values: Vec<f64> = children.iter().map(|child| child.size as f64).collect();
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

fn paint_children(
    ui: &egui::Ui,
    painter: &egui::Painter,
    parent: &TreeNode,
    path: &mut Vec<String>,
    rect: Rect,
    selected: &mut Option<Vec<String>>,
    hover: &mut Option<Vec<String>>,
    parent_id: egui::Id,
    levels: usize,
    live: bool,
    partner_highlight_rel: Option<&[String]>,
) {
    let values = layout_values(&parent.children, live);
    let layout = squarify_indices(
        &values,
        rect.left(),
        rect.top(),
        rect.width(),
        rect.height(),
    );
    for item in layout {
        let child = &parent.children[item.item];
        let tile = Rect::from_min_size(
            Pos2::new(item.rect.x, item.rect.y),
            Vec2::new(item.rect.w, item.rect.h),
        );
        path.push(child.name.clone());
        paint_node(
            ui,
            painter,
            child,
            path,
            tile,
            selected,
            hover,
            parent_id,
            levels,
            live,
            partner_highlight_rel,
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
    parent_id: egui::Id,
    levels: usize,
    live: bool,
    partner_highlight_rel: Option<&[String]>,
) {
    if rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }

    let too_wide = live && node.children.len() > 400;
    let nest =
        node.is_dir && !node.excluded && !node.children.is_empty() && levels > 1 && !too_wide;

    if node.excluded {
        hatch_rect(painter, rect, ui.visuals().weak_text_color());
        let title = if node.is_dir {
            format!("{}/", node.name)
        } else {
            node.name.clone()
        };
        label_tile(ui, painter, rect, &title, "excluded");
    } else if nest {
        let border = ui.visuals().window_stroke;
        painter.rect_stroke(rect.expand(-1.0), 3.0, border, StrokeKind::Inside);
        let header_h = 16.0f32.min(rect.height() * 0.25);
        painter.text(
            rect.min + Vec2::new(4.0, 2.0),
            egui::Align2::LEFT_TOP,
            format!("{}/", node.name),
            egui::FontId::proportional(11.0),
            ui.visuals().weak_text_color(),
        );
        if rect.width() > 96.0 {
            painter.text(
                Pos2::new(rect.max.x - 4.0, rect.min.y + 2.0),
                egui::Align2::RIGHT_TOP,
                format_size(node.size),
                egui::FontId::proportional(10.0),
                ui.visuals().weak_text_color(),
            );
        }
        let inner = Rect::from_min_max(
            Pos2::new(rect.min.x + 2.0, rect.min.y + header_h),
            Pos2::new(rect.max.x - 2.0, rect.max.y - 2.0),
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
                parent_id,
                levels - 1,
                live,
                partner_highlight_rel,
            );
        }
    } else if node.is_dir {
        painter.rect_filled(
            rect.shrink(1.0),
            2.0,
            type_color(&node.name).gamma_multiply(0.85),
        );
        label_tile(
            ui,
            painter,
            rect,
            &format!("{}/", node.name),
            &format_size(node.size),
        );
    } else {
        fill_file_tile(ui, painter, rect, &node.name);
    }

    maybe_partner_stroke(
        painter,
        rect,
        partner_highlight_rel,
        rel_within_root(path),
    );
    track_pointer(ui, rect, path, parent_id, selected, hover);
}

fn maybe_partner_stroke(
    painter: &egui::Painter,
    r: Rect,
    partner_highlight_rel: Option<&[String]>,
    this_rel: Vec<String>,
) {
    if partner_highlight_rel == Some(this_rel.as_slice()) {
        painter.rect_stroke(
            r.shrink(0.5),
            2.0,
            Stroke::new(2.0, Color32::from_rgb(255, 210, 80)),
            StrokeKind::Outside,
        );
    }
}

fn label_tile(ui: &egui::Ui, painter: &egui::Painter, rect: Rect, title: &str, detail: &str) {
    if rect.width() > 48.0 && rect.height() > 18.0 {
        painter.text(
            rect.min + Vec2::new(4.0, 2.0),
            egui::Align2::LEFT_TOP,
            title,
            egui::FontId::proportional(11.0),
            ui.visuals().text_color(),
        );
    }
    if rect.width() > 48.0 && rect.height() > 32.0 {
        painter.text(
            rect.min + Vec2::new(4.0, 16.0),
            egui::Align2::LEFT_TOP,
            detail,
            egui::FontId::proportional(10.0),
            ui.visuals().weak_text_color(),
        );
    }
}

fn track_pointer(
    ui: &egui::Ui,
    rect: Rect,
    path: &[String],
    parent_id: egui::Id,
    selected: &mut Option<Vec<String>>,
    hover: &mut Option<Vec<String>>,
) {
    let id = parent_id.with(path.join("/"));
    let interact = ui.interact(rect, id, Sense::click());
    if interact.hovered() {
        *hover = Some(path.to_vec());
    }
    if interact.clicked() {
        *selected = Some(path.to_vec());
    }
}

fn fill_file_tile(ui: &egui::Ui, painter: &egui::Painter, r: Rect, name: &str) {
    let color = type_color(file_type_bucket(name));
    painter.rect_filled(r.shrink(1.0), 3.0, color);
    if r.width() > 56.0 && r.height() > 20.0 {
        painter.text(
            r.min + Vec2::new(4.0, 4.0),
            egui::Align2::LEFT_TOP,
            name,
            egui::FontId::proportional(11.0),
            ui.visuals().text_color(),
        );
    }
}

pub fn hatch_rect(painter: &egui::Painter, rect: Rect, color: Color32) {
    painter.rect_filled(rect, 0.0, Color32::from_gray(35));
    let spacing = 6.0;
    let mut x = rect.left();
    while x < rect.right() {
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0, color.gamma_multiply(0.6)),
        );
        x += spacing;
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
