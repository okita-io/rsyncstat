//! Scan a real directory, lay it out as a treemap, and hit-test the result.

use rsyncstat_filter::FilterSet;
use rsyncstat_treemap::{hit_test, squarify};
use std::fs;

#[test]
fn scanned_tree_lays_out_into_the_viewport_and_hit_tests_to_the_right_child() {
    let root = std::env::temp_dir().join(format!("rsyncstat-e2e-treemap-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    for (name, len) in [
        ("huge.bin", 8000usize),
        ("big.bin", 4000),
        ("mid.bin", 2000),
        ("small.bin", 500),
        ("tiny.bin", 10),
    ] {
        fs::write(root.join(name), vec![0u8; len]).unwrap();
    }

    let tree = rsyncstat_scan::scan_root(&root, &FilterSet::default_presets()).unwrap();
    let (w, h) = (800.0f32, 600.0f32);
    let layout = squarify(&tree.children, |n| n.size as f64, 0.0, 0.0, w, h);
    assert_eq!(layout.len(), 5);

    // Tiles tile the viewport: areas sum to it, none leave it.
    let total: f32 = layout.iter().map(|li| li.rect.w * li.rect.h).sum();
    assert!((total - w * h).abs() < 1.0, "area {total}");
    for li in &layout {
        let r = li.rect;
        assert!(r.x >= -0.01 && r.y >= -0.01 && r.x + r.w <= w + 0.01 && r.y + r.h <= h + 0.01);
    }

    // Area is proportional to size.
    let total_bytes = tree.size as f32;
    for li in &layout {
        let want = li.item.size as f32 / total_bytes * w * h;
        let got = li.rect.w * li.rect.h;
        assert!(
            (want - got).abs() / want.max(1.0) < 0.01,
            "{}: {got} vs {want}",
            li.item.name
        );
    }

    // Clicking the centre of every tile finds that tile.
    for (i, li) in layout.iter().enumerate() {
        let r = li.rect;
        let hit = hit_test(&layout, r.x + r.w / 2.0, r.y + r.h / 2.0).unwrap();
        assert_eq!(layout[hit].item.name, layout[i].item.name);
    }
    assert!(hit_test(&layout, -5.0, -5.0).is_none());
    assert!(hit_test(&layout, w + 1.0, h + 1.0).is_none());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn zero_size_and_empty_inputs_do_not_panic() {
    let layout = squarify::<u8, _>(&[], |_| 1.0, 0.0, 0.0, 100.0, 100.0);
    assert!(layout.is_empty());
    let layout = squarify(&[1u8, 2], |_| 0.0, 0.0, 0.0, 100.0, 100.0);
    assert!(layout.len() <= 2);
    let layout = squarify(&[1u8], |_| 5.0, 0.0, 0.0, 0.0, 0.0);
    let _ = hit_test(&layout, 0.0, 0.0);
}
