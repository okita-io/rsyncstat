//! Parallel directory walk with exclude pruning.

use ignore::WalkBuilder;
use rsyncstat_filter::FilterSet;
use rsyncstat_model::TreeNode;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Clone, Debug)]
pub struct ScanProgress {
    pub paths_seen: u64,
    pub bytes_indexed: u64,
    pub excluded_dirs: u64,
}

#[derive(Clone, Debug)]
struct FlatEntry {
    rel: PathBuf,
    size: u64,
    is_dir: bool,
    excluded: bool,
    rule: Option<String>,
}

/// Run a parallel scan and return a tree rooted at `root`'s name.
pub fn scan_root(root: &Path, filter: &FilterSet) -> std::io::Result<TreeNode> {
    let root = root.canonicalize()?;
    let root_name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());

    let filter = Arc::new(filter.clone());
    let flat: Arc<Mutex<Vec<FlatEntry>>> = Arc::new(Mutex::new(Vec::new()));

    let mut walker = WalkBuilder::new(&root);
    walker.standard_filters(false);
    walker.hidden(false);
    walker.threads(num_cpus::get().max(2));

    let prune_flat = Arc::clone(&flat);
    let walk_filter = Arc::clone(&filter);
    let walk_base = root.clone();
    walker.filter_entry(move |entry| {
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(&walk_base) else {
            return true;
        };
        if rel.as_os_str().is_empty() {
            return true;
        }
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        let verdict = walk_filter.verdict(rel, is_dir);
        if verdict.excluded {
            let size = if is_dir {
                0
            } else {
                entry.metadata().map(|m| m.len()).unwrap_or(0)
            };
            prune_flat.lock().unwrap().push(FlatEntry {
                rel: rel.to_path_buf(),
                size,
                is_dir,
                excluded: true,
                rule: verdict.pattern.clone(),
            });
            return false;
        }
        true
    });

    for result in walker.build() {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(&root) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        let verdict = filter.verdict(rel, is_dir);
        if verdict.excluded {
            continue;
        }
        let size = if is_dir {
            0
        } else {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        };
        flat.lock().unwrap().push(FlatEntry {
            rel: rel.to_path_buf(),
            size,
            is_dir,
            excluded: false,
            rule: None,
        });
    }

    let entries = Arc::try_unwrap(flat).ok().unwrap().into_inner().unwrap();
    Ok(build_tree(&root_name, entries))
}

/// Scan on a background thread, sending the finished tree when done.
pub fn scan_root_async(
    root: PathBuf,
    filter: Arc<FilterSet>,
    done_tx: crossbeam_channel::Sender<std::io::Result<TreeNode>>,
) {
    thread::spawn(move || {
        let result = scan_root(&root, filter.as_ref());
        let _ = done_tx.send(result);
    });
}

fn build_tree(root_name: &str, mut flat: Vec<FlatEntry>) -> TreeNode {
    flat.sort_by(|a, b| a.rel.cmp(&b.rel));
    let mut root = TreeNode::new_dir(root_name);
    for entry in flat {
        insert_path(&mut root, &entry);
    }
    rollup_sizes(&mut root);
    root
}

fn insert_path(root: &mut TreeNode, entry: &FlatEntry) {
    let components: Vec<_> = entry.rel.components().collect();
    let mut node = root;
    for (i, comp) in components.iter().enumerate() {
        let name = comp.as_os_str().to_string_lossy().into_owned();
        let last = i + 1 == components.len();
        if last {
            if entry.is_dir {
                if let Some(child) = node
                    .children
                    .iter_mut()
                    .find(|c| c.name == name && c.is_dir)
                {
                    if entry.excluded {
                        child.excluded = true;
                        child.exclude_rule = entry.rule.clone();
                    }
                } else {
                    let new_dir = if entry.excluded {
                        TreeNode::excluded_dir(name, entry.rule.clone())
                    } else {
                        TreeNode::new_dir(name)
                    };
                    node.children.push(new_dir);
                }
            } else {
                node.children.push(TreeNode::file(name, entry.size));
            }
        } else if let Some(idx) = node
            .children
            .iter()
            .position(|c| c.name == name && c.is_dir)
        {
            node = &mut node.children[idx];
        } else {
            node.children.push(TreeNode::new_dir(name));
            node = node.children.last_mut().unwrap();
        }
    }
}

fn rollup_sizes(node: &mut TreeNode) -> u64 {
    if node.excluded {
        node.size = 0;
        return 0;
    }
    if !node.children.is_empty() {
        let s: u64 = node.children.iter_mut().map(rollup_sizes).sum();
        node.size = s;
        s
    } else {
        node.size
    }
}
