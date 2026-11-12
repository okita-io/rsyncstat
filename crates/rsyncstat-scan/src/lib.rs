//! Parallel directory walk with exclude pruning.
//!
//! Workers stat each entry once and push into a thread-local batch, then the UI
//! thread folds those batches into the tree. That keeps gitignore pruning and
//! avoids a shared lock on every path.

use ignore::{WalkBuilder, WalkState};
use rsyncstat_filter::FilterSet;
use rsyncstat_model::TreeNode;
use std::cell::RefCell;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const READ_ATTEMPTS: u32 = 3;
const MAX_RECOVERY_NOTES: usize = 40;

const BATCH_ENTRIES: usize = 128;
const DIR_FLUSH_AT: usize = 8;
const FLUSH_AFTER: Duration = Duration::from_millis(40);

/// One path discovered by the walker. `rel` is relative to the scan root.
#[derive(Clone, Debug)]
pub struct ScanEntry {
    pub rel: PathBuf,
    pub size: u64,
    pub is_dir: bool,
    pub excluded: bool,
    pub rule: Option<String>,
}

/// Incremental results from [`scan_root_async`].
#[derive(Debug)]
pub enum ScanUpdate {
    /// Canonical root name. Always sent before any [`ScanUpdate::Batch`].
    Started {
        root_name: String,
    },
    Batch(Vec<ScanEntry>),
    /// Absolute path currently being visited (best-effort during parallel walk).
    Visiting(PathBuf),
    /// A directory read failed and is being retried, or the retry gave up.
    Notice(String),
    Finished,
    Failed(String),
}

/// Run a parallel scan and return a tree rooted at `root`'s name.
pub fn scan_root(root: &Path, filter: &FilterSet) -> io::Result<TreeNode> {
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    scan_streaming(root, filter, tx, cancel, false)?;
    let mut tree = None;
    while let Ok(update) = rx.recv() {
        match update {
            ScanUpdate::Started { root_name } => tree = Some(TreeNode::new_dir(root_name)),
            ScanUpdate::Batch(entries) => {
                let tree = tree
                    .as_mut()
                    .ok_or_else(|| io::Error::other("scan batch arrived before the root name"))?;
                for entry in entries {
                    tree.insert_scanned(
                        &entry.rel,
                        entry.size,
                        entry.is_dir,
                        entry.excluded,
                        entry.rule,
                    );
                }
            }
            ScanUpdate::Visiting(_) | ScanUpdate::Notice(_) => {}
            ScanUpdate::Finished => break,
            ScanUpdate::Failed(message) => return Err(io::Error::other(message)),
        }
    }
    let mut tree = tree.ok_or_else(|| io::Error::other("scan produced no tree"))?;
    tree.sort_children_recursive();
    Ok(tree)
}

/// Scan on a background thread, streaming batches as directories are read.
///
/// `cancel` stops workers cooperatively. In-flight batches may still arrive
/// on `tx` until the walk unwinds.
pub fn scan_root_async(
    root: PathBuf,
    filter: Arc<FilterSet>,
    tx: crossbeam_channel::Sender<ScanUpdate>,
    cancel: Arc<AtomicBool>,
    same_file_system: bool,
) {
    thread::spawn(move || {
        let tx_fail = tx.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scan_streaming(&root, filter.as_ref(), tx, cancel, same_file_system)
        }));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                let _ = tx_fail.send(ScanUpdate::Failed(err.to_string()));
            }
            Err(_) => {
                let _ = tx_fail.send(ScanUpdate::Failed(
                    "scan stopped unexpectedly; press Rescan to try again".to_string(),
                ));
            }
        }
    });
}

fn worker_threads() -> usize {
    // Directory reads are latency-bound on APFS and SSDs. Two workers per core
    // keeps the disk queue full; the cap avoids hundreds of threads on big machines.
    num_cpus::get().saturating_mul(2).clamp(2, 32)
}

