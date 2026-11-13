//! File-type legend rows.

use super::*;

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

pub(super) fn collect_buckets(node: &TreeNode, out: &mut HashMap<FileTypeId, u64>) {
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
