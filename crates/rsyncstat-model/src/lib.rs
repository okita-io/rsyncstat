//! Shared scan tree model, file-type colors and display helpers.

mod file_color;
mod format;
mod path_display;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub use file_color::{file_color_rgb, file_type_id, readable_color_rgb, FileTypeId};
pub use format::format_bytes;
pub use path_display::elide_path_for_width;

/// Stable node identifier for future arena storage.
/// A node in a directory tree built from a scan.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TreeNode {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub excluded: bool,
    /// Human-readable hint for which rule excluded this path (the pattern text).
    pub exclude_rule: Option<String>,
    pub children: Vec<TreeNode>,
    /// Child name → index. Present only after a child has been inserted.
    // Boxed so a leaf node stays one pointer wide.
    #[allow(clippy::box_collection)]
    #[serde(skip)]
    child_index: Option<Box<HashMap<String, usize>>>,
}

impl TreeNode {
    pub fn new_dir(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            is_dir: true,
            ..Default::default()
        }
    }

    pub fn file(name: impl Into<String>, size: u64) -> Self {
        Self {
            name: name.into(),
            size,
            is_dir: false,
            ..Default::default()
        }
    }

    pub fn excluded_dir(name: impl Into<String>, rule: Option<String>) -> Self {
        Self {
            name: name.into(),
            is_dir: true,
            excluded: true,
            exclude_rule: rule,
            ..Default::default()
        }
    }

    pub fn total_size(&self) -> u64 {
        if self.excluded && self.is_dir {
            return 0;
        }
        self.size
    }

    /// Direct child lookup. Uses the scan index when this directory has one.
    pub fn child(&self, name: &str) -> Option<&TreeNode> {
        if let Some(index) = &self.child_index {
            return index.get(name).map(|&i| &self.children[i]);
        }
        self.children.iter().find(|c| c.name == name)
    }

    /// Follow `path` down from this node. The first segment must be this node's name.
    pub fn find_by_path(&self, path: &[String]) -> Option<&TreeNode> {
        if path.first()? != &self.name {
            return None;
        }
        let mut node = self;
        for part in path.iter().skip(1) {
            node = node.child(part)?;
        }
        Some(node)
    }

    /// Insert one path discovered by the scanner.
    ///
    /// File sizes roll up through ancestors as the path is created, so a partial
    /// tree can be drawn before the walk finishes. Excluded directories contribute
    /// no bytes. Excluded files keep their size so they still appear in the map.
    pub fn insert_scanned(
        &mut self,
        rel: &Path,
        size: u64,
        is_dir: bool,
        excluded: bool,
        rule: Option<String>,
    ) {
        let names: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if names.is_empty() {
            return;
        }
        let delta = insert_at(self, &names, size, is_dir, excluded, &rule);
        add_size(self, delta);
    }

    /// Alphabetical children, after a scan finishes. Rebuilds the name index.
    pub fn sort_children_recursive(&mut self) {
        self.children.sort_by(|a, b| a.name.cmp(&b.name));
        self.reindex();
        for child in &mut self.children {
            child.sort_children_recursive();
        }
    }

    fn child_pos(&self, name: &str) -> Option<usize> {
        self.child_index.as_ref().and_then(|m| m.get(name).copied())
    }

    fn push_child(&mut self, child: TreeNode) -> usize {
        let idx = self.children.len();
        let name = child.name.clone();
        self.children.push(child);
        self.child_index
            .get_or_insert_with(|| Box::new(HashMap::with_capacity(4)))
            .insert(name, idx);
        idx
    }

    fn reindex(&mut self) {
        if self.children.is_empty() {
            self.child_index = None;
            return;
        }
        let map = self
            .child_index
            .get_or_insert_with(|| Box::new(HashMap::with_capacity(self.children.len())));
        map.clear();
        for (i, child) in self.children.iter().enumerate() {
            map.insert(child.name.clone(), i);
        }
    }
}

fn add_size(node: &mut TreeNode, delta: i64) {
    if delta >= 0 {
        node.size = node.size.saturating_add(delta as u64);
    } else {
        node.size = node.size.saturating_sub((-delta) as u64);
    }
}

/// Returns the size delta already applied to `node`'s descendants, but not to `node` itself.
/// The caller adds that delta to `node`.
fn insert_at(
    node: &mut TreeNode,
    names: &[String],
    size: u64,
    is_dir: bool,
    excluded: bool,
    rule: &Option<String>,
) -> i64 {
    let name = &names[0];
    let last = names.len() == 1;
    if let Some(idx) = node.child_pos(name) {
        if last && is_dir {
            let child = &mut node.children[idx];
            child.is_dir = true;
            if excluded {
                child.excluded = true;
                child.exclude_rule = rule.clone();
            }
            return 0;
        }
        if last {
            let child = &mut node.children[idx];
            let old = child.size;
            child.size = size;
            child.is_dir = false;
            child.excluded = excluded;
            return size as i64 - old as i64;
        }
        let delta = insert_at(
            &mut node.children[idx],
            &names[1..],
            size,
            is_dir,
            excluded,
            rule,
        );
        add_size(&mut node.children[idx], delta);
        return delta;
    }

    if last && is_dir {
        let child = if excluded {
            TreeNode::excluded_dir(name.clone(), rule.clone())
        } else {
            TreeNode::new_dir(name.clone())
        };
        node.push_child(child);
        return 0;
    }
    if last {
        let mut child = TreeNode::file(name.clone(), size);
        child.excluded = excluded;
        node.push_child(child);
        return size as i64;
    }

    let idx = node.push_child(TreeNode::new_dir(name.clone()));
    let delta = insert_at(
        &mut node.children[idx],
        &names[1..],
        size,
        is_dir,
        excluded,
        rule,
    );
    add_size(&mut node.children[idx], delta);
    delta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_rolls_sizes_through_ancestors() {
        let mut root = TreeNode::new_dir("root");
        root.insert_scanned(Path::new("b/file.txt"), 10, false, false, None);
        root.insert_scanned(Path::new("b/other.txt"), 7, false, false, None);
        root.insert_scanned(Path::new("a/file.txt"), 5, false, false, None);
        root.insert_scanned(Path::new("skip"), 0, true, true, Some("skip/".into()));
        root.insert_scanned(
            Path::new(".DS_Store"),
            4,
            false,
            true,
            Some(".DS_Store".into()),
        );

        assert_eq!(root.size, 26);
        assert_eq!(root.child("b").unwrap().size, 17);
        assert_eq!(root.child("b").unwrap().child("other.txt").unwrap().size, 7);
        assert!(root.child("skip").unwrap().excluded);
        assert!(root.child("skip").unwrap().children.is_empty());
        assert!(root.child(".DS_Store").unwrap().excluded);
        assert_eq!(root.child(".DS_Store").unwrap().size, 4);

        root.sort_children_recursive();
        assert_eq!(root.children[0].name, ".DS_Store");
        assert_eq!(root.child("b").unwrap().size, 17);
        assert_eq!(root.child("a").unwrap().child("file.txt").unwrap().size, 5);
    }
}
