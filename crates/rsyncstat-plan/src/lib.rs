//! Compare two scanned trees and plan which files to copy.
//!
//! Pure logic: no file system access. A plan lists files that are missing or
//! differ on the target; nothing here deletes or overwrites.

use rsyncstat_filter::{FilterSet, PatternOptions};
use rsyncstat_model::TreeNode;
use std::collections::HashMap;
use std::path::Path;

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

pub fn rules_from_patterns(
    include: &str,
    include_opts: PatternOptions,
    exclude: &str,
    exclude_opts: PatternOptions,
    use_presets: bool,
) -> SyncRules {
    let (filter, _) =
        FilterSet::from_rules(include, include_opts, exclude, exclude_opts, use_presets);
    SyncRules { filter }
}

impl SyncRules {
    fn excludes(&self, rel: &str, is_dir: bool) -> bool {
        self.filter.verdict(Path::new(rel), is_dir).excluded
    }
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

/// Files to copy from source onto target. A file only on the target stays put.
pub fn plan_sync(source: &TreeNode, target: &TreeNode, rules: &SyncRules) -> SyncPlan {
    let (files_a, excluded_a) = collect_files(source, rules);
    let (files_b, excluded_b) = collect_files(target, rules);
    let mut plan = SyncPlan {
        excluded: excluded_a + excluded_b,
        ..SyncPlan::default()
    };
    for (rel, size_a) in &files_a {
        match files_b.get(rel) {
            Some(size_b) if size_b == size_a => plan.same += 1,
            Some(_) | None => plan.a_to_b.push(CopyOp {
                rel: rel.clone(),
                bytes: *size_a,
            }),
        }
    }
    plan.a_to_b.sort_by(|left, right| left.rel.cmp(&right.rel));
    plan
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
        let lines = rsyncstat_filter::gitignore_lines("*.rs, src/**", "*.log, node_modules", false);
        assert!(lines.iter().any(|line| line == "*.log"));
        assert!(lines.iter().any(|line| line == "node_modules"));
        assert!(lines.iter().any(|line| line == "*"));
        assert!(lines.iter().any(|line| line == "!*/"));
        assert!(lines.iter().any(|line| line == "!*.rs"));
        assert!(lines.iter().any(|line| line == "!src/**"));
        assert!(rsyncstat_filter::gitignore_lines("", "../secret", false).is_empty());
    }

    #[test]
    fn include_patterns_keep_only_matching_files() {
        let (a, b) = sample_trees();
        let rules = rules_from_patterns(
            "*.txt",
            PatternOptions::default(),
            "skip",
            PatternOptions::default(),
            false,
        );
        let plan = plan_sync(&a, &b, &rules);
        let mut copied: Vec<_> = plan.a_to_b.iter().map(|op| op.rel.as_str()).collect();
        copied.sort();
        assert_eq!(copied, vec!["diff.txt", "only_a.txt"]);
    }

    #[test]
    fn one_way_copies_missing_and_different_sizes() {
        let (a, b) = sample_trees();
        let rules = rules_from_patterns(
            "",
            PatternOptions::default(),
            "*.log, *.jpg, skip",
            PatternOptions::default(),
            false,
        );
        let plan = plan_sync(&a, &b, &rules);
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
}
