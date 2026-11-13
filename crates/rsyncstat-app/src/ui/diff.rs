//! Size-difference and free-space views.

use super::*;

pub fn size_diff_view(ui: &mut egui::Ui, diff: &rsyncstat_plan::DirectoryDiff) {
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
            .map(rsyncstat_plan::DirDelta::a_minus_b)
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

pub(super) fn on_side(delta: i128) -> String {
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

pub(super) fn paint_space_tile(
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

pub(super) fn space_caption(tile: SpaceTile) -> String {
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
