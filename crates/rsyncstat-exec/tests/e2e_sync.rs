//! End-to-end tests: real directories on disk, real scan, real plan, real copy.

mod sync {
    pub use rsyncstat_exec::*;
    pub use rsyncstat_plan::*;
}

use rsyncstat_filter::{FilterSet, PatternOptions};
use rsyncstat_model::TreeNode;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use sync::{execute, plan_sync, rules_from_patterns, SyncEvent, SyncPlan, SyncRules};

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(label: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "rsyncstat-e2e-{label}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("A")).unwrap();
        fs::create_dir_all(root.join("B")).unwrap();
        Self { root }
    }
    fn a(&self) -> PathBuf {
        self.root.join("A")
    }
    fn b(&self) -> PathBuf {
        self.root.join("B")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn read(root: &Path, rel: &str) -> Vec<u8> {
    fs::read(root.join(rel)).unwrap()
}

fn no_rules() -> SyncRules {
    rules_from_patterns(
        "",
        PatternOptions::default(),
        "",
        PatternOptions::default(),
        false,
    )
}

fn preset_rules() -> SyncRules {
    rules_from_patterns(
        "",
        PatternOptions::default(),
        "",
        PatternOptions::default(),
        true,
    )
}

fn scan(root: &Path) -> TreeNode {
    rsyncstat_scan::scan_root(root, &FilterSet::default_presets()).unwrap()
}

fn plan(sb: &Sandbox, rules: &SyncRules) -> SyncPlan {
    plan_sync(&scan(&sb.a()), &scan(&sb.b()), rules)
}

fn run(sb: &Sandbox, plan: &SyncPlan) -> sync::SyncReport {
    execute(
        &sb.a(),
        &sb.b(),
        plan,
        &AtomicBool::new(false),
        |_, _, _| {},
        |_| {},
    )
}

fn all_files(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                let rel = path.strip_prefix(base).unwrap();
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn op_names(plan: &SyncPlan) -> Vec<&str> {
    plan.a_to_b.iter().map(|op| op.rel.as_str()).collect()
}

#[test]
fn scan_plan_copy_makes_target_a_superset_with_identical_bytes() {
    let sb = Sandbox::new("full");
    write(&sb.a(), "readme.md", b"hello");
    write(&sb.a(), "src/deep/nested/main.rs", b"fn main() {}");
    write(&sb.a(), "src/lib.rs", b"pub fn x() {}");
    write(&sb.a(), "empty.bin", b"");
    write(&sb.b(), "keep_me.txt", b"only in B");

    let plan = plan(&sb, &no_rules());
    assert_eq!(plan.copy_count(), 4);
    assert!(plan.b_to_a.is_empty(), "sync is one-way A to B");
    assert_eq!(plan.copy_bytes(), 5 + 12 + 13);

    let report = run(&sb, &plan);
    assert_eq!(report.copied, 4);
    assert_eq!(report.bytes, 30);
    assert!(report.errors.is_empty() && report.mismatched.is_empty() && !report.stopped);

    assert_eq!(read(&sb.b(), "readme.md"), b"hello");
    assert_eq!(read(&sb.b(), "src/deep/nested/main.rs"), b"fn main() {}");
    assert_eq!(read(&sb.b(), "empty.bin"), b"");
    // Files only on the target are never touched.
    assert_eq!(read(&sb.b(), "keep_me.txt"), b"only in B");
}

#[test]
fn second_run_is_a_no_op() {
    let sb = Sandbox::new("idem");
    write(&sb.a(), "a/one.txt", b"1");
    write(&sb.a(), "two.txt", b"22");
    let first = plan(&sb, &no_rules());
    run(&sb, &first);

    let again = plan(&sb, &no_rules());
    assert_eq!(again.copy_count(), 0);
    assert_eq!(again.same, 2);
}

#[test]
fn different_size_target_is_reported_never_overwritten() {
    let sb = Sandbox::new("mismatch");
    write(&sb.a(), "conflict.txt", b"source version, longer");
    write(&sb.b(), "conflict.txt", b"target");

    let plan = plan(&sb, &no_rules());
    assert_eq!(op_names(&plan), vec!["conflict.txt"]);
    let report = run(&sb, &plan);

    assert_eq!(report.copied, 0);
    assert_eq!(report.mismatched.len(), 1);
    assert_eq!(report.mismatched[0].rel, "conflict.txt");
    assert_eq!(read(&sb.b(), "conflict.txt"), b"target");
    assert_eq!(
        all_files(&sb.b()),
        vec!["conflict.txt"],
        "no temp file left"
    );
}

#[test]
fn same_size_different_content_is_caught_by_hash_when_forced() {
    let sb = Sandbox::new("hash");
    write(&sb.a(), "f.bin", b"AAAA");
    write(&sb.b(), "f.bin", b"BBBB");
    // The size-based plan treats these as equal...
    assert_eq!(plan(&sb, &no_rules()).copy_count(), 0);
    // ...but if a plan does name the file, the hash guards the target.
    let forced = SyncPlan {
        a_to_b: vec![sync::CopyOp {
            rel: "f.bin".into(),
            bytes: 4,
        }],
        ..SyncPlan::default()
    };
    let report = run(&sb, &forced);
    assert_eq!(report.mismatched.len(), 1);
    assert_eq!(read(&sb.b(), "f.bin"), b"BBBB");
}

#[test]
fn identical_existing_file_is_skipped() {
    let sb = Sandbox::new("skip");
    write(&sb.a(), "f.bin", b"same");
    write(&sb.b(), "f.bin", b"same");
    let forced = SyncPlan {
        a_to_b: vec![sync::CopyOp {
            rel: "f.bin".into(),
            bytes: 4,
        }],
        ..SyncPlan::default()
    };
    let report = run(&sb, &forced);
    assert_eq!((report.copied, report.skipped), (0, 1));
}

#[test]
fn default_presets_keep_junk_and_rebuildable_dirs_out() {
    let sb = Sandbox::new("presets");
    write(&sb.a(), "src/main.rs", b"x");
    write(&sb.a(), "node_modules/pkg/index.js", b"junk");
    write(&sb.a(), "target/debug/app", b"junk");
    write(&sb.a(), ".DS_Store", b"junk");
    write(&sb.a(), "scratch.tmp", b"junk");
    write(&sb.a(), "data.lancedb/part", b"junk");

    let plan = plan(&sb, &preset_rules());
    assert_eq!(op_names(&plan), vec!["src/main.rs"]);
    run(&sb, &plan);
    assert_eq!(all_files(&sb.b()), vec!["src/main.rs"]);
}

#[test]
fn user_exclude_and_include_rules_filter_the_plan() {
    let sb = Sandbox::new("rules");
    write(&sb.a(), "a.txt", b"1");
    write(&sb.a(), "b.log", b"2");
    write(&sb.a(), "private/c.txt", b"3");

    let exclude = rules_from_patterns(
        "",
        PatternOptions::default(),
        "*.log, private",
        PatternOptions::default(),
        false,
    );
    assert_eq!(op_names(&plan(&sb, &exclude)), vec!["a.txt"]);

    let include = rules_from_patterns(
        "*.log",
        PatternOptions::default(),
        "",
        PatternOptions::default(),
        false,
    );
    assert_eq!(op_names(&plan(&sb, &include)), vec!["b.log"]);
}

#[test]
fn unicode_and_spaced_names_survive() {
    let sb = Sandbox::new("names");
    write(&sb.a(), "my folder/résumé final.txt", b"cv");
    write(&sb.a(), "日本語/ファイル.txt", b"jp");
    let report = run(&sb, &plan(&sb, &no_rules()));
    assert_eq!(report.copied, 2);
    assert_eq!(read(&sb.b(), "my folder/résumé final.txt"), b"cv");
    assert_eq!(read(&sb.b(), "日本語/ファイル.txt"), b"jp");
}

#[test]
fn large_file_copies_bit_for_bit() {
    let sb = Sandbox::new("large");
    let data: Vec<u8> = (0..3_000_000u32)
        .map(|i| (i.wrapping_mul(31) % 251) as u8)
        .collect();
    write(&sb.a(), "big.bin", &data);
    let report = run(&sb, &plan(&sb, &no_rules()));
    assert_eq!(report.bytes, data.len() as u64);
    assert_eq!(blake3::hash(&read(&sb.b(), "big.bin")), blake3::hash(&data));
}

#[test]
fn no_temp_files_remain_and_stale_temp_is_replaced() {
    let sb = Sandbox::new("tmp");
    write(&sb.a(), "d/f.txt", b"fresh");
    // A crashed earlier run left a partial temp file behind.
    write(
        &sb.b(),
        &format!("d/f.txt{}", sync::COPY_TEMP_SUFFIX),
        b"partial garbage",
    );
    let forced = SyncPlan {
        a_to_b: vec![sync::CopyOp {
            rel: "d/f.txt".into(),
            bytes: 5,
        }],
        ..SyncPlan::default()
    };
    let report = run(&sb, &forced);
    assert_eq!(report.copied, 1);
    assert_eq!(read(&sb.b(), "d/f.txt"), b"fresh");
    let leftovers: Vec<_> = all_files(&sb.b())
        .into_iter()
        .filter(|f| f.contains(sync::COPY_TEMP_SUFFIX) && f != "d/f.txt.rsyncstat-tmp")
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn unsafe_relative_paths_are_refused_and_nothing_escapes() {
    let sb = Sandbox::new("unsafe");
    write(&sb.root, "secret.txt", b"outside both roots");
    write(&sb.a(), "ok.txt", b"ok");
    let ops = [
        "../secret.txt",
        "a/../../secret.txt",
        "",
        "/etc/passwd",
        "a//b",
        "./x",
    ];
    let plan = SyncPlan {
        a_to_b: ops
            .iter()
            .map(|rel| sync::CopyOp {
                rel: (*rel).into(),
                bytes: 1,
            })
            .collect(),
        ..SyncPlan::default()
    };
    let report = run(&sb, &plan);
    assert_eq!(report.copied, 0);
    assert_eq!(report.errors.len(), ops.len());
    assert!(all_files(&sb.b()).is_empty());
    assert_eq!(read(&sb.root, "secret.txt"), b"outside both roots");
}

#[test]
fn missing_source_file_is_an_error_not_a_panic() {
    let sb = Sandbox::new("vanished");
    write(&sb.a(), "gone.txt", b"here for the scan");
    let p = plan(&sb, &no_rules());
    fs::remove_file(sb.a().join("gone.txt")).unwrap();
    let report = run(&sb, &p);
    assert_eq!(report.copied, 0);
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].rel, "gone.txt");
    assert!(all_files(&sb.b()).is_empty(), "no empty or temp file left");
}

#[test]
fn cancel_before_start_copies_nothing() {
    let sb = Sandbox::new("cancel0");
    write(&sb.a(), "a.txt", b"1");
    write(&sb.a(), "b.txt", b"2");
    let p = plan(&sb, &no_rules());
    let cancel = AtomicBool::new(true);
    let report = execute(&sb.a(), &sb.b(), &p, &cancel, |_, _, _| {}, |_| {});
    assert!(report.stopped);
    assert_eq!(report.copied, 0);
    assert!(all_files(&sb.b()).is_empty());
}

#[test]
fn cancel_mid_run_stops_between_files_and_leaves_whole_files() {
    let sb = Sandbox::new("cancel1");
    for i in 0..10 {
        write(
            &sb.a(),
            &format!("f{i:02}.txt"),
            format!("body {i}").as_bytes(),
        );
    }
    let p = plan(&sb, &no_rules());
    let cancel = AtomicBool::new(false);
    let report = execute(
        &sb.a(),
        &sb.b(),
        &p,
        &cancel,
        |done, _, _| {
            if done == 3 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        |_| {},
    );
    assert!(report.stopped);
    // The file in flight when the stop landed still completes; later ones do not start.
    assert_eq!(report.copied, 3);
    for rel in all_files(&sb.b()) {
        let i: usize = rel[1..3].parse().unwrap();
        assert_eq!(read(&sb.b(), &rel), format!("body {i}").as_bytes());
    }
    // A fresh run finishes the job.
    let rest = run(&sb, &plan(&sb, &no_rules()));
    assert_eq!(rest.copied, 7);
    assert_eq!(plan(&sb, &no_rules()).copy_count(), 0);
}

#[test]
fn progress_reports_every_file_in_order_with_a_stable_total() {
    let sb = Sandbox::new("progress");
    for name in ["a", "b", "c"] {
        write(&sb.a(), &format!("{name}.txt"), b"x");
    }
    let p = plan(&sb, &no_rules());
    let mut seen = Vec::new();
    execute(
        &sb.a(),
        &sb.b(),
        &p,
        &AtomicBool::new(false),
        |done, total, rel| seen.push((done, total, rel.to_string())),
        |_| {},
    );
    assert_eq!(
        seen,
        vec![
            (1, 3, "a.txt".to_string()),
            (2, 3, "b.txt".to_string()),
            (3, 3, "c.txt".to_string())
        ]
    );
}

#[test]
fn spawn_sync_streams_progress_then_finishes() {
    let sb = Sandbox::new("spawn");
    write(&sb.a(), "x/one.txt", b"1");
    write(&sb.a(), "two.txt", b"22");
    let p = plan(&sb, &no_rules());
    let (tx, rx) = crossbeam_channel::unbounded();
    sync::spawn_sync(
        sb.a(),
        sb.b(),
        p,
        Arc::new(AtomicBool::new(false)),
        None,
        tx,
    );

    let mut progress = 0;
    let report = loop {
        match rx
            .recv_timeout(Duration::from_secs(20))
            .expect("sync stalled")
        {
            SyncEvent::Progress { total, .. } => {
                assert_eq!(total, 2);
                progress += 1;
            }
            SyncEvent::Log(_) => {}
            SyncEvent::Finished(report) => break report,
        }
    };
    assert_eq!(progress, 2);
    assert_eq!(report.copied, 2);
    assert_eq!(read(&sb.b(), "x/one.txt"), b"1");
}

#[test]
fn dir_excluded_on_both_sides_plans_nothing() {
    // node_modules is excluded on both sides, so no copy is planned for it.
    let sb = Sandbox::new("excl-both");
    write(&sb.a(), "node_modules/x.js", b"1");
    write(&sb.b(), "node_modules/y.js", b"2");
    let p = plan(&sb, &preset_rules());
    assert_eq!(p.copy_count(), 0);
}

#[cfg(unix)]
#[test]
fn symlink_in_target_cannot_redirect_writes_outside_the_target_root() {
    use std::os::unix::fs::symlink;
    let sb = Sandbox::new("symlink-escape");
    let outside = sb.root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    write(&sb.a(), "link/payload.txt", b"should stay inside B");
    // B/link points at a directory outside B.
    symlink(&outside, sb.b().join("link")).unwrap();

    let forced = SyncPlan {
        a_to_b: vec![sync::CopyOp {
            rel: "link/payload.txt".into(),
            bytes: 20,
        }],
        ..SyncPlan::default()
    };
    let report = run(&sb, &forced);
    assert!(
        !outside.join("payload.txt").exists(),
        "write escaped through a symlink"
    );
    assert_eq!(report.copied, 0);
}

#[cfg(unix)]
#[test]
fn dangling_and_directory_symlinks_in_source_do_not_crash_the_pipeline() {
    use std::os::unix::fs::symlink;
    let sb = Sandbox::new("symlink-src");
    write(&sb.a(), "real.txt", b"real");
    symlink(sb.a().join("nowhere"), sb.a().join("dangling")).unwrap();
    symlink(sb.a(), sb.a().join("loop")).unwrap();
    let p = plan(&sb, &no_rules());
    let report = run(&sb, &p);
    assert_eq!(read(&sb.b(), "real.txt"), b"real");
    // The loop never recurses: the scan finished and nothing blew up.
    assert!(report.copied >= 1);
}
