//! Tree, treemap, and legend widgets.

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use rsyncstat_model::{file_type_bucket, TreeNode};
use rsyncstat_treemap::{squarify, LayoutItem};
use std::collections::HashMap;
use std::path::Path;

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
) {
    let size_text = if node.excluded {
        "(excluded)".to_string()
    } else {
        format_size(node.size)
    };
    let suffix = if node.is_dir { "/" } else { "" };
    let label = format!("{}{suffix}  {size_text}", node.name);

    let response = if node.is_dir && !node.children.is_empty() {
        ui.collapsing(label, |ui| {
            for child in &node.children {
                let mut child_path = path.to_vec();
                child_path.push(child.name.clone());
                tree_view_with_path(ui, child, &child_path, selected, hover);
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
    }
}

pub fn type_legend(ui: &mut egui::Ui, root: &TreeNode) {
    let mut buckets: HashMap<&'static str, u64> = HashMap::new();
    collect_buckets(root, &mut buckets);
    let mut items: Vec<_> = buckets.into_iter().collect();
    items.sort_by(|a, b| b.1.cmp(&a.1));
    for (name, bytes) in items {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(12.0, 12.0), Sense::hover());
            ui.painter().rect_filled(rect, 2.0, type_color(name));
            ui.label(format!("{name}: {}", format_size(bytes)));
        });
    }
}

fn collect_buckets(node: &TreeNode, out: &mut HashMap<&'static str, u64>) {
    if node.excluded {
        return;
    }
    if node.is_dir {
        for c in &node.children {
            collect_buckets(c, out);
        }
    } else {
        let b = file_type_bucket(&node.name);
        *out.entry(b).or_default() += node.size;
    }
}

#[derive(Clone)]
struct Tile {
    path: Vec<String>,
    size: u64,
    excluded: bool,
    is_dir: bool,
    name: String,
    children: Vec<Tile>,
}

impl Tile {
    fn from_node(node: &TreeNode, path: Vec<String>) -> Self {
        let children = node
            .children
            .iter()
            .map(|c| {
                let mut p = path.clone();
                p.push(c.name.clone());
                Tile::from_node(c, p)
            })
            .collect();
        Self {
            name: node.name.clone(),
            size: node.size,
            excluded: node.excluded,
            is_dir: node.is_dir,
            path,
            children,
        }
    }
}

pub fn treemap_view(
    ui: &mut egui::Ui,
    root: &TreeNode,
    selected: &mut Option<Vec<String>>,
    hover: &mut Option<Vec<String>>,
    _fs_root: &Path,
) {
    let available = ui.available_size();
    let (response, painter) = ui.allocate_painter(available, Sense::click_and_drag());
    let rect = response.rect;

    let top_items: Vec<Tile> = root
        .children
        .iter()
        .map(|c| Tile::from_node(c, vec![root.name.clone(), c.name.clone()]))
        .collect();

    let top_layout = squarify(
        &top_items,
        |t| t.size as f64,
        rect.left(),
        rect.top(),
        rect.width(),
        rect.height(),
    );

    let pointer = ui.input(|i| i.pointer.hover_pos());
    let mut hovered: Option<Vec<String>> = None;

    for item in &top_layout {
        draw_folder_tile(ui, &painter, item, selected, &mut hovered, response.id);
    }

    if let Some(pos) = pointer {
        if rect.contains(pos) {
            *hover = hovered.clone();
        }
    }
}

fn draw_folder_tile(
    ui: &egui::Ui,
    painter: &egui::Painter,
    item: &LayoutItem<Tile>,
    selected: &mut Option<Vec<String>>,
    hover: &mut Option<Vec<String>>,
    parent_id: egui::Id,
) {
    let r = egui::Rect::from_min_size(
        Pos2::new(item.rect.x, item.rect.y),
        Vec2::new(item.rect.w, item.rect.h),
    );
    let node = &item.item;

    if r.width() < 2.0 || r.height() < 2.0 {
        return;
    }

    let border = ui.visuals().window_stroke;
    painter.rect_stroke(r.expand(-1.0), 3.0, border, StrokeKind::Inside);

    let header_h = 16.0f32.min(r.height() * 0.25);
    painter.text(
        r.min + Vec2::new(4.0, 12.0),
        egui::Align2::LEFT_TOP,
        format!("{}/", node.name),
        egui::FontId::proportional(11.0),
        ui.visuals().weak_text_color(),
    );

    let inner = Rect::from_min_max(
        Pos2::new(r.min.x + 2.0, r.min.y + header_h),
        Pos2::new(r.max.x - 2.0, r.max.y - 2.0),
    );

    if node.excluded {
        hatch_rect(painter, r, ui.visuals().weak_text_color());
        if r.width() > 40.0 && r.height() > 24.0 {
            painter.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                "excluded",
                egui::FontId::proportional(10.0),
                ui.visuals().weak_text_color(),
            );
        }
    } else if !node.children.is_empty() && inner.is_positive() {
        let kid_layout = squarify(
            &node.children,
            |t| t.size as f64,
            inner.left(),
            inner.top(),
            inner.width(),
            inner.height(),
        );
        for kid in kid_layout {
            draw_leaf_or_nested(ui, painter, &kid, selected, hover, parent_id);
        }
    } else if !node.is_dir {
        fill_file_tile(ui, painter, inner, &node.name);
    }

    let id = parent_id.with(node.path.join("/"));
    let interact = ui.interact(r, id, Sense::click());
    if interact.hovered() {
        *hover = Some(node.path.clone());
    }
    if interact.clicked() {
        *selected = Some(node.path.clone());
    }
}

fn draw_leaf_or_nested(
    ui: &egui::Ui,
    painter: &egui::Painter,
    item: &LayoutItem<Tile>,
    selected: &mut Option<Vec<String>>,
    hover: &mut Option<Vec<String>>,
    parent_id: egui::Id,
) {
    let r = egui::Rect::from_min_size(
        Pos2::new(item.rect.x, item.rect.y),
        Vec2::new(item.rect.w, item.rect.h),
    );
    let node = &item.item;

    if r.width() < 2.0 || r.height() < 2.0 {
        return;
    }

    if node.excluded {
        hatch_rect(painter, r, ui.visuals().weak_text_color());
    } else if node.is_dir && !node.children.is_empty() {
        draw_folder_tile(ui, painter, item, selected, hover, parent_id);
        return;
    } else if node.is_dir {
        painter.rect_filled(r.shrink(1.0), 2.0, Color32::from_gray(60));
    } else {
        fill_file_tile(ui, painter, r, &node.name);
    }

    let id = parent_id.with(node.path.join("/"));
    let interact = ui.interact(r, id, Sense::click());
    if interact.hovered() {
        *hover = Some(node.path.clone());
    }
    if interact.clicked() {
        *selected = Some(node.path.clone());
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
