//! Run a sync plan: copy files from source to target.
//!
//! Copies files that are missing on the target. When the target file already
//! exists, the contents are hashed: a match is skipped, and a different hash
//! is recorded and left in place. Nothing is overwritten or deleted.

use rsyncstat_plan::SyncPlan;
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
    report
}

/// Run [`execute`] on a background thread and stream [`SyncEvent`]s.
///
/// When `mismatch_list` is set, the list of files that already existed with
/// different bytes is written there (or the file is removed when the list is
/// empty).
pub fn spawn_sync(
    root_a: PathBuf,
    root_b: PathBuf,
    plan: SyncPlan,
    cancel: Arc<AtomicBool>,
    mismatch_list: Option<PathBuf>,
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
        if let Some(path) = persist_mismatches(mismatch_list.as_deref(), &report.mismatched) {
            write_recovery(&format!("mismatch list {}", path.display()), &tx_log);
        }
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
    refuse_symlinked_parents(to_root, &rel_path)?;
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

/// A symlinked directory on the target would carry the write outside `to_root`.
/// Refuse it rather than follow the link.
fn refuse_symlinked_parents(to_root: &Path, rel_path: &Path) -> io::Result<()> {
    let mut current = to_root.to_path_buf();
    let depth = rel_path.components().count().saturating_sub(1);
    for part in rel_path.components().take(depth) {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "refusing to write through a symlinked directory",
                ));
            }
            Ok(_) => {}
            // Not created yet, so nothing below it can be a link either.
            Err(err) if err.kind() == io::ErrorKind::NotFound => break,
            Err(err) => return Err(err),
        }
    }
    Ok(())
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
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(
        PathBuf::from(home)
            .join(".rsyncstat")
            .join("mismatched-files.txt"),
    )
}

fn persist_mismatches(path: Option<&Path>, items: &[Mismatch]) -> Option<PathBuf> {
    let path = path?;
    if items.is_empty() {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
        return None;
    }
    write_mismatch_list(path, items).ok()?;
    Some(path.to_path_buf())
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

#[cfg(test)]
mod tests {
    use super::*;
    use rsyncstat_plan::CopyOp;
    use std::time::{SystemTime, UNIX_EPOCH};

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
