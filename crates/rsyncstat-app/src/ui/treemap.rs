//! The treemap: layout, tiles and hover.

use super::*;

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

pub(super) fn layout_values(children: &[TreeNode], live: bool, show_excluded: bool) -> Vec<f64> {
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
pub(super) fn layout_bytes(node: &TreeNode, show_excluded: bool) -> u64 {
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

pub(super) fn hidden_excluded_bytes(node: &TreeNode) -> u64 {
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

pub(super) fn paint_children(
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

pub(super) fn paint_node(
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
        other_files.and_then(|files| rsyncstat_plan::match_file(&rel, node.size, files))
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

pub(super) fn match_color(kind: MatchKind) -> Color32 {
    match kind {
        MatchKind::Same => MATCH_GREEN,
        MatchKind::Different => Color32::from_rgb(240, 170, 60),
    }
}

pub(super) fn track_pointer(
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

pub(super) fn show_tile_tip(response: &egui::Response, text: impl Into<egui::WidgetText>) {
    let mut tip = egui::Tooltip::for_enabled(response);
    tip.popup = tip.popup.align(egui::RectAlign::TOP);
    tip.show(|ui| {
        ui.set_max_width(ui.spacing().tooltip_width);
        ui.label(text);
    });
}

pub(super) fn hover_tip(
    rel: &str,
    node: &TreeNode,
    mark: Option<MatchKind>,
    other_compared: bool,
) -> String {
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
