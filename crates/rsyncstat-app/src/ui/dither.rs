//! Dithered tile fills, outlines and hatching on a shared pixel grid.

use super::*;

/// Ordered-dither steps between the tile color and a 30% darker shade.
pub(super) const DITHER_LEVELS: u8 = 5;

/// One rendered pixel is this many device pixels in every box.
pub(super) const MAP_PIXEL_PX: i32 = 6;

/// On a large pane the pixel grows so the shorter side stays near this many
/// rendered pixels, stretched to the pane.
pub(super) const MAP_SHORT_SIDE_PX: f32 = 800.0;

/// Shared pixel lattice for one file map. Every box steps by `cell_px`.
pub(super) struct MapGrid {
    pub(super) pixels_per_point: f32,
    pub(super) cell_px: i32,
    pub(super) origin_x: i32,
    pub(super) origin_y: i32,
}

pub(super) fn map_grid(rect: Rect, pixels_per_point: f32) -> MapGrid {
    let pixels_per_point = pixels_per_point.max(1.0);
    let short_side = rect.width().min(rect.height()) * pixels_per_point;
    MapGrid {
        pixels_per_point,
        cell_px: map_cell_px(short_side),
        origin_x: (rect.left() * pixels_per_point).round() as i32,
        origin_y: (rect.top() * pixels_per_point).round() as i32,
    }
}

pub(super) fn map_cell_px(short_side_px: f32) -> i32 {
    let stretched = (short_side_px / MAP_SHORT_SIDE_PX).round().max(1.0) as i32;
    stretched.max(MAP_PIXEL_PX)
}

/// 4×4 Bayer matrix. A cell uses the high color when its mix clears this threshold.
pub(super) const BAYER4: [[u8; 4]; 4] =
    [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Match outline is a solid edge. Hover is the box color, drawn on the same
/// quarter of the Bayer cells.
pub(super) struct DitherGlows {
    pub(super) outline: Option<Color32>,
    pub(super) hover: Option<Color32>,
}

/// Each box shades from its own color at the top edge to 30% darker at the
/// bottom edge. The fade is a pixel-aligned Bayer stipple, so neighboring
/// boxes do not share one smooth gradient. A match outline is a solid frame
/// one map pixel thick. A hover fills that same quarter of the cells with
/// this box's color, 30% brighter.
pub(super) fn paint_tile_gradient(
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
pub(super) fn paint_dither_overlay(
    painter: &egui::Painter,
    grid: &MapGrid,
    rect: Rect,
    glows: &DitherGlows,
) {
    paint_dither(painter, grid, rect, None, glows);
}

pub(super) fn paint_dither(
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

pub(super) fn dither_mesh(
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
            if glows.hover.is_some_and(|_| dither_lit(HOVER_DITHER, bayer)) {
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
pub(super) fn dither_mix(t: f32, bayer: u8, levels: u8) -> f32 {
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
pub(super) fn outline_frame(rect: Rect, grid: &MapGrid) -> Vec<Rect> {
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

pub(super) fn dither_lit(coverage: f32, bayer: u8) -> bool {
    coverage > 0.0 && coverage * 16.0 > f32::from(bayer)
}

pub(super) fn lerp_color(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |start: u8, end: u8| (start as f32 + (end as f32 - start as f32) * t).round() as u8;
    let [r1, g1, b1, a1] = from.to_array();
    let [r2, g2, b2, a2] = to.to_array();
    Color32::from_rgba_unmultiplied(mix(r1, r2), mix(g1, g2), mix(b1, b2), mix(a1, a2))
}

pub(super) fn push_solid_quad(mesh: &mut Mesh, rect: Rect, color: Color32) {
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
pub(super) fn shade_darker(color: Color32, factor: f32) -> Color32 {
    scale_channels(color, factor)
}

/// Keep hue and alpha. Multiply the color channels, clamped, so the result is brighter.
pub(super) fn shade_brighter(color: Color32, factor: f32) -> Color32 {
    scale_channels(color, factor)
}

pub(super) fn scale_channels(color: Color32, factor: f32) -> Color32 {
    let [r, g, b, a] = color.to_array();
    let scale = |channel: u8| (channel as f32 * factor).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(scale(r), scale(g), scale(b), a)
}

/// Directory frame. Adjacent depths alternate so the parent's grey shows as
/// the spacing around the box inside it. No stroke.
pub(super) fn directory_grey(dark: bool, depth: usize) -> Color32 {
    let tone = if dark {
        if depth.is_multiple_of(2) {
            68
        } else {
            98
        }
    } else if depth.is_multiple_of(2) {
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
pub(super) fn hatch_shapes(rect: Rect, color: Color32, fill: f32) -> Vec<egui::Shape> {
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
