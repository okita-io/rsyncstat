//! Parallel directory walk with exclude pruning.
//!
//! Workers stat each entry once and push into a thread-local batch, then the UI
//! thread folds those batches into the tree. That keeps gitignore pruning and
//! avoids a shared lock on every path.

use ignore::{WalkBuilder, WalkState};
use rsyncstat_filter::FilterSet;
use rsyncstat_model::TreeNode;
use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

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
    Finished,
    Failed(String),
}

/// Run a parallel scan and return a tree rooted at `root`'s name.
pub fn scan_root(root: &Path, filter: &FilterSet) -> io::Result<TreeNode> {
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    scan_streaming(root, filter, tx, cancel)?;
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
) {
    thread::spawn(move || {
        if let Err(err) = scan_streaming(&root, filter.as_ref(), tx.clone(), cancel) {
            let _ = tx.send(ScanUpdate::Failed(err.to_string()));
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

    let filter = Arc::new(filter.clone());
    let mut walker = WalkBuilder::new(&root);
    walker.standard_filters(false);
    walker.hidden(false);
    walker.follow_links(false);
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
    walker.build_parallel().run(move || {
        let tx_visit = tx_visit.clone();
        let root_visit = root_visit.clone();
        let cancel_visit = Arc::clone(&cancel_visit);
        Box::new(move |result| {
            if cancel_visit.load(Ordering::Relaxed) {
                return WalkState::Quit;
            }
            let entry = match result {
                Ok(entry) => entry,
                Err(_) => return WalkState::Continue,
            };
            let path = entry.path();
            let Ok(rel) = path.strip_prefix(&root_visit) else {
                return WalkState::Continue;
            };
            if rel.as_os_str().is_empty() {
                return WalkState::Continue;
            }
            let (is_dir, size) = entry_kind(&entry);
            tls_push(
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

    let _ = tx.send(ScanUpdate::Finished);
    Ok(())
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

fn tls_push(tx: &crossbeam_channel::Sender<ScanUpdate>, entry: ScanEntry) {
    LOCAL.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(LocalBuf {
                tx: tx.clone(),
                buf: Vec::with_capacity(BATCH_ENTRIES),
                since: Instant::now(),
            });
        }
        slot.as_mut().unwrap().push(entry);
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
}
