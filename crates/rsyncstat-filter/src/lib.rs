//! Gitignore-style exclude rules compiled with the `ignore` crate.

mod named;
mod presets;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How one include or exclude field interprets its text.
///
/// All three off is a case-insensitive glob, which is the original behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PatternOptions {
    /// `File.TXT` and `file.txt` are different.
    pub match_case: bool,
    /// Literal text must fall on word boundaries. `log` matches `notes.log`, not `catalog`.
    pub whole_word: bool,
    /// The text is a regular expression instead of a glob.
    pub regex: bool,
}

/// Which of the three pattern toggles was clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatternToggle {
    Case,
    Word,
    Regex,
}

impl PatternOptions {
    fn is_plain_glob(self) -> bool {
        !self.match_case && !self.whole_word && !self.regex
    }

    pub fn flip(&mut self, toggle: PatternToggle) {
        match toggle {
            PatternToggle::Case => self.match_case = !self.match_case,
            PatternToggle::Word => self.whole_word = !self.whole_word,
            PatternToggle::Regex => self.regex = !self.regex,
        }
    }
}

pub use named::{
    builtin_named_presets, builtin_preset_yaml, field_patterns, merge_field_patterns,
    parse_preset_file, serialize_preset_file, upsert_preset, validate_preset_name, NamedPreset,
};
pub use presets::default_preset_rules;

/// Where a matching rule came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleLevel {
    BuiltinPreset,
    Profile,
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

/// Compiled filter rules (built-in presets plus optional extra lines).
#[derive(Debug, Clone)]
pub struct FilterSet {
    gitignore: Arc<Gitignore>,
    level: RuleLevel,
    custom: Option<CustomMatch>,
}

impl FilterSet {
    /// Build from the default built-in preset rule list.
    pub fn default_presets() -> Self {
        Self::from_lines(&default_preset_rules(), RuleLevel::BuiltinPreset)
    }

    pub fn from_lines(lines: &[&str], level: RuleLevel) -> Self {
        let mut builder = GitignoreBuilder::new(".");
        builder
            .case_insensitive(true)
            .expect("case flag is accepted");
        for line in lines {
            builder.add_line(None, line).expect("invalid preset line");
        }
        let gitignore = Arc::new(builder.build().expect("failed to build gitignore"));
        Self {
            gitignore,
            level,
            custom: None,
        }
    }

    /// Like [`Self::from_lines`], but a pattern the ignore crate rejects is skipped
    /// instead of aborting. Used for text typed into the include and exclude fields.
    pub fn from_user_lines(lines: &[&str], level: RuleLevel) -> Self {
        let mut builder = GitignoreBuilder::new(".");
        builder
            .case_insensitive(true)
            .expect("case flag is accepted");
        for line in lines {
            let _ = builder.add_line(None, line);
        }
        let gitignore = Arc::new(builder.build().unwrap_or_else(|_| Gitignore::empty()));
        Self {
            gitignore,
            level,
            custom: None,
        }
    }

    /// Include and exclude text, with the match-case, whole-word, and regex toggles.
    ///
    /// Plain globs stay on the gitignore path. Any toggle that changes that
    /// reading compiles the field into a regular expression. Invalid regular
    /// expressions are skipped and returned as messages.
    pub fn from_rules(
        include: &str,
        include_opts: PatternOptions,
        exclude: &str,
        exclude_opts: PatternOptions,
        use_presets: bool,
    ) -> (Self, Vec<String>) {
        if include_opts.is_plain_glob() && exclude_opts.is_plain_glob() {
            let lines = gitignore_lines(include, exclude, use_presets);
            let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
            return (
                Self::from_user_lines(&borrowed, RuleLevel::Profile),
                Vec::new(),
            );
        }
        let mut errors = Vec::new();
        let includes = compile_list(include, include_opts, "include", &mut errors);
        let presets = if use_presets && includes.is_empty() {
            Some(Arc::clone(&Self::default_presets().gitignore))
        } else {
            None
        };
        let excludes = compile_list(exclude, exclude_opts, "exclude", &mut errors);
        (
            Self {
                gitignore: Arc::new(Gitignore::empty()),
                level: RuleLevel::Profile,
                custom: Some(CustomMatch {
                    presets,
                    includes,
                    excludes,
                }),
            },
            errors,
        )
    }

