//! End-to-end scan tests against real directories.

use crossbeam_channel::RecvTimeoutError;
use rsyncstat_filter::{FilterSet, RuleLevel};
use rsyncstat_model::TreeNode;
use rsyncstat_scan::{scan_root, scan_root_async, ScanUpdate};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Sandbox(PathBuf);

impl Sandbox {
    fn new(label: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "rsyncstat-scan-e2e-{label}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(root: &Path, rel: &str, len: usize) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, vec![7u8; len]).unwrap();
}

fn collect_async(
    root: &Path,
    filter: FilterSet,
    cancel: Arc<AtomicBool>,
) -> (TreeNode, Vec<String>, bool) {
    let (tx, rx) = crossbeam_channel::unbounded();
    scan_root_async(root.to_path_buf(), Arc::new(filter), tx, cancel, false);
    let mut tree: Option<TreeNode> = None;
    let mut notices = Vec::new();
    let mut started_first = true;
    let mut seen_any = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ScanUpdate::Started { root_name }) => {
                tree = Some(TreeNode::new_dir(root_name));
                seen_any = true;
            }
            Ok(ScanUpdate::Batch(entries)) => {
                if tree.is_none() {
                    started_first = false;
                    break;
                }
                for e in entries {
                    tree.as_mut()
                        .unwrap()
                        .insert_scanned(&e.rel, e.size, e.is_dir, e.excluded, e.rule);
                }
            }
            Ok(ScanUpdate::Notice(n)) => notices.push(n),
            Ok(ScanUpdate::Visiting(_)) => {}
            Ok(ScanUpdate::Finished) => break,
            Ok(ScanUpdate::Failed(m)) => panic!("scan failed: {m}"),
            Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
            Err(e) => panic!("{e}"),
        }
    }
    assert!(seen_any || !started_first);
    let mut tree = tree.expect("no tree");
    tree.sort_children_recursive();
    (tree, notices, started_first)
}

#[test]
fn sizes_roll_up_exactly_through_nested_directories() {
    let sb = Sandbox::new("rollup");
    write(&sb.0, "a/b/c/leaf.bin", 100);
    write(&sb.0, "a/b/mid.bin", 20);
    write(&sb.0, "a/top.bin", 3);
    write(&sb.0, "root.bin", 1);
    let tree = scan_root(&sb.0, &FilterSet::default_presets()).unwrap();
    assert_eq!(tree.size, 124);
    assert_eq!(tree.child("a").unwrap().size, 123);
    assert_eq!(tree.child("a").unwrap().child("b").unwrap().size, 120);
    assert!(tree.child("a").unwrap().is_dir);
    assert!(!tree.child("root.bin").unwrap().is_dir);
}

#[test]
fn empty_directory_scans_to_an_empty_tree() {
    let sb = Sandbox::new("empty");
    let tree = scan_root(&sb.0, &FilterSet::default_presets()).unwrap();
    assert_eq!(tree.size, 0);
    assert!(tree.children.is_empty());
}

#[test]
fn missing_root_is_an_error() {
    let sb = Sandbox::new("missing");
    assert!(scan_root(&sb.0.join("nope"), &FilterSet::default_presets()).is_err());
}

#[test]
fn async_scan_sends_started_first_and_matches_the_sync_scan() {
    let sb = Sandbox::new("async");
    for i in 0..200 {
        write(&sb.0, &format!("d{}/f{i}.dat", i % 10), i + 1);
    }
    let (tree, _, started_first) = collect_async(
        &sb.0,
        FilterSet::default_presets(),
        Arc::new(AtomicBool::new(false)),
    );
    assert!(started_first);
    let expected: usize = (1..=200).sum();
    assert_eq!(tree.size, expected as u64);
    let blocking = scan_root(&sb.0, &FilterSet::default_presets()).unwrap();
    assert_eq!(tree.size, blocking.size);
    assert_eq!(tree.children.len(), blocking.children.len());
}

#[test]
fn excluded_paths_are_listed_but_not_descended_or_counted() {
    let sb = Sandbox::new("excluded");
    write(&sb.0, "src/main.rs", 10);
    write(&sb.0, "node_modules/a/b/c.js", 999);
    write(&sb.0, "target/debug/x", 999);
    write(&sb.0, "cache.lancedb/seg", 999);
    let tree = scan_root(&sb.0, &FilterSet::default_presets()).unwrap();
    assert_eq!(tree.size, 10);
    for name in ["node_modules", "target", "cache.lancedb"] {
        let node = tree.child(name).unwrap_or_else(|| panic!("{name} missing"));
        assert!(node.excluded, "{name}");
        assert!(node.children.is_empty(), "{name} was descended");
        assert!(node.exclude_rule.is_some(), "{name} has no rule text");
    }
}

#[test]
fn custom_filter_lines_drive_exclusion() {
    let sb = Sandbox::new("custom");
    write(&sb.0, "keep.txt", 5);
    write(&sb.0, "drop.secret", 5);
    write(&sb.0, "private/x.txt", 5);
    let filter = FilterSet::from_user_lines(&["*.secret", "private/"], RuleLevel::Profile);
    let tree = scan_root(&sb.0, &filter).unwrap();
    assert!(!tree.child("keep.txt").unwrap().excluded);
    assert!(tree.child("drop.secret").is_none_or(|n| n.excluded));
    assert!(tree
        .child("private")
        .is_none_or(|n| n.excluded && n.children.is_empty()));
}

#[test]
fn cancelled_scan_still_terminates_cleanly() {
    let sb = Sandbox::new("cancel");
    for i in 0..50 {
        write(&sb.0, &format!("d{i}/f"), 1);
    }
    let cancel = Arc::new(AtomicBool::new(true));
    let (tree, _, _) = collect_async(&sb.0, FilterSet::default_presets(), cancel);
    // Cancelled before the walk: at most a partial tree, never a hang or panic.
    assert!(tree.size <= 50);
}

#[test]
fn consecutive_scans_do_not_leak_entries_into_each_other() {
    let sb = Sandbox::new("isolation");
    write(&sb.0, "one/a", 1);
    let first = scan_root(&sb.0, &FilterSet::default_presets()).unwrap();
    fs::remove_dir_all(sb.0.join("one")).unwrap();
    write(&sb.0, "two/b", 2);
    let second = scan_root(&sb.0, &FilterSet::default_presets()).unwrap();
    assert!(first.child("one").is_some());
    assert!(second.child("one").is_none());
    assert_eq!(second.size, 2);
}

#[cfg(unix)]
#[test]
fn symlink_loops_do_not_hang_and_links_are_not_followed() {
    use std::os::unix::fs::symlink;
    let sb = Sandbox::new("links");
    write(&sb.0, "real/file", 10);
    symlink(&sb.0, sb.0.join("real/loop")).unwrap();
    let tree = scan_root(&sb.0, &FilterSet::default_presets()).unwrap();
    // The link entry itself is tiny; the 10-byte file is counted once.
    let loop_node = tree.child("real").unwrap().child("loop").unwrap();
    assert!(loop_node.children.is_empty());
    assert!(tree.size >= 10 && tree.size < 10 + 4096);
}