fn scan_streaming(
    root: &Path,
    filter: &FilterSet,
    tx: crossbeam_channel::Sender<ScanUpdate>,
    cancel: Arc<AtomicBool>,
    same_file_system: bool,
) -> io::Result<()> {
    let root = root.canonicalize()?;
    let root_name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());
    let _ = tx.send(ScanUpdate::Started { root_name });
    if cancel.load(Ordering::Relaxed) {
        let _ = tx.send(ScanUpdate::Finished);
        return Ok(());
    }

    // Each scan gets its own id so a worker thread left over from the previous
    // walk flushes into that walk's channel instead of this one.
    static SCAN_IDS: AtomicU64 = AtomicU64::new(1);
    let scan_id = SCAN_IDS.fetch_add(1, Ordering::Relaxed);
    let faults = Arc::new(Mutex::new(Vec::<PathBuf>::new()));

    let filter = Arc::new(filter.clone());
    let mut walker = WalkBuilder::new(&root);
    walker.standard_filters(false);
    walker.hidden(false);
    walker.follow_links(false);
    // A whole-drive scan stays on that volume so another mounted disk is not
    // pulled into the map or compared against this volume's free space.
    walker.same_file_system(same_file_system);
    walker.threads(worker_threads());

    let filter_for_walk = Arc::clone(&filter);
    let tx_filter = tx.clone();
    let root_filter = root.clone();
    let cancel_filter = Arc::clone(&cancel);
    walker.filter_entry(move |entry| {
        if cancel_filter.load(Ordering::Relaxed) {
            return false;
        }
        let path = entry.path();
        let _ = tx_filter.try_send(ScanUpdate::Visiting(path.to_path_buf()));
        let Ok(rel) = path.strip_prefix(&root_filter) else {
            return true;
        };
        if rel.as_os_str().is_empty() {
            return true;
        }
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        let verdict = filter_for_walk.verdict(rel, is_dir);
        if !verdict.excluded {
            return true;
        }
        // Stat only the entries we are about to drop. Included paths are
        // measured once, later, on the worker that visits them.
        let size = if is_dir {
            0
        } else {
            entry.metadata().map(|meta| meta.len()).unwrap_or(0)
        };
        tls_push(
            scan_id,
            &tx_filter,
            ScanEntry {
                rel: rel.to_path_buf(),
                size,
                is_dir,
                excluded: true,
                rule: verdict.pattern,
            },
        );
        false
    });

    let tx_visit = tx.clone();
    let root_visit = root.clone();
    let cancel_visit = Arc::clone(&cancel);
    let faults_visit = Arc::clone(&faults);
    walker.build_parallel().run(move || {
        let tx_visit = tx_visit.clone();
        let root_visit = root_visit.clone();
        let cancel_visit = Arc::clone(&cancel_visit);
        let faults_visit = Arc::clone(&faults_visit);
        Box::new(move |result| {
            if cancel_visit.load(Ordering::Relaxed) {
                return WalkState::Quit;
            }
            let entry = match result {
                Ok(entry) => entry,
                Err(err) => {
                    if let Some(path) = error_path(&err) {
                        if let Ok(mut faults) = faults_visit.lock() {
                            faults.push(path);
                        }
                    }
                    return WalkState::Continue;
                }
            };
            let path = entry.path();
            let _ = tx_visit.try_send(ScanUpdate::Visiting(path.to_path_buf()));
            let Ok(rel) = path.strip_prefix(&root_visit) else {
                return WalkState::Continue;
            };
            if rel.as_os_str().is_empty() {
                return WalkState::Continue;
            }
            let (is_dir, size) = entry_kind(&entry);
            tls_push(
                scan_id,
                &tx_visit,
                ScanEntry {
                    rel: rel.to_path_buf(),
                    size,
                    is_dir,
                    excluded: false,
                    rule: None,
                },
            );
            WalkState::Continue
        })
    });

    let pending = faults
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .drain(..)
        .collect();
    retry_unreadable(
        &root,
        filter.as_ref(),
        &tx,
        &cancel,
        same_file_system,
        scan_id,
        pending,
    );
    tls_flush();

    let _ = tx.send(ScanUpdate::Finished);
    Ok(())
}

fn error_path(err: &ignore::Error) -> Option<PathBuf> {
    match err {
        ignore::Error::WithPath { path, err } => error_path(err).or_else(|| Some(path.clone())),
        ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
            error_path(err)
        }
        ignore::Error::Partial(errs) => errs.iter().find_map(error_path),
        _ => None,
    }
}

