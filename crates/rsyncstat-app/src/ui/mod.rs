//! Tree, treemap, legend, and status widgets.
//!
//! Painting functions take the egui context, geometry and state as separate
//! arguments; bundling them would only add a struct per call site.

#![allow(clippy::too_many_arguments)]

use crate::volume::SpaceTile;
use eframe::egui::{self, Color32, FontId, Mesh, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use egui_ltreeview::{Action, NodeBuilder, RowLayout, TreeView, TreeViewState};
use rsyncstat_filter::{NamedPreset, PatternOptions, PatternToggle};
use rsyncstat_model::{
    elide_path_for_width, file_color_rgb, file_type_id, readable_color_rgb, FileTypeId, TreeNode,
};
use rsyncstat_plan::{self, MatchKind};
use rsyncstat_treemap::squarify_indices;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

mod diff;
mod dither;
mod legend;
mod patterns;
mod tree_view;
mod treemap;

pub use diff::*;
pub use dither::*;
pub use legend::*;
pub use patterns::*;
pub use tree_view::*;
pub use treemap::*;

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

fn text_width(ui: &egui::Ui, text: &str, font_id: &FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font_id.clone(), Color32::PLACEHOLDER)
        .size()
        .x
}

fn format_size(n: u64) -> String {
    rsyncstat_model::format_bytes(n, 1)
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
        let rows = merged_type_rows(&[&collect_type_sizes(&source), &collect_type_sizes(&target)]);
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
