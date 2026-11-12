//! Squarified treemap layout (Bruls, Huizing, van Wijk).

use std::cmp::Ordering;

/// Axis-aligned rectangle in layout space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// One laid-out item.
#[derive(Clone, Debug)]
pub struct LayoutItem<T> {
    pub item: T,
    pub rect: Rect,
}

fn worst(row: &[f32], areas: &[f32], side: f32) -> f32 {
    if row.is_empty() {
        return f32::MAX;
    }
    let s: f32 = areas.iter().sum();
    let mx = areas
        .iter()
        .cloned()
        .fold(0.0f32, |a, b| if b > a { b } else { a });
    let mn = areas
        .iter()
        .cloned()
        .fold(f32::MAX, |a, b| if b < a { b } else { a });
    let s2 = s * s;
    let side2 = side * side;
    (side2 * mx / s2).max(s2 / (side2 * mn))
}

/// Squarified layout of positive values. `item` is the index into `values`.
///
/// Zero and negative weights are skipped. The index refers to the original slice,
/// so callers can draw from a tree without cloning nodes.
pub fn squarify_indices(values: &[f64], x: f32, y: f32, w: f32, h: f32) -> Vec<LayoutItem<usize>> {
    let mut rest: Vec<(usize, f64)> = values
        .iter()
        .enumerate()
        .filter_map(|(i, v)| if *v > 0.0 { Some((i, *v)) } else { None })
        .collect();
    rest.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));

    let total: f64 = rest.iter().map(|(_, v)| v).sum();
    if total <= 0.0 || w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }

    let scale = (w as f64 * h as f64) / total;
    let mut out = Vec::new();
    let mut cx = x;
    let mut cy = y;
    let mut cw = w;
    let mut ch = h;

    while !rest.is_empty() {
        let side = cw.min(ch);
        let mut row: Vec<(usize, f64)> = vec![rest[0]];
        let mut i = 1usize;
        while i < rest.len() {
            let trial: Vec<f32> = row
                .iter()
                .chain(std::iter::once(&rest[i]))
                .map(|(_, v)| (*v * scale) as f32)
                .collect();
            let current: Vec<f32> = row.iter().map(|(_, v)| (*v * scale) as f32).collect();
            if worst(&trial, &trial, side) <= worst(&current, &current, side) {
                row.push(rest[i]);
                i += 1;
            } else {
                break;
            }
        }

        let row_sum: f64 = row.iter().map(|(_, v)| v * scale).sum();
        if cw >= ch {
            let rw = row_sum as f32 / ch;
            let mut yy = cy;
            for (idx, v) in &row {
                let area = (*v * scale) as f32;
                let hh = area / rw;
                out.push(LayoutItem {
                    item: *idx,
                    rect: Rect {
                        x: cx,
                        y: yy,
                        w: rw,
                        h: hh,
                    },
                });
                yy += hh;
            }
            cx += rw;
            cw -= rw;
        } else {
            let rh = row_sum as f32 / cw;
            let mut xx = cx;
            for (idx, v) in &row {
                let area = (*v * scale) as f32;
                let ww = area / rh;
                out.push(LayoutItem {
                    item: *idx,
                    rect: Rect {
                        x: xx,
                        y: cy,
                        w: ww,
                        h: rh,
                    },
                });
                xx += ww;
            }
            cy += rh;
            ch -= rh;
        }
        rest = rest.split_off(i);
    }

    out
}

/// Squarified treemap for items with positive `value(item)`.
pub fn squarify<T, F>(
    items: &[T],
    mut value: F,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> Vec<LayoutItem<T>>
where
    T: Clone,
    F: FnMut(&T) -> f64,
{
    let values: Vec<f64> = items.iter().map(|item| value(item)).collect();
    squarify_indices(&values, x, y, w, h)
        .into_iter()
        .map(|li| LayoutItem {
            item: items[li.item].clone(),
            rect: li.rect,
        })
        .collect()
}

/// Map pointer position to the deepest layout item containing it.
pub fn hit_test<T>(layout: &[LayoutItem<T>], px: f32, py: f32) -> Option<usize> {
    layout
        .iter()
        .enumerate()
        .rev()
        .find(|(_, li)| {
            let r = li.rect;
            px >= r.x && px < r.x + r.w && py >= r.y && py < r.y + r.h
        })
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squarify_fills_bounding_box() {
        let items = vec!["a", "b", "c", "d"];
        let layout = squarify(
            &items,
            |s| match *s {
                "a" => 40.0,
                "b" => 30.0,
                "c" => 20.0,
                "d" => 10.0,
                _ => 0.0,
            },
            0.0,
            0.0,
            100.0,
            100.0,
        );
        assert_eq!(layout.len(), 4);
        let area: f32 = layout.iter().map(|li| li.rect.w * li.rect.h).sum();
        assert!((area - 10_000.0).abs() < 1.0);
    }

    #[test]
    fn squarify_skips_zero_weight() {
        let items = vec![("big", 100.0), ("zero", 0.0)];
        let layout = squarify(&items, |(_, v)| *v, 0.0, 0.0, 50.0, 50.0);
        assert_eq!(layout.len(), 1);
        assert_eq!(layout[0].item.0, "big");
    }

    #[test]
    fn hit_test_finds_inner_tile() {
        let items = vec![1u32, 2u32];
        let layout = squarify(&items, |n| *n as f64, 0.0, 0.0, 100.0, 100.0);
        let idx = hit_test(&layout, 10.0, 10.0);
        assert!(idx.is_some());
    }
}