/// Read directories the first walk could not list, and record what happened.
fn retry_unreadable(
    root: &Path,
    filter: &FilterSet,
    tx: &crossbeam_channel::Sender<ScanUpdate>,
    cancel: &AtomicBool,
    same_file_system: bool,
    scan_id: u64,
    pending: Vec<PathBuf>,
) {
    let mut jobs: Vec<(PathBuf, u32)> = unique_paths(pending)
        .into_iter()
        .map(|path| (path, 0))
        .collect();
    if jobs.is_empty() {
        return;
    }
    let root_device = device_id(root);
    let mut notes = 0usize;
    let mut slept_through = 0u32;
    note(
        tx,
        &mut notes,
        format!(
            "recovery: {} director{} could not be read; retrying",
            jobs.len(),
            if jobs.len() == 1 { "y" } else { "ies" }
        ),
    );
    while let Some((path, tried)) = jobs.pop() {
        if cancel.load(Ordering::Relaxed) {
            note(
                tx,
                &mut notes,
                format!("stopped retrying {}", path.display()),
            );
            break;
        }
        let attempt = tried + 1;
        if attempt > slept_through {
            if attempt > 1 {
                thread::sleep(Duration::from_millis(40 * u64::from(attempt)));
            }
            slept_through = attempt;
        }
        note(
            tx,
            &mut notes,
            format!("retry {attempt}/{READ_ATTEMPTS} reading {}", path.display()),
        );
        match reread(
            root,
            &path,
            filter,
            tx,
            cancel,
            same_file_system,
            root_device,
            scan_id,
        ) {
            Ok(children) => {
                note(tx, &mut notes, format!("recovered read {}", path.display()));
                for child in unique_paths(children) {
                    jobs.push((child, 0));
                }
            }
            Err(err) => {
                note(
                    tx,
                    &mut notes,
                    format!(
                        "attempt {attempt}/{READ_ATTEMPTS} {}: {err}",
                        path.display()
                    ),
                );
                if attempt < READ_ATTEMPTS {
                    jobs.push((path, attempt));
                } else {
                    note(
                        tx,
                        &mut notes,
                        format!("gave up reading {}", path.display()),
                    );
                }
            }
        }
    }
}

fn unique_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for path in paths {
        if seen.insert(path.clone()) {
            out.push(path);
        }
    }
    out
}

fn note(tx: &crossbeam_channel::Sender<ScanUpdate>, sent: &mut usize, line: String) {
    const MAX_LOG_LINES: usize = 200;
    if *sent == MAX_LOG_LINES {
        let line = "recovery: further directory retries omitted from the log".to_string();
        eprintln!("rsyncstat: {line}");
        #[cfg(not(test))]
        {
            let _ = append_recovery_log(&line);
        }
        let _ = tx.send(ScanUpdate::Notice(line));
    }
    *sent += 1;
    if *sent > MAX_LOG_LINES {
        return;
    }
    eprintln!("rsyncstat: {line}");
    #[cfg(not(test))]
    {
        let _ = append_recovery_log(&line);
    }
    if *sent <= MAX_RECOVERY_NOTES {
        let _ = tx.send(ScanUpdate::Notice(line));
    }
}

/// Path of the on-disk recovery log, created on the first failed read or copy.
pub fn recovery_log_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".rsyncstat").join("recovery.log"))
}

/// Append one recovery line. Returns the log path when the write succeeds.
pub fn append_recovery_log(line: &str) -> Option<PathBuf> {
    let path = recovery_log_path()?;
    append_recovery_line(&path, line).ok()?;
    Some(path)
}

fn append_recovery_line(path: &Path, line: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")?;
    file.flush()?;
    Ok(())
}

/// Re-read `failed`. `Ok` lists child directories that still could not be read.
///
/// The directory that failed the first walk is already in the tree, so it is
/// not emitted again. Its children are.
fn reread(
    root: &Path,
    failed: &Path,
    filter: &FilterSet,
    tx: &crossbeam_channel::Sender<ScanUpdate>,
    cancel: &AtomicBool,
    same_file_system: bool,
    root_device: Option<u64>,
    scan_id: u64,
) -> Result<Vec<PathBuf>, String> {
    let meta = fs::symlink_metadata(failed).map_err(|err| err.to_string())?;
    let is_link = meta.file_type().is_symlink();
    let is_dir = meta.is_dir() && !is_link;
    if !is_dir {
        emit_scanned(root, failed, filter, tx, scan_id, false, meta.len());
        return Ok(Vec::new());
    }
    if crosses_devices(same_file_system, root_device, failed) {
        return Ok(Vec::new());
    }
    let listed = fs::read_dir(failed).map_err(|err| err.to_string())?;
    let mut stack = Vec::new();
    for entry in listed {
        match entry {
            Ok(entry) => stack.push(entry.path()),
            Err(err) => return Err(err.to_string()),
        }
    }
    let mut still_failed = Vec::new();
    while let Some(path) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(_) => {
                still_failed.push(path);
                continue;
            }
        };
        let is_link = meta.file_type().is_symlink();
        let is_dir = meta.is_dir() && !is_link;
        let size = if is_dir { 0 } else { meta.len() };
        emit_scanned(root, &path, filter, tx, scan_id, is_dir, size);
        if !is_dir {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => continue,
        };
        if filter.verdict(&rel, true).excluded {
            continue;
        }
        if crosses_devices(same_file_system, root_device, &path) {
            continue;
        }
        match fs::read_dir(&path) {
            Ok(listed) => {
                for entry in listed {
                    match entry {
                        Ok(entry) => stack.push(entry.path()),
                        Err(_) => still_failed.push(path.clone()),
                    }
                }
            }
            Err(_) => still_failed.push(path),
        }
    }
    Ok(still_failed)
}

