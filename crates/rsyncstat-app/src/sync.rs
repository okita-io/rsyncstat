//! Compare two scanned trees and copy files between them.
//!
//! One-way sync copies missing files and overwrites when the size differs.
//! It does not delete extras on the destination. Two-way sync copies only
//! files that are missing on the other side, and leaves size mismatches alone.

use rsyncstat_model::TreeNode;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const COPY_ATTEMPTS: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncMethod {
    AtoB,
    BtoA,
    BothWays,
}

impl SyncMethod {
    pub fn label(self) -> &'static str {
        match self {
            Self::AtoB => "Copy A → B",
            Self::BtoA => "Copy B → A",
            Self::BothWays => "Two-way (missing only)",
        }
    }

    pub fn all() -> [Self; 3] {
        [Self::AtoB, Self::BtoA, Self::BothWays]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchKind {
    Same,
    Different,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncRules {
    /// Extensions without a leading dot, matched case-insensitively.
    /// `tar.gz` matches names that end in `.tar.gz`.
    pub file_types: Vec<String>,
    /// Directory names. A match anywhere in the relative path drops that subtree.
    pub dir_names: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyOp {
    pub rel: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncPlan {
    pub a_to_b: Vec<CopyOp>,
    pub b_to_a: Vec<CopyOp>,
    pub same: u64,
    pub different: u64,
    pub excluded: u64,
}

impl SyncPlan {
    pub fn copy_count(&self) -> usize {
        self.a_to_b.len() + self.b_to_a.len()
    }

    pub fn copy_bytes(&self) -> u64 {
        self.a_to_b
            .iter()
            .chain(self.b_to_a.iter())
            .map(|op| op.bytes)
            .sum()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompareSummary {
    pub same: u64,
    pub different: u64,
    pub only_a: u64,
    pub only_b: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyFailure {
    pub rel: String,
    pub direction: &'static str,
    pub attempts: u32,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct SyncReport {
    pub copied: u64,
    pub bytes: u64,
    /// Copies that failed once and then succeeded.
    pub recovered: u64,
    pub errors: Vec<CopyFailure>,
}

#[derive(Debug)]
pub enum SyncEvent {
    Progress {
        done: u64,
        total: u64,
        rel: String,
    },
    /// One recovery-log line: a failed attempt, a retry, or a recovered copy.
    Log(String),
    Finished(SyncReport),
}

pub fn parse_file_types(raw: &str) -> Vec<String> {
    split_tokens(raw)
        .into_iter()
        .map(|token| {
            token
                .trim_start_matches("*.")
                .trim_start_matches('.')
                .to_ascii_lowercase()
        })
        .filter(|token| is_safe_token(token))
        .collect()
}

pub fn parse_dir_names(raw: &str) -> Vec<String> {
    split_tokens(raw)
        .into_iter()
        .map(|token| token.trim_matches('/').to_string())
        .filter(|token| is_safe_token(token))
        .collect()
}

pub fn rules_from_text(file_types: &str, dir_names: &str) -> SyncRules {
    SyncRules {
        file_types: parse_file_types(file_types),
        dir_names: parse_dir_names(dir_names),
    }
}

/// Preset lines plus the extra type and directory excludes, ready for [`rsyncstat_filter::FilterSet`].
pub fn filter_lines(file_types: &str, dir_names: &str) -> Vec<String> {
    let rules = rules_from_text(file_types, dir_names);
    let mut lines: Vec<String> = rsyncstat_filter::default_preset_rules()
        .into_iter()
        .map(str::to_string)
        .collect();
    for ext in rules.file_types {
        lines.push(format!("*.{ext}"));
    }
    for name in rules.dir_names {
        lines.push(format!("{name}/"));
    }
    lines
}

pub fn match_file(rel: &str, size: u64, other: &HashMap<String, u64>) -> Option<MatchKind> {
    match other.get(rel) {
        Some(other_size) if *other_size == size => Some(MatchKind::Same),
        Some(_) => Some(MatchKind::Different),
        None => None,
    }
}

pub fn compare_indexes(a: &HashMap<String, u64>, b: &HashMap<String, u64>) -> CompareSummary {
    let mut summary = CompareSummary::default();
    for (rel, size_a) in a {
        match b.get(rel) {
            Some(size_b) if size_b == size_a => summary.same += 1,
            Some(_) => summary.different += 1,
            None => summary.only_a += 1,
        }
    }
    for rel in b.keys() {
        if !a.contains_key(rel) {
            summary.only_b += 1;
        }
    }
    summary
}

/// One direct child of the two scan roots, included only when its sizes differ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirDelta {
    pub name: String,
    pub bytes_a: u64,
    pub bytes_b: u64,
}

impl DirDelta {
    pub fn a_minus_b(&self) -> i128 {
        self.bytes_a as i128 - self.bytes_b as i128
    }
}

/// Byte difference between two scanned folders, checked against their children.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryDiff {
    pub bytes_a: u64,
    pub bytes_b: u64,
    /// Differing top-level files and folders, largest absolute gap first.
    pub entries: Vec<DirDelta>,
    /// Sum of every direct child's (A − B) equals [`Self::bytes_a`] − [`Self::bytes_b`].
    pub balanced: bool,
}

impl DirectoryDiff {
    pub fn a_minus_b(&self) -> i128 {
        self.bytes_a as i128 - self.bytes_b as i128
    }
}

/// Compare the two scan roots by their direct children.
///
/// Equal children are omitted. `balanced` is true when those children, including
/// the equal ones, account for the whole difference between the roots.
pub fn directory_diff(a: &TreeNode, b: &TreeNode) -> DirectoryDiff {
    let mut sizes_a: HashMap<&str, (u64, bool)> = HashMap::new();
    let mut sizes_b: HashMap<&str, (u64, bool)> = HashMap::new();
    for child in &a.children {
        sizes_a.insert(child.name.as_str(), (child.total_size(), child.is_dir));
    }
    for child in &b.children {
        sizes_b.insert(child.name.as_str(), (child.total_size(), child.is_dir));
    }
    let mut names: Vec<&str> = sizes_a.keys().copied().collect();
    for name in sizes_b.keys() {
        if !sizes_a.contains_key(name) {
            names.push(name);
        }
    }
    let mut child_net: i128 = 0;
    let mut entries = Vec::new();
    for name in names {
        let (bytes_a, dir_a) = sizes_a.get(name).copied().unwrap_or((0, false));
        let (bytes_b, dir_b) = sizes_b.get(name).copied().unwrap_or((0, false));
        child_net += bytes_a as i128 - bytes_b as i128;
        if bytes_a == bytes_b {
            continue;
        }
        let name = if dir_a || dir_b {
            format!("{name}/")
        } else {
            (*name).to_string()
        };
        entries.push(DirDelta {
            name,
            bytes_a,
            bytes_b,
        });
    }
    entries.sort_by(|left, right| {
        right
            .a_minus_b()
            .unsigned_abs()
            .cmp(&left.a_minus_b().unsigned_abs())
            .then_with(|| left.name.cmp(&right.name))
    });
    let bytes_a = a.total_size();
    let bytes_b = b.total_size();
    DirectoryDiff {
        bytes_a,
        bytes_b,
        entries,
        balanced: child_net == bytes_a as i128 - bytes_b as i128,
    }
}

pub fn plan_sync(a: &TreeNode, b: &TreeNode, method: SyncMethod, rules: &SyncRules) -> SyncPlan {
    let (files_a, excluded_a) = collect_files(a, rules);
    let (files_b, excluded_b) = collect_files(b, rules);
    let mut plan = SyncPlan {
        excluded: excluded_a + excluded_b,
        ..SyncPlan::default()
    };
    for (rel, size_a) in &files_a {
        match files_b.get(rel) {
            Some(size_b) if size_b == size_a => plan.same += 1,
            Some(size_b) => match method {
                SyncMethod::AtoB => plan.a_to_b.push(CopyOp {
                    rel: rel.clone(),
                    bytes: *size_a,
                }),
                SyncMethod::BtoA => plan.b_to_a.push(CopyOp {
                    rel: rel.clone(),
                    bytes: *size_b,
                }),
                SyncMethod::BothWays => plan.different += 1,
            },
            None => {
                if matches!(method, SyncMethod::AtoB | SyncMethod::BothWays) {
                    plan.a_to_b.push(CopyOp {
                        rel: rel.clone(),
                        bytes: *size_a,
                    });
                }
            }
        }
    }
    for (rel, size_b) in &files_b {
        if files_a.contains_key(rel) {
            continue;
        }
        if matches!(method, SyncMethod::BtoA | SyncMethod::BothWays) {
            plan.b_to_a.push(CopyOp {
                rel: rel.clone(),
                bytes: *size_b,
            });
        }
    }
    plan.a_to_b.sort_by(|left, right| left.rel.cmp(&right.rel));
    plan.b_to_a.sort_by(|left, right| left.rel.cmp(&right.rel));
    plan
}

pub fn execute(
    root_a: &Path,
    root_b: &Path,
    plan: &SyncPlan,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64, &str),
    mut log: impl FnMut(String),
) -> SyncReport {
    let total = plan.copy_count() as u64;
    let mut report = SyncReport::default();
    let mut done = 0u64;
    let mut failed = Vec::new();
    for (op, from_root, to_root, direction) in plan
        .a_to_b
        .iter()
        .map(|op| (op, root_a, root_b, "A→B"))
        .chain(plan.b_to_a.iter().map(|op| (op, root_b, root_a, "B→A")))
    {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        done += 1;
        progress(done, total, &op.rel);
        match copy_with_retry(from_root, to_root, &op.rel, direction, &mut log) {
            Ok((bytes, recovered)) => {
                report.copied += 1;
                report.bytes += bytes;
                if recovered {
                    report.recovered += 1;
                }
            }
            Err(failure) => failed.push(failure),
        }
    }
    if !failed.is_empty() && !cancel.load(Ordering::Relaxed) {
        log(format!(
            "recovery: retrying {} failed {}",
            failed.len(),
            if failed.len() == 1 { "copy" } else { "copies" }
        ));
        for failure in failed {
            if cancel.load(Ordering::Relaxed) {
                report.errors.push(failure);
                continue;
            }
            let (from_root, to_root) = if failure.direction == "A→B" {
                (root_a, root_b)
            } else {
                (root_b, root_a)
            };
            progress(done, total, &failure.rel);
            match copy_with_retry(
                from_root,
                to_root,
                &failure.rel,
                failure.direction,
                &mut log,
            ) {
                Ok((bytes, _)) => {
                    report.copied += 1;
                    report.bytes += bytes;
                    report.recovered += 1;
                }
                Err(err) => report.errors.push(err),
            }
        }
    } else {
        report.errors = failed;
    }
    report
}

pub fn spawn_sync(
    root_a: PathBuf,
    root_b: PathBuf,
    plan: SyncPlan,
    cancel: Arc<AtomicBool>,
    tx: crossbeam_channel::Sender<SyncEvent>,
) {
    std::thread::spawn(move || {
        let tx_log = tx.clone();
        let mut announced = false;
        let mut logged = 0usize;
        let report = execute(
            &root_a,
            &root_b,
            &plan,
            &cancel,
            |done, total, rel| {
                let _ = tx.send(SyncEvent::Progress {
                    done,
                    total,
                    rel: rel.to_string(),
                });
            },
            |line| {
                const MAX_LOG_LINES: usize = 200;
                if !announced {
                    announced = true;
                    if let Some(path) = rsyncstat_scan::recovery_log_path() {
                        let header = format!("recovery log {}", path.display());
                        write_recovery(&header, &tx_log);
                    }
                }
                if logged < MAX_LOG_LINES {
                    write_recovery(&line, &tx_log);
                    logged += 1;
                } else if logged == MAX_LOG_LINES {
                    write_recovery(
                        "recovery: further copy retries omitted from the log",
                        &tx_log,
                    );
                    logged += 1;
                }
            },
        );
        let _ = tx.send(SyncEvent::Finished(report));
    });
}

fn write_recovery(line: &str, tx: &crossbeam_channel::Sender<SyncEvent>) {
    eprintln!("rsyncstat: {line}");
    let _ = rsyncstat_scan::append_recovery_log(line);
    let _ = tx.send(SyncEvent::Log(line.to_string()));
}

fn copy_with_retry(
    from_root: &Path,
    to_root: &Path,
    rel: &str,
    direction: &'static str,
    log: &mut impl FnMut(String),
) -> Result<(u64, bool), CopyFailure> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match copy_rel(from_root, to_root, rel) {
            Ok(bytes) => {
                if attempt > 1 {
                    log(format!(
                        "recovered {direction} {rel} after {attempt} attempts"
                    ));
                }
                return Ok((bytes, attempt > 1));
            }
            Err(err) => {
                log(format!(
                    "attempt {attempt}/{COPY_ATTEMPTS} {direction} {rel}: {err}"
                ));
                match retry_wait(attempt, &err) {
                    Some(wait) if !wait.is_zero() => thread::sleep(wait),
                    Some(_) => {}
                    None => {
                        return Err(CopyFailure {
                            rel: rel.to_string(),
                            direction,
                            attempts: attempt,
                            message: err.to_string(),
                        });
                    }
                }
            }
        }
    }
}

/// `Some` means try again after waiting. `None` means keep the failure.
fn retry_wait(attempt: u32, err: &io::Error) -> Option<Duration> {
    if attempt >= COPY_ATTEMPTS || err.kind() == io::ErrorKind::InvalidInput {
        return None;
    }
    let transient = matches!(
        err.kind(),
        io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
            | io::ErrorKind::UnexpectedEof
    ) || matches!(err.raw_os_error(), Some(11 | 16 | 26 | 35 | 60));
    if transient {
        Some(Duration::from_millis(50 * u64::from(attempt)))
    } else if attempt == 1 {
        // One immediate retry for races (file locked, dest just created).
        Some(Duration::ZERO)
    } else {
        None
    }
}

fn copy_rel(from_root: &Path, to_root: &Path, rel: &str) -> io::Result<u64> {
    let rel_path = safe_rel(rel).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "refusing unsafe relative path")
    })?;
    let from = from_root.join(&rel_path);
    let to = to_root.join(&rel_path);
    let parent = to
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut tmp_name = to
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name"))?
        .to_os_string();
    tmp_name.push(".rsyncstat-tmp");
    let tmp = to.with_file_name(tmp_name);
    let bytes = match fs::copy(&from, &tmp) {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
    };
    if to.exists() {
        if let Err(err) = fs::remove_file(&to) {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
    }
    if let Err(err) = fs::rename(&tmp, &to) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(bytes)
}

fn safe_rel(rel: &str) -> Option<PathBuf> {
    if rel.is_empty() {
        return None;
    }
    let mut out = PathBuf::new();
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

fn collect_files(root: &TreeNode, rules: &SyncRules) -> (HashMap<String, u64>, u64) {
    let mut files = HashMap::new();
    let mut excluded = 0u64;
    walk(root, "", rules, &mut files, &mut excluded);
    (files, excluded)
}

fn walk(
    node: &TreeNode,
    prefix: &str,
    rules: &SyncRules,
    files: &mut HashMap<String, u64>,
    excluded: &mut u64,
) {
    for child in &node.children {
        let rel = if prefix.is_empty() {
            child.name.clone()
        } else {
            format!("{prefix}/{}", child.name)
        };
        if child.is_dir {
            if child.excluded || rules.dir_names.iter().any(|name| name == &child.name) {
                *excluded += count_files(child);
                continue;
            }
            walk(child, &rel, rules, files, excluded);
        } else if child.excluded || ext_excluded(&child.name, &rules.file_types) {
            *excluded += 1;
        } else {
            files.insert(rel, child.size);
        }
    }
}

fn count_files(node: &TreeNode) -> u64 {
    if !node.is_dir {
        return 1;
    }
    node.children.iter().map(count_files).sum()
}

fn ext_excluded(name: &str, types: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    types.iter().any(|ext| lower.ends_with(&format!(".{ext}")))
}

fn split_tokens(raw: &str) -> Vec<&str> {
    raw.split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect()
}

fn is_safe_token(token: &str) -> bool {
    !token.is_empty()
        && token != "."
        && token != ".."
        && !token.contains(['/', '\\', '*', '?', '!', '#', '[', ']'])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample_trees() -> (TreeNode, TreeNode) {
        let mut a = TreeNode::new_dir("A");
        a.insert_scanned(Path::new("same.txt"), 10, false, false, None);
        a.insert_scanned(Path::new("only_a.txt"), 4, false, false, None);
        a.insert_scanned(Path::new("diff.txt"), 1, false, false, None);
        a.insert_scanned(Path::new("notes.log"), 3, false, false, None);
        a.insert_scanned(Path::new("skip/secret.txt"), 9, false, false, None);

        let mut b = TreeNode::new_dir("B");
        b.insert_scanned(Path::new("same.txt"), 10, false, false, None);
        b.insert_scanned(Path::new("only_b.txt"), 8, false, false, None);
        b.insert_scanned(Path::new("diff.txt"), 5, false, false, None);
        b.insert_scanned(Path::new("photo.JPG"), 2, false, false, None);
        (a, b)
    }

    #[test]
    fn parses_types_and_directory_names() {
        assert_eq!(
            parse_file_types("log, *.bak TMP .jpg"),
            vec!["log", "bak", "tmp", "jpg"]
        );
        assert_eq!(
            parse_dir_names("node_modules, .git, node_modules/"),
            vec!["node_modules", ".git", "node_modules"]
        );
        assert!(parse_dir_names("skip/nested").is_empty());
    }

    #[test]
    fn one_way_copies_missing_and_different_sizes() {
        let (a, b) = sample_trees();
        let rules = rules_from_text("log, jpg", "skip");
        let plan = plan_sync(&a, &b, SyncMethod::AtoB, &rules);
        assert_eq!(plan.same, 1);
        assert_eq!(
            plan.a_to_b
                .iter()
                .map(|op| op.rel.as_str())
                .collect::<Vec<_>>(),
            vec!["diff.txt", "only_a.txt"]
        );
        assert!(plan.b_to_a.is_empty());
        assert!(plan.excluded >= 3);
    }

    #[test]
    fn two_way_copies_only_missing_files() {
        let (a, b) = sample_trees();
        let rules = rules_from_text("log jpg", "skip");
        let plan = plan_sync(&a, &b, SyncMethod::BothWays, &rules);
        assert_eq!(plan.different, 1);
        assert_eq!(plan.a_to_b.len(), 1);
        assert_eq!(plan.a_to_b[0].rel, "only_a.txt");
        assert_eq!(plan.b_to_a.len(), 1);
        assert_eq!(plan.b_to_a[0].rel, "only_b.txt");
    }

    #[test]
    fn compare_counts_each_side() {
        let mut a = HashMap::new();
        a.insert("same.txt".into(), 10);
        a.insert("diff.txt".into(), 1);
        a.insert("only_a.txt".into(), 4);
        let mut b = HashMap::new();
        b.insert("same.txt".into(), 10);
        b.insert("diff.txt".into(), 5);
        b.insert("only_b.txt".into(), 8);
        let summary = compare_indexes(&a, &b);
        assert_eq!(
            summary,
            CompareSummary {
                same: 1,
                different: 1,
                only_a: 1,
                only_b: 1,
            }
        );
        assert_eq!(match_file("same.txt", 10, &b), Some(MatchKind::Same));
        assert_eq!(match_file("diff.txt", 1, &b), Some(MatchKind::Different));
        assert_eq!(match_file("only_a.txt", 4, &b), None);
    }

    fn tree_with(files: &[(&str, u64)]) -> TreeNode {
        let mut root = TreeNode::new_dir("root");
        for (path, size) in files {
            root.insert_scanned(std::path::Path::new(path), *size, false, false, None);
        }
        root
    }

    #[test]
    fn directory_diff_lists_gaps_that_add_up() {
        let a = tree_with(&[
            ("keep/a.txt", 10),
            ("bigger/a.txt", 100),
            ("only_a/a.txt", 40),
        ]);
        let b = tree_with(&[
            ("keep/a.txt", 10),
            ("bigger/a.txt", 25),
            ("only_b/b.txt", 7),
        ]);
        let diff = directory_diff(&a, &b);
        assert_eq!(diff.bytes_a, 150);
        assert_eq!(diff.bytes_b, 42);
        assert_eq!(diff.a_minus_b(), 108);
        assert!(diff.balanced);
        assert_eq!(
            diff.entries
                .iter()
                .map(|entry| (entry.name.as_str(), entry.a_minus_b()))
                .collect::<Vec<_>>(),
            vec![("bigger/", 75), ("only_a/", 40), ("only_b/", -7)]
        );
        let listed: i128 = diff.entries.iter().map(DirDelta::a_minus_b).sum();
        assert_eq!(listed, diff.a_minus_b());
    }

    #[test]
    fn directory_diff_is_empty_when_sizes_match() {
        let tree = tree_with(&[("x/a.txt", 5)]);
        let diff = directory_diff(&tree, &tree);
        assert_eq!(diff.a_minus_b(), 0);
        assert!(diff.entries.is_empty());
        assert!(diff.balanced);
    }

    #[test]
    fn execute_copies_missing_file_and_refuses_parent_segments() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rsyncstat-sync-{nanos}"));
        let a = root.join("a");
        let b = root.join("b");
        fs::create_dir_all(a.join("sub")).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("sub/hello.txt"), b"hello").unwrap();
        let plan = SyncPlan {
            a_to_b: vec![
                CopyOp {
                    rel: "sub/hello.txt".into(),
                    bytes: 5,
                },
                CopyOp {
                    rel: "../escape.txt".into(),
                    bytes: 1,
                },
            ],
            ..SyncPlan::default()
        };
        let mut lines = Vec::new();
        let report = execute(
            &a,
            &b,
            &plan,
            &AtomicBool::new(false),
            |_, _, _| {},
            |line| lines.push(line),
        );
        assert_eq!(report.copied, 1);
        assert_eq!(fs::read(b.join("sub/hello.txt")).unwrap(), b"hello");
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].rel, "../escape.txt");
        assert!(lines
            .iter()
            .any(|line| line.contains("unsafe relative path")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_file_is_retried_and_logged_for_another_pass() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rsyncstat-retry-{nanos}"));
        let a = root.join("a");
        let b = root.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let plan = SyncPlan {
            a_to_b: vec![CopyOp {
                rel: "missing.txt".into(),
                bytes: 1,
            }],
            ..SyncPlan::default()
        };
        let mut lines = Vec::new();
        let report = execute(
            &a,
            &b,
            &plan,
            &AtomicBool::new(false),
            |_, _, _| {},
            |line| lines.push(line),
        );
        let _ = fs::remove_dir_all(&root);
        assert_eq!(report.copied, 0);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].rel, "missing.txt");
        assert_eq!(report.errors[0].direction, "A→B");
        assert!(report.errors[0].attempts >= 2);
        assert!(lines
            .iter()
            .any(|line| line.starts_with("recovery: retrying")));
        assert!(lines.iter().filter(|line| line.contains("attempt")).count() >= 2);
    }

    #[test]
    fn transient_errors_wait_and_other_errors_retry_once() {
        let busy = io::Error::from_raw_os_error(16);
        assert_eq!(retry_wait(1, &busy), Some(Duration::from_millis(50)));
        assert_eq!(retry_wait(2, &busy), Some(Duration::from_millis(100)));
        assert_eq!(retry_wait(3, &busy), None);

        let missing = io::Error::new(io::ErrorKind::NotFound, "gone");
        assert_eq!(retry_wait(1, &missing), Some(Duration::ZERO));
        assert_eq!(retry_wait(2, &missing), None);

        let refused = io::Error::new(io::ErrorKind::InvalidInput, "no");
        assert_eq!(retry_wait(1, &refused), None);
    }
}
