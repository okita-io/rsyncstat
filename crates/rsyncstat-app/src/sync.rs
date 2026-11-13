//! Compare two scanned trees and copy files between them.
//!
//! One-way sync copies files that are missing on the destination. When the
//! destination file already exists, the contents are hashed: a match is skipped,
//! and a different hash is recorded and left in place. Nothing is overwritten.
//! Two-way sync copies only files that are missing on the other side.

use rsyncstat_filter::FilterSet;
use rsyncstat_model::TreeNode;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const COPY_ATTEMPTS: u32 = 3;
/// Appended to the destination file name while bytes are copied, then removed on rename.
pub const COPY_TEMP_SUFFIX: &str = ".rsyncstat-tmp";

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

/// Include and exclude globs for one sync plan.
///
/// An empty include list keeps every file the exclude list does not drop.
pub struct SyncRules {
    filter: FilterSet,
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

/// A destination file whose contents differ. It was not replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub rel: String,
    pub direction: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct SyncReport {
    pub copied: u64,
    pub bytes: u64,
    /// Copies that failed once and then succeeded.
    pub recovered: u64,
    /// Destination already had the same bytes.
    pub skipped: u64,
    /// Destination existed with different bytes and was left alone.
    pub mismatched: Vec<Mismatch>,
    pub errors: Vec<CopyFailure>,
    /// The user stopped the run. Files not started yet were left in place.
    pub stopped: bool,
}

enum Placed {
    Copied { bytes: u64, recovered: bool },
    Skipped,
    Mismatch,
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

pub fn rules_from_patterns(include: &str, exclude: &str, use_presets: bool) -> SyncRules {
    let lines = filter_lines(include, exclude, use_presets);
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    SyncRules {
        filter: FilterSet::from_user_lines(&borrowed, rsyncstat_filter::RuleLevel::Profile),
    }
}

impl SyncRules {
    fn excludes(&self, rel: &str, is_dir: bool) -> bool {
        self.filter.verdict(Path::new(rel), is_dir).excluded
    }
}

/// Gitignore lines for the scan and the sync plan.
///
/// Patterns are comma-separated. `*.log` and `node_modules` exclude those names.
/// Include patterns are a whitelist: an empty include list keeps every file
/// that is not excluded. Excludes are applied after includes, so they win.
pub fn filter_lines(include: &str, exclude: &str, use_presets: bool) -> Vec<String> {
    let mut lines = Vec::new();
    if use_presets {
        lines.extend(
            rsyncstat_filter::default_preset_rules()
                .into_iter()
                .map(str::to_string),
        );
    }
    let includes = split_patterns(include);
    if !includes.is_empty() {
        // Drop everything, keep directories walkable, then put the requested
        // names back. User excludes are appended after this so they still win.
        lines.push("*".to_string());
        lines.push("!*/".to_string());
        for token in includes {
            lines.extend(include_lines(&token));
        }
    }
    for token in split_patterns(exclude) {
        if let Some(line) = exclude_line(&token) {
            lines.push(line);
        }
    }
    lines
}

fn split_patterns(raw: &str) -> Vec<&str> {
    raw.split([',', ';', '\n'])
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect()
}

fn safe_pattern(token: &str) -> bool {
    let token = token.trim().trim_start_matches('!');
    !token.is_empty()
        && !token.contains(['\n', '\r', '\0'])
        && token.split(['/', '\\']).all(|part| part != "..")
}

fn exclude_line(token: &str) -> Option<String> {
    let token = token.trim().trim_start_matches('!');
    if !safe_pattern(token) {
        return None;
    }
    Some(token.to_string())
}

fn include_lines(token: &str) -> Vec<String> {
    let token = token.trim().trim_start_matches('!');
    if !safe_pattern(token) {
        return Vec::new();
    }
    let token = token.trim_end_matches('/');
    if token.is_empty() {
        return Vec::new();
    }
    // A slash or a wildcard is used as written, anchored the way gitignore anchors it.
    // A bare name also matches that file or directory in any folder.
    if token.contains(['*', '?', '/']) {
        return vec![format!("!{token}"), format!("!{token}/**")];
    }
    vec![
        format!("!{token}"),
        format!("!{token}/**"),
        format!("!**/{token}"),
        format!("!**/{token}/**"),
    ]
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
            report.stopped = true;
            break;
        }
        done += 1;
        progress(done, total, &op.rel);
        match copy_with_retry(from_root, to_root, &op.rel, direction, cancel, &mut log) {
            Ok(Placed::Copied { bytes, recovered }) => {
                report.copied += 1;
                report.bytes += bytes;
                if recovered {
                    report.recovered += 1;
                }
            }
            Ok(Placed::Skipped) => report.skipped += 1,
            Ok(Placed::Mismatch) => report.mismatched.push(Mismatch {
                rel: op.rel.clone(),
                direction,
            }),
            Err(failure) => {
                // A stop between retries is not a failed copy.
                if failure.message == "stopped" {
                    report.stopped = true;
                    break;
                }
                failed.push(failure);
            }
        }
    }
    if cancel.load(Ordering::Relaxed) {
        report.stopped = true;
    }
    if !failed.is_empty() && !report.stopped {
        log(format!(
            "recovery: retrying {} failed {}",
            failed.len(),
            if failed.len() == 1 { "copy" } else { "copies" }
        ));
        for failure in failed {
            if cancel.load(Ordering::Relaxed) {
                report.stopped = true;
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
                cancel,
                &mut log,
            ) {
                Ok(Placed::Copied { bytes, .. }) => {
                    report.copied += 1;
                    report.bytes += bytes;
                    report.recovered += 1;
                }
                Ok(Placed::Skipped) => report.skipped += 1,
                Ok(Placed::Mismatch) => report.mismatched.push(Mismatch {
                    rel: failure.rel.clone(),
                    direction: failure.direction,
                }),
                Err(err) => {
                    if err.message == "stopped" {
                        report.stopped = true;
                    } else {
                        report.errors.push(err);
                    }
                }
            }
        }
    } else {
        report.errors = failed;
    }
    if let Some(path) = persist_mismatches(&report.mismatched) {
        log(format!("mismatch list {}", path.display()));
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
    cancel: &AtomicBool,
    log: &mut impl FnMut(String),
) -> Result<Placed, CopyFailure> {
    let mut attempt = 0u32;
    loop {
        if attempt > 0 && cancel.load(Ordering::Relaxed) {
            return Err(CopyFailure {
                rel: rel.to_string(),
                direction,
                attempts: attempt,
                message: "stopped".to_string(),
            });
        }
        attempt += 1;
        match place_file(from_root, to_root, rel) {
            Ok(Placed::Copied { bytes, .. }) => {
                if attempt > 1 {
                    log(format!(
                        "recovered {direction} {rel} after {attempt} attempts"
                    ));
                }
                return Ok(Placed::Copied {
                    bytes,
                    recovered: attempt > 1,
                });
            }
            Ok(Placed::Skipped) => {
                log(format!(
                    "skip {direction} {rel}: hash matches, left in place"
                ));
                return Ok(Placed::Skipped);
            }
            Ok(Placed::Mismatch) => {
                log(format!(
                    "mismatch {direction} {rel}: hash differs, not overwritten"
                ));
                return Ok(Placed::Mismatch);
            }
            Err(err) => {
                log(format!(
                    "attempt {attempt}/{COPY_ATTEMPTS} {direction} {rel}: {err}"
                ));
                match retry_wait(attempt, &err) {
                    Some(_) if cancel.load(Ordering::Relaxed) => {
                        return Err(CopyFailure {
                            rel: rel.to_string(),
                            direction,
                            attempts: attempt,
                            message: err.to_string(),
                        });
                    }
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

/// Copy `rel` when the destination is absent. An existing file is hashed and
/// either skipped or reported. It is never replaced.
fn place_file(from_root: &Path, to_root: &Path, rel: &str) -> io::Result<Placed> {
    let rel_path = safe_rel(rel).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "refusing unsafe relative path")
    })?;
    let from = from_root.join(&rel_path);
    let to = to_root.join(&rel_path);
    if to.is_file() {
        return classify_existing(&from, &to);
    }
    let parent = to
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut tmp_name = to
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name"))?
        .to_os_string();
    tmp_name.push(COPY_TEMP_SUFFIX);
    let tmp = to.with_file_name(tmp_name);
    // A leftover temp from an earlier attempt makes macOS clonefile return
    // "file exists" instead of replacing it.
    let _ = fs::remove_file(&tmp);
    let bytes = match fs::copy(&from, &tmp) {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
    };
    // The destination showed up while the bytes were copying. Keep it and compare.
    if to.is_file() {
        let _ = fs::remove_file(&tmp);
        return classify_existing(&from, &to);
    }
    if let Err(err) = fs::rename(&tmp, &to) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(Placed::Copied {
        bytes,
        recovered: false,
    })
}

fn classify_existing(from: &Path, to: &Path) -> io::Result<Placed> {
    if same_content(from, to)? {
        Ok(Placed::Skipped)
    } else {
        Ok(Placed::Mismatch)
    }
}

/// Equal lengths still get a content hash. Different lengths cannot match.
fn same_content(from: &Path, to: &Path) -> io::Result<bool> {
    if fs::metadata(from)?.len() != fs::metadata(to)?.len() {
        return Ok(false);
    }
    Ok(content_hash(from)? == content_hash(to)?)
}

fn content_hash(path: &Path) -> io::Result<blake3::Hash> {
    let file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(file)?;
    Ok(hasher.finalize())
}

/// Where the last sync wrote files that existed and did not match.
pub fn mismatch_list_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(
        PathBuf::from(home)
            .join(".rsyncstat")
            .join("mismatched-files.txt"),
    )
}

fn persist_mismatches(items: &[Mismatch]) -> Option<PathBuf> {
    let path = mismatch_list_path()?;
    if items.is_empty() {
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
        return None;
    }
    write_mismatch_list(&path, items).ok()?;
    Some(path)
}

fn write_mismatch_list(path: &Path, items: &[Mismatch]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut body = String::from(
        "# Files that already existed with a different hash. They were not overwritten.\n",
    );
    for item in items {
        body.push_str(item.direction);
        body.push(' ');
        body.push_str(&item.rel);
        body.push('\n');
    }
    fs::write(path, body)
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
            if child.excluded || rules.excludes(&rel, true) {
                *excluded += count_files(child);
                continue;
            }
            walk(child, &rel, rules, files, excluded);
        } else if child.excluded || rules.excludes(&rel, false) {
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
    fn patterns_become_gitignore_lines() {
        let lines = filter_lines("*.rs, src/**", "*.log, node_modules", false);
        assert!(lines.iter().any(|line| line == "*.log"));
        assert!(lines.iter().any(|line| line == "node_modules"));
        assert!(lines.iter().any(|line| line == "*"));
        assert!(lines.iter().any(|line| line == "!*/"));
        assert!(lines.iter().any(|line| line == "!*.rs"));
        assert!(lines.iter().any(|line| line == "!src/**"));
        assert!(filter_lines("", "../secret", false).is_empty());
    }

    #[test]
    fn include_patterns_keep_only_matching_files() {
        let (a, b) = sample_trees();
        let rules = rules_from_patterns("*.txt", "skip", false);
        let plan = plan_sync(&a, &b, SyncMethod::AtoB, &rules);
        let mut copied: Vec<_> = plan.a_to_b.iter().map(|op| op.rel.as_str()).collect();
        copied.sort();
        assert_eq!(copied, vec!["diff.txt", "only_a.txt"]);
    }

    #[test]
    fn one_way_copies_missing_and_different_sizes() {
        let (a, b) = sample_trees();
        let rules = rules_from_patterns("", "*.log, *.jpg, skip", false);
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
        let rules = rules_from_patterns("", "*.log, *.jpg, skip", false);
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
    fn stop_leaves_files_that_have_not_started() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rsyncstat-stop-{nanos}"));
        let a = root.join("a");
        let b = root.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("one.txt"), b"one").unwrap();
        fs::write(a.join("two.txt"), b"two").unwrap();
        let plan = SyncPlan {
            a_to_b: vec![
                CopyOp {
                    rel: "one.txt".into(),
                    bytes: 3,
                },
                CopyOp {
                    rel: "two.txt".into(),
                    bytes: 3,
                },
            ],
            ..SyncPlan::default()
        };
        let cancel = AtomicBool::new(false);
        let report = execute(
            &a,
            &b,
            &plan,
            &cancel,
            |done, _, _| {
                if done >= 1 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            |_| {},
        );
        assert!(report.stopped);
        assert_eq!(report.copied, 1);
        assert_eq!(fs::read(b.join("one.txt")).unwrap(), b"one");
        assert!(!b.join("two.txt").exists());
        let _ = fs::remove_dir_all(&root);
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

    #[test]
    fn existing_file_with_the_same_hash_is_skipped() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rsyncstat-same-{nanos}"));
        let a = root.join("a");
        let b = root.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("chapter.json"), b"{\"act\":2}").unwrap();
        fs::write(b.join("chapter.json"), b"{\"act\":2}").unwrap();
        let plan = SyncPlan {
            a_to_b: vec![CopyOp {
                rel: "chapter.json".into(),
                bytes: 9,
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
        assert_eq!(report.copied, 0);
        assert_eq!(report.skipped, 1);
        assert!(report.mismatched.is_empty());
        assert!(report.errors.is_empty());
        assert_eq!(fs::read(b.join("chapter.json")).unwrap(), b"{\"act\":2}");
        assert!(lines.iter().any(|line| line.contains("hash matches")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_file_with_a_different_hash_is_listed_and_kept() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rsyncstat-diff-{nanos}"));
        let a = root.join("a");
        let b = root.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("chapter.json"), b"source-a").unwrap();
        fs::write(b.join("chapter.json"), b"source-b").unwrap();
        let list = root.join("mismatched-files.txt");
        let plan = SyncPlan {
            a_to_b: vec![CopyOp {
                rel: "chapter.json".into(),
                bytes: 8,
            }],
            ..SyncPlan::default()
        };
        let report = execute(&a, &b, &plan, &AtomicBool::new(false), |_, _, _| {}, |_| {});
        assert_eq!(report.copied, 0);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.mismatched.len(), 1);
        assert_eq!(report.mismatched[0].rel, "chapter.json");
        assert_eq!(report.mismatched[0].direction, "A→B");
        assert_eq!(fs::read(b.join("chapter.json")).unwrap(), b"source-b");
        write_mismatch_list(&list, &report.mismatched).unwrap();
        let saved = fs::read_to_string(&list).unwrap();
        assert!(saved.contains("A→B chapter.json"));
        assert!(!saved.contains("destination"));
        let _ = fs::remove_dir_all(&root);
    }
}