    /// Evaluate `rel` (relative to scan root, platform path separators allowed).
    pub fn verdict(&self, rel: &Path, is_dir: bool) -> Verdict {
        if self.custom.is_some() {
            return self.custom_verdict(rel, is_dir);
        }
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

    fn custom_verdict(&self, rel: &Path, is_dir: bool) -> Verdict {
        let custom = self.custom.as_ref().expect("custom filter");
        if let Some(parent) = self.custom_ancestor(custom, rel) {
            return Verdict {
                excluded: true,
                level: Some(self.level.clone()),
                line_number: None,
                pattern: None,
                via_parent: Some(parent),
            };
        }
        let (excluded, pattern) = custom.decide(rel, is_dir);
        if excluded {
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

    fn custom_ancestor(&self, custom: &CustomMatch, rel: &Path) -> Option<PathBuf> {
        let mut path = rel.to_path_buf();
        while path.pop() {
            if path.as_os_str().is_empty() {
                break;
            }
            let (excluded, _) = custom.decide(&path, true);
            if excluded {
                return Some(path);
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
struct CustomMatch {
    presets: Option<Arc<Gitignore>>,
    includes: Vec<CompiledPattern>,
    excludes: Vec<CompiledPattern>,
}

impl CustomMatch {
    fn decide(&self, rel: &Path, is_dir: bool) -> (bool, Option<String>) {
        let path = rel.to_string_lossy().replace('\\', "/");
        let mut excluded = false;
        let mut pattern = None;
        if self.includes.is_empty() {
            if let Some(presets) = &self.presets {
                let matched = presets.matched(rel, is_dir);
                if matched.is_ignore() {
                    excluded = true;
                    pattern = matched.inner().map(|rule| rule.original().to_string());
                }
            }
        } else if !is_dir {
            excluded = true;
            pattern = Some("*".to_string());
            if self
                .includes
                .iter()
                .any(|rule| rule.matches_included(&path))
            {
                excluded = false;
                pattern = None;
            }
        }
        if let Some(rule) = self
            .excludes
            .iter()
            .find(|rule| rule.matches_path(&path, is_dir))
        {
            excluded = true;
            pattern = Some(rule.source.clone());
        }
        (excluded, pattern)
    }
}

#[derive(Debug, Clone)]
struct CompiledPattern {
    source: String,
    regex: Regex,
    dir_only: bool,
}

impl CompiledPattern {
    fn matches_path(&self, path: &str, is_dir: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        self.regex.is_match(path)
    }

    /// A file matches an include when the pattern hits the file or a parent directory.
    fn matches_included(&self, path: &str) -> bool {
        if self.matches_path(path, false) {
            return true;
        }
        let mut acc = String::new();
        for part in path.split('/') {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(part);
            if acc == path {
                break;
            }
            if self.matches_path(&acc, true) {
                return true;
            }
        }
        false
    }
}

fn compile_list(
    raw: &str,
    opts: PatternOptions,
    label: &str,
    errors: &mut Vec<String>,
) -> Vec<CompiledPattern> {
    split_patterns(raw)
        .into_iter()
        .filter_map(|token| match compile_pattern(token, opts) {
            Ok(Some(pattern)) => Some(pattern),
            Ok(None) => None,
            Err(message) => {
                errors.push(format!("{label}: {token}: {message}"));
                None
            }
        })
        .collect()
}

fn compile_pattern(token: &str, opts: PatternOptions) -> Result<Option<CompiledPattern>, String> {
    let token = token.trim().trim_start_matches('!');
    if token.is_empty() || token.contains(['\n', '\r', '\0']) {
        return Ok(None);
    }
    if !opts.regex && !safe_pattern(token) {
        return Ok(None);
    }
    let (body, dir_only) = match token.strip_suffix('/') {
        Some(rest) if !rest.is_empty() => (rest, true),
        _ => (token, false),
    };
    let source = pattern_source(body, opts)?;
    let regex = Regex::new(&source).map_err(|err| err.to_string())?;
    Ok(Some(CompiledPattern {
        source: token.to_string(),
        regex,
        dir_only,
    }))
}

fn pattern_source(token: &str, opts: PatternOptions) -> Result<String, String> {
    let case = if opts.match_case { "" } else { "(?i)" };
    if opts.regex {
        let body = if opts.whole_word {
            wrap_word(token)
        } else {
            token.to_string()
        };
        return Ok(format!("{case}{body}"));
    }
    let rooted = token.starts_with('/') || token.contains('/');
    let token = token.trim_start_matches('/');
    if opts.whole_word && is_literal_glob(token) {
        let literal = word_bounded_literal(token);
        return Ok(format!(r"{case}(?:^|/)[^/]*{literal}[^/]*$"));
    }
    let body = glob_body(token, opts.whole_word);
    if rooted {
        Ok(format!("{case}^{body}$"))
    } else {
        Ok(format!(r"{case}(?:^|/){body}$"))
    }
}

fn is_literal_glob(token: &str) -> bool {
    !token.contains(['*', '?', '/', '\\', '['])
}

fn word_bounded_literal(token: &str) -> String {
    let mut out = String::new();
    if starts_with_word(token) {
        out.push_str(r"\b");
    }
    out.push_str(&regex::escape(token));
    if ends_with_word(token) {
        out.push_str(r"\b");
    }
    out
}

fn wrap_word(pattern: &str) -> String {
    let mut out = String::new();
    if starts_with_word(pattern) {
        out.push_str(r"\b");
    }
    out.push_str("(?:");
    out.push_str(pattern);
    out.push(')');
    if ends_with_word(pattern) {
        out.push_str(r"\b");
    }
    out
}

fn starts_with_word(text: &str) -> bool {
    text.chars().next().is_some_and(is_word_char)
}

fn ends_with_word(text: &str) -> bool {
    text.chars().next_back().is_some_and(is_word_char)
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn glob_body(pattern: &str, whole_word: bool) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            push_literal(&mut out, &chars[i + 1].to_string(), whole_word);
            i += 2;
            continue;
        }
        if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            if i + 2 < chars.len() && chars[i + 2] == '/' {
                out.push_str("(?:.*/)?");
                i += 3;
            } else {
                out.push_str(".*");
                i += 2;
            }
            continue;
        }
        if chars[i] == '*' {
            out.push_str("[^/]*");
            i += 1;
            continue;
        }
        if chars[i] == '?' {
            out.push_str("[^/]");
            i += 1;
            continue;
        }
        if chars[i] == '[' {
            if let Some(end) = chars[i + 1..].iter().position(|ch| *ch == ']') {
                let class: String = chars[i..=i + 1 + end].iter().collect();
                if let Some(rest) = class.strip_prefix("[!") {
                    out.push_str("[^");
                    out.push_str(rest);
                } else {
                    out.push_str(&class);
                }
                i += end + 2;
                continue;
            }
        }
        let start = i;
        while i < chars.len() && !matches!(chars[i], '*' | '?' | '[' | '\\') {
            i += 1;
        }
        let literal: String = chars[start..i].iter().collect();
        push_literal(&mut out, &literal, whole_word);
    }
    out
}

fn push_literal(out: &mut String, literal: &str, whole_word: bool) {
    if literal.is_empty() {
        return;
    }
    if !whole_word {
        out.push_str(&regex::escape(literal));
        return;
    }
    if starts_with_word(literal) {
        out.push_str(r"\b");
    }
    out.push_str(&regex::escape(literal));
    if ends_with_word(literal) {
        out.push_str(r"\b");
    }
}

/// Gitignore lines for the scan and the sync plan.
///
/// Patterns are comma-separated. `*.log` and `node_modules` exclude those names.
/// Include patterns are a whitelist: an empty include list keeps every file
/// that is not excluded. Excludes are applied after includes, so they win.
pub fn gitignore_lines(include: &str, exclude: &str, use_presets: bool) -> Vec<String> {
    let mut lines = Vec::new();
    if use_presets {
        lines.extend(default_preset_rules().into_iter().map(str::to_string));
    }
    let includes = split_patterns(include);
    if !includes.is_empty() {
        // Drop everything, keep directories walkable, then put the requested
        // names back. User excludes are appended after this so they still win.
        lines.push("*".to_string());
        lines.push("!*/".to_string());
        for token in includes {
            lines.extend(include_lines(token));
        }
    }
    for token in split_patterns(exclude) {
        if let Some(line) = exclude_line(token) {
            lines.push(line);
        }
    }
    lines
}

pub(crate) fn split_patterns(raw: &str) -> Vec<&str> {
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

    fn excludes(
        include: &str,
        include_opts: PatternOptions,
        exclude: &str,
        exclude_opts: PatternOptions,
        rel: &str,
    ) -> bool {
        let (filter, errors) =
            FilterSet::from_rules(include, include_opts, exclude, exclude_opts, false);
        assert!(errors.is_empty(), "{errors:?}");
        filter.verdict(Path::new(rel), false).excluded
    }

    #[test]
    fn toggled_globs_follow_gitignore_when_case_is_the_only_change() {
        let case = PatternOptions {
            match_case: true,
            ..PatternOptions::default()
        };
        // Empty include with match-case on forces the regex compiler, so the
        // exclude glob below is case-insensitive and should agree with gitignore.
        let (filter, errors) = FilterSet::from_rules(
            "",
            case,
            "*.log, node_modules, src/**",
            PatternOptions::default(),
            false,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let samples = [
            "notes.log",
            "dir/notes.log",
            "notes.logout",
            "node_modules",
            "web/node_modules",
            "web/other",
            "src/a.rs",
            "src/a/b.rs",
            "lib/a.rs",
            "File.LOG",
        ];
        for rel in samples {
            let want = {
                let mut builder = GitignoreBuilder::new(".");
                builder.case_insensitive(true).unwrap();
                for line in ["*.log", "node_modules", "src/**"] {
                    builder.add_line(None, line).unwrap();
                }
                let gitignore = builder.build().unwrap();
                let path = Path::new(rel);
                let self_match = gitignore.matched(path, false).is_ignore();
                let mut parent = path.to_path_buf();
                let mut via_parent = false;
                while parent.pop() && !parent.as_os_str().is_empty() {
                    if gitignore.matched(&parent, true).is_ignore() {
                        via_parent = true;
                        break;
                    }
                }
                self_match || via_parent
            };
            assert_eq!(
                filter.verdict(Path::new(rel), false).excluded,
                want,
                "{rel}"
            );
        }
    }

    #[test]
    fn match_case_distinguishes_extensions() {
        let case = PatternOptions {
            match_case: true,
            ..PatternOptions::default()
        };
        assert!(excludes(
            "",
            PatternOptions::default(),
            "*.TXT",
            case,
            "notes.TXT"
        ));
        assert!(!excludes(
            "",
            PatternOptions::default(),
            "*.TXT",
            case,
            "notes.txt"
        ));
        assert!(excludes(
            "",
            PatternOptions::default(),
            "*.txt",
            PatternOptions::default(),
            "notes.TXT"
        ));
    }

    #[test]
    fn whole_word_requires_boundaries() {
        let word = PatternOptions {
            whole_word: true,
            ..PatternOptions::default()
        };
        assert!(excludes(
            "",
            PatternOptions::default(),
            "log",
            word,
            "notes.log"
        ));
        assert!(excludes("", PatternOptions::default(), "log", word, "log"));
        assert!(!excludes(
            "",
            PatternOptions::default(),
            "log",
            word,
            "catalog.txt"
        ));
        assert!(!excludes(
            "",
            PatternOptions::default(),
            "log",
            word,
            "login.txt"
        ));
        assert!(!excludes(
            "",
            PatternOptions::default(),
            "*log*",
            word,
            "catalog.txt"
        ));
        assert!(excludes(
            "",
            PatternOptions::default(),
            "*log*",
            word,
            "my-log.txt"
        ));
        assert!(!excludes(
            "",
            PatternOptions::default(),
            "log",
            PatternOptions::default(),
            "notes.log"
        ));
        assert!(excludes(
            "",
            PatternOptions::default(),
            "log",
            PatternOptions::default(),
            "dir/log"
        ));
    }

    #[test]
    fn regex_matches_the_path_and_reports_a_bad_pattern() {
        let regex = PatternOptions {
            regex: true,
            ..PatternOptions::default()
        };
        assert!(excludes(
            "",
            PatternOptions::default(),
            r"\.log$",
            regex,
            "dir/notes.log"
        ));
        assert!(!excludes(
            "",
            PatternOptions::default(),
            r"\.log$",
            regex,
            "dir/notes.logout"
        ));
        let case = PatternOptions {
            regex: true,
            match_case: true,
            ..PatternOptions::default()
        };
        assert!(!excludes(
            "",
            PatternOptions::default(),
            r"\.log$",
            case,
            "notes.LOG"
        ));
        let word = PatternOptions {
            regex: true,
            whole_word: true,
            ..PatternOptions::default()
        };
        assert!(excludes(
            "",
            PatternOptions::default(),
            "log",
            word,
            "notes.log"
        ));
        assert!(!excludes(
            "",
            PatternOptions::default(),
            "log",
            word,
            "catalog"
        ));
        let (filter, errors) =
            FilterSet::from_rules("", PatternOptions::default(), "(", regex, false);
        assert!(!errors.is_empty());
        assert!(!filter.verdict(Path::new("notes.log"), false).excluded);
    }

    #[test]
    fn include_whitelist_uses_the_same_toggles() {
        let case = PatternOptions {
            match_case: true,
            ..PatternOptions::default()
        };
        assert!(!excludes(
            "*.txt",
            case,
            "",
            PatternOptions::default(),
            "a.txt"
        ));
        assert!(excludes(
            "*.txt",
            case,
            "",
            PatternOptions::default(),
            "a.TXT"
        ));
        assert!(excludes(
            "*.txt",
            case,
            "",
            PatternOptions::default(),
            "a.rs"
        ));
        assert!(!excludes(
            "skip",
            PatternOptions::default(),
            "",
            PatternOptions::default(),
            "skip/secret.txt"
        ));
        assert!(excludes(
            "*.txt",
            PatternOptions::default(),
            "*.log",
            PatternOptions::default(),
            "notes.log"
        ));
    }
}