fn emit_scanned(
    root: &Path,
    path: &Path,
    filter: &FilterSet,
    tx: &crossbeam_channel::Sender<ScanUpdate>,
    scan_id: u64,
    is_dir: bool,
    size: u64,
) {
    let Ok(rel) = path.strip_prefix(root) else {
        return;
    };
    if rel.as_os_str().is_empty() {
        return;
    }
    let verdict = filter.verdict(rel, is_dir);
    tls_push(
        scan_id,
        tx,
        ScanEntry {
            rel: rel.to_path_buf(),
            size: if is_dir { 0 } else { size },
            is_dir,
            excluded: verdict.excluded,
            rule: verdict.pattern,
        },
    );
}

fn crosses_devices(same_file_system: bool, root_device: Option<u64>, path: &Path) -> bool {
    if !same_file_system {
        return false;
    }
    match (root_device, device_id(path)) {
        (Some(root_dev), Some(dev)) => dev != root_dev,
        _ => false,
    }
}

fn device_id(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).ok().map(|meta| meta.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

fn entry_kind(entry: &ignore::DirEntry) -> (bool, u64) {
    match entry.file_type() {
        Some(kind) if kind.is_dir() => (true, 0),
        Some(_) => (false, entry.metadata().map(|m| m.len()).unwrap_or(0)),
        None => match entry.metadata() {
            Ok(meta) => (meta.is_dir(), if meta.is_dir() { 0 } else { meta.len() }),
            Err(_) => (false, 0),
        },
    }
}

struct LocalBuf {
    scan_id: u64,
    tx: crossbeam_channel::Sender<ScanUpdate>,
    buf: Vec<ScanEntry>,
    since: Instant,
}

impl LocalBuf {
    fn push(&mut self, entry: ScanEntry) {
        let is_dir = entry.is_dir;
        if self.buf.is_empty() {
            self.since = Instant::now();
        }
        self.buf.push(entry);
        let due = self.buf.len() >= BATCH_ENTRIES
            || self.since.elapsed() >= FLUSH_AFTER
            || (is_dir && self.buf.len() >= DIR_FLUSH_AT);
        if due {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let entries = std::mem::take(&mut self.buf);
        let _ = self.tx.send(ScanUpdate::Batch(entries));
    }
}

impl Drop for LocalBuf {
    fn drop(&mut self) {
        self.flush();
    }
}

thread_local! {
    static LOCAL: RefCell<Option<LocalBuf>> = const { RefCell::new(None) };
}

fn tls_push(scan_id: u64, tx: &crossbeam_channel::Sender<ScanUpdate>, entry: ScanEntry) {
    LOCAL.with(|slot| {
        let mut slot = slot.borrow_mut();
        let stale = slot.as_ref().is_some_and(|buf| buf.scan_id != scan_id);
        if stale {
            if let Some(mut old) = slot.take() {
                old.flush();
            }
        }
        if slot.is_none() {
            *slot = Some(LocalBuf {
                scan_id,
                tx: tx.clone(),
                buf: Vec::with_capacity(BATCH_ENTRIES),
                since: Instant::now(),
            });
        }
        slot.as_mut().unwrap().push(entry);
    });
}

fn tls_flush() {
    LOCAL.with(|slot| {
        if let Some(buf) = slot.borrow_mut().as_mut() {
            buf.flush();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scan_prunes_excluded_dirs_and_keeps_file_sizes() {
        let dir = std::env::temp_dir().join(format!(
            "rsyncstat-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let tmp = Tmp(dir.clone());
        std::fs::create_dir_all(tmp.0.join("src")).unwrap();
        std::fs::create_dir_all(tmp.0.join("wide")).unwrap();
        std::fs::create_dir_all(tmp.0.join("node_modules/pkg")).unwrap();
        let main_bytes = b"fn main() {}";
        let junk = b"junk";
        let hidden = b"secret";
        std::fs::write(tmp.0.join("src/main.rs"), main_bytes).unwrap();
        std::fs::write(tmp.0.join(".DS_Store"), junk).unwrap();
        std::fs::write(tmp.0.join("node_modules/pkg/index.js"), hidden).unwrap();
        for i in 0..40 {
            std::fs::write(tmp.0.join("wide").join(format!("f{i}")), vec![0u8; 10]).unwrap();
        }

        let tree = scan_root(&tmp.0, &FilterSet::default_presets()).unwrap();
        let expected = main_bytes.len() as u64 + junk.len() as u64 + 40 * 10;
        assert_eq!(tree.size, expected);
        assert_eq!(
            tree.child("src")
                .and_then(|n| n.child("main.rs"))
                .map(|n| n.size),
            Some(main_bytes.len() as u64)
        );
        let modules = tree
            .child("node_modules")
            .expect("excluded dir is recorded");
        assert!(modules.excluded);
        assert!(modules.children.is_empty());
        assert_eq!(tree.child("wide").map(|n| n.children.len()), Some(40));
    }

    #[test]
    fn second_scan_rereads_directories() {
        let dir = std::env::temp_dir().join(format!(
            "rsyncstat-rescan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let tmp = Tmp(dir.clone());
        std::fs::create_dir_all(&tmp.0).unwrap();
        std::fs::write(tmp.0.join("a.txt"), b"one").unwrap();

        let first = scan_root(&tmp.0, &FilterSet::default_presets()).unwrap();
        assert_eq!(first.child("a.txt").map(|node| node.size), Some(3));
        assert!(first.child("b.txt").is_none());

        std::fs::write(tmp.0.join("b.txt"), b"four").unwrap();
        std::fs::write(tmp.0.join("a.txt"), b"longer").unwrap();
        let second = scan_root(&tmp.0, &FilterSet::default_presets()).unwrap();
        assert_eq!(second.child("a.txt").map(|node| node.size), Some(6));
        assert_eq!(second.child("b.txt").map(|node| node.size), Some(4));

        std::fs::remove_file(tmp.0.join("a.txt")).unwrap();
        let third = scan_root(&tmp.0, &FilterSet::default_presets()).unwrap();
        assert!(third.child("a.txt").is_none());
        assert_eq!(third.child("b.txt").map(|node| node.size), Some(4));
    }

    #[test]
    fn recovery_line_appends_to_the_given_file() {
        let path = std::env::temp_dir().join(format!(
            "rsyncstat-recovery-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        append_recovery_line(&path, "attempt 1/3 notes/a.txt: busy").unwrap();
        append_recovery_line(&path, "recovered read notes").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(text.contains("attempt 1/3 notes/a.txt: busy"));
        assert!(text.contains("recovered read notes"));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_is_logged_and_retried() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "rsyncstat-locked-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let tmp = Tmp(dir.clone());
        let locked = tmp.0.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("secret.txt"), b"hidden").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        struct Restore(PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        let _restore = Restore(locked.clone());

        let (tx, rx) = crossbeam_channel::unbounded();
        scan_root_async(
            tmp.0.clone(),
            Arc::new(FilterSet::default_presets()),
            tx,
            Arc::new(AtomicBool::new(false)),
            false,
        );
        let mut tree = None;
        let mut notices = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(ScanUpdate::Started { root_name }) => tree = Some(TreeNode::new_dir(root_name)),
                Ok(ScanUpdate::Batch(entries)) => {
                    let tree = tree.as_mut().expect("batch before start");
                    for entry in entries {
                        tree.insert_scanned(
                            &entry.rel,
                            entry.size,
                            entry.is_dir,
                            entry.excluded,
                            entry.rule,
                        );
                    }
                }
                Ok(ScanUpdate::Notice(message)) => notices.push(message),
                Ok(ScanUpdate::Visiting(_)) => {}
                Ok(ScanUpdate::Finished) => break,
                Ok(ScanUpdate::Failed(message)) => panic!("{message}"),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(err) => panic!("{err}"),
            }
        }
        let tree = tree.expect("scan produced no tree");
        let saw_file = tree
            .child("locked")
            .and_then(|node| node.child("secret.txt"))
            .is_some();
        let saw_retry = notices.iter().any(|line| line.contains("retry"));
        assert!(
            saw_file || saw_retry,
            "expected the file or a retry notice, tree children {:?}, notices {notices:?}",
            tree.children
                .iter()
                .map(|child| &child.name)
                .collect::<Vec<_>>()
        );
    }
}
