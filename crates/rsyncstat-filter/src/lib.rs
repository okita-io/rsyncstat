//! Gitignore-style exclude rules compiled with the `ignore` crate.

mod presets;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use presets::{default_preset_rules, PresetGroup};

/// Where a matching rule came from (more levels in M1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleLevel {
    BuiltinPreset,
    Profile,
    Global,
    RsyncIgnore,
}

/// Result of evaluating a path against the active filter set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub excluded: bool,
    pub level: Option<RuleLevel>,
    pub line_number: Option<u64>,
    pub pattern: Option<String>,
    /// Set when a parent directory is excluded and the path is never entered.
    pub via_parent: Option<PathBuf>,
}

impl Verdict {
    pub fn included() -> Self {
        Self {
            excluded: false,
            level: None,
            line_number: None,
            pattern: None,
            via_parent: None,
        }
    }
}

/// Compiled filter rules (M0: built-in presets plus optional extra lines).
#[derive(Debug, Clone)]
pub struct FilterSet {
    gitignore: Arc<Gitignore>,
    level: RuleLevel,
}

impl FilterSet {
    /// Build from the default built-in preset rule list.
    pub fn default_presets() -> Self {
        Self::from_lines(&default_preset_rules(), RuleLevel::BuiltinPreset)
    }

    pub fn from_lines(lines: &[&str], level: RuleLevel) -> Self {
        let mut builder = GitignoreBuilder::new(".");
        for line in lines {
            builder.add_line(None, line).expect("invalid preset line");
        }
        let gitignore = Arc::new(builder.build().expect("failed to build gitignore"));
        Self { gitignore, level }
    }

    /// Evaluate `rel` (relative to scan root, platform path separators allowed).
    pub fn verdict(&self, rel: &Path, is_dir: bool) -> Verdict {
        if let Some(parent) = self.excluded_ancestor(rel) {
            return Verdict {
                excluded: true,
                level: Some(self.level.clone()),
                line_number: None,
                pattern: None,
                via_parent: Some(parent),
            };
        }
        self.match_path(rel, is_dir)
    }

    /// Whether a directory should be pruned during scan (excluded folder).
    pub fn should_prune_dir(&self, rel: &Path) -> bool {
        self.verdict(rel, true).excluded
    }

    fn excluded_ancestor(&self, rel: &Path) -> Option<PathBuf> {
        let mut p = rel.to_path_buf();
        while p.pop() {
            if p.as_os_str().is_empty() {
                break;
            }
            let v = self.match_path(&p, true);
            if v.excluded {
                return Some(p);
            }
        }
        None
    }

    fn match_path(&self, rel: &Path, is_dir: bool) -> Verdict {
        let m = self.gitignore.matched(rel, is_dir);
        if m.is_ignore() {
            let pattern = m.inner().map(|g| g.original().to_string());
            Verdict {
                excluded: true,
                level: Some(self.level.clone()),
                line_number: None,
                pattern,
                via_parent: None,
            }
        } else {
            Verdict::included()
        }
    }
}

/// Convenience: path string with `./` or leading slashes stripped.
pub fn normalize_rel_path(raw: &str) -> (PathBuf, bool) {
    let mut s = raw.trim().replace('\\', "/");
    if let Some(stripped) = s.strip_prefix("./") {
        s = stripped.to_string();
    }
    while s.starts_with('/') {
        s = s[1..].to_string();
    }
    let is_dir = s.ends_with('/');
    while s.ends_with('/') {
        s.pop();
    }
    (PathBuf::from(s), is_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> FilterSet {
        FilterSet::default_presets()
    }

    #[test]
    fn excludes_lancedb_extension() {
        let (p, _) = normalize_rel_path("projects/app/vectors.lancedb/data/0.lance");
        let v = filter().verdict(&p, false);
        assert!(v.excluded, "expected *.lancedb to match: {:?}", v);
    }

    #[test]
    fn excludes_node_modules_dir() {
        let (p, is_dir) = normalize_rel_path("web/node_modules/");
        let v = filter().verdict(&p, is_dir);
        assert!(v.excluded);
    }

    #[test]
    fn excludes_via_parent_without_descending() {
        let (p, _) = normalize_rel_path("web/node_modules/react/index.js");
        let v = filter().verdict(&p, false);
        assert!(v.excluded);
        assert!(v.via_parent.is_some());
    }

    #[test]
    fn includes_normal_source_file() {
        let (p, _) = normalize_rel_path("projects/app/src/main.rs");
        let v = filter().verdict(&p, false);
        assert!(!v.excluded);
    }

    #[test]
    fn excludes_ds_store() {
        let (p, _) = normalize_rel_path("photos/.DS_Store");
        let v = filter().verdict(&p, false);
        assert!(v.excluded);
    }

    #[test]
    fn excludes_target_directory() {
        let (p, is_dir) = normalize_rel_path("crate/target/");
        let v = filter().verdict(&p, is_dir);
        assert!(v.excluded);
    }

    #[test]
    fn custom_rule_reinclude_not_in_m0_presets() {
        let rules: Vec<&str> = default_preset_rules()
            .iter()
            .copied()
            .chain(["*.log", "!deploy.log"].iter().copied())
            .collect();
        let f = FilterSet::from_lines(&rules, RuleLevel::Profile);
        let (log, _) = normalize_rel_path("logs/server.log");
        let (deploy, _) = normalize_rel_path("logs/deploy.log");
        assert!(f.verdict(&log, false).excluded);
        assert!(!f.verdict(&deploy, false).excluded);
    }
}
