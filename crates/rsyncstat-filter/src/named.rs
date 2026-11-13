//! Named include/exclude packs stored as YAML.

use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

use crate::split_patterns;

/// One named pack of include and exclude pattern strings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedPreset {
    pub name: String,
    #[serde(default, deserialize_with = "deserialize_patterns")]
    pub include: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_patterns")]
    pub exclude: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PresetFile {
    #[serde(default)]
    presets: Vec<NamedPreset>,
}

/// Built-in YAML shipped with the crate (git, node, vite, …).
pub fn builtin_preset_yaml() -> &'static str {
    include_str!("../include-exclude.yaml")
}

pub fn builtin_named_presets() -> Vec<NamedPreset> {
    parse_preset_file(builtin_preset_yaml()).expect("built-in include-exclude.yaml is valid")
}

pub fn parse_preset_file(text: &str) -> Result<Vec<NamedPreset>, String> {
    let file: PresetFile =
        serde_yaml::from_str(text).map_err(|err| format!("include-exclude.yaml: {err}"))?;
    let mut out = Vec::with_capacity(file.presets.len());
    for mut preset in file.presets {
        preset.name = preset.name.trim().to_string();
        if preset.name.is_empty() {
            continue;
        }
        preset.include = normalize_patterns(preset.include);
        preset.exclude = normalize_patterns(preset.exclude);
        out.push(preset);
    }
    Ok(out)
}

pub fn serialize_preset_file(presets: &[NamedPreset]) -> Result<String, String> {
    let body = serde_yaml::to_string(&PresetFile {
        presets: presets.to_vec(),
    })
    .map_err(|err| format!("include-exclude.yaml: {err}"))?;
    Ok(format!("{PRESET_FILE_HEADER}{body}"))
}

const PRESET_FILE_HEADER: &str = "\
# Named include/exclude packs for RSyncStat.
# Click + next to a name in the app to append these strings to the fields.
# Patterns use the same syntax as the include/exclude boxes (gitignore globs,
# or regular expressions if .* is turned on).
#
# This file is created next to the RSyncStat binary. Edit it here, or use
# \"Add to presets…\" in the app to save the current fields under a new name.

";

/// Split a field the same way the include/exclude boxes do.
pub fn field_patterns(raw: &str) -> Vec<String> {
    split_patterns(raw)
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// Append `extra` tokens to a comma-separated field, skipping duplicates.
pub fn merge_field_patterns(existing: &str, extra: &[String]) -> String {
    let mut out = field_patterns(existing);
    for token in extra {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let seen = out.iter().any(|have| have.eq_ignore_ascii_case(token));
        if !seen {
            out.push(token.to_string());
        }
    }
    out.join(", ")
}

impl NamedPreset {
    pub fn apply_to(&self, include: &mut String, exclude: &mut String) {
        *include = merge_field_patterns(include, &self.include);
        *exclude = merge_field_patterns(exclude, &self.exclude);
    }

    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }
}

pub fn validate_preset_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("Type a name for this preset.".to_string());
    }
    if name.chars().count() > 64 {
        return Err("Keep the preset name to 64 characters.".to_string());
    }
    if name.contains(['/', '\\', '\n', '\r', '\0']) {
        return Err("The preset name cannot contain slashes or line breaks.".to_string());
    }
    Ok(name.to_string())
}

/// Insert or replace a pack of the same name (case-insensitive). Returns true if this was new.
pub fn upsert_preset(presets: &mut Vec<NamedPreset>, preset: NamedPreset) -> bool {
    if let Some(existing) = presets
        .iter_mut()
        .find(|have| have.name.eq_ignore_ascii_case(&preset.name))
    {
        *existing = preset;
        false
    } else {
        presets.push(preset);
        true
    }
}

fn normalize_patterns(items: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        for token in split_patterns(&item) {
            if !token.is_empty() {
                out.push(token.to_string());
            }
        }
    }
    out
}

fn deserialize_patterns<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct PatternVisitor;

    impl<'de> serde::de::Visitor<'de> for PatternVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a pattern string or a list of patterns")
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(field_patterns(value))
        }

        fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
            Ok(field_patterns(&value))
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(item) = seq.next_element::<String>()? {
                out.extend(field_patterns(&item));
            }
            Ok(out)
        }

        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
    }

    deserializer.deserialize_any(PatternVisitor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FilterSet, PatternOptions};
    use std::path::Path;

    #[test]
    fn builtin_yaml_has_git_node_and_vite() {
        let names: Vec<_> = builtin_named_presets()
            .into_iter()
            .map(|preset| preset.name)
            .collect();
        for want in ["git", "node", "vite", "rust", "python"] {
            assert!(names.iter().any(|have| have == want), "missing {want}");
        }
    }

    #[test]
    fn git_pack_excludes_dot_git() {
        let git = builtin_named_presets()
            .into_iter()
            .find(|preset| preset.name == "git")
            .expect("git pack");
        let exclude = merge_field_patterns("", &git.exclude);
        let (filter, errors) = FilterSet::from_rules(
            "",
            PatternOptions::default(),
            &exclude,
            PatternOptions::default(),
            false,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert!(filter.verdict(Path::new(".git"), true).excluded);
        assert!(filter.verdict(Path::new(".git/HEAD"), false).excluded);
        assert!(!filter.verdict(Path::new("src/main.rs"), false).excluded);
    }

    #[test]
    fn merge_skips_duplicates_and_keeps_order() {
        let merged = merge_field_patterns("*.rs, src/**", &["src/**".into(), "tests/**".into()]);
        assert_eq!(merged, "*.rs, src/**, tests/**");
    }

    #[test]
    fn apply_to_fills_include_and_exclude() {
        let preset = NamedPreset {
            name: "source".into(),
            include: vec!["*.rs".into()],
            exclude: vec!["target/".into()],
        };
        let mut include = String::from("*.md");
        let mut exclude = String::new();
        preset.apply_to(&mut include, &mut exclude);
        assert_eq!(include, "*.md, *.rs");
        assert_eq!(exclude, "target/");
    }

    #[test]
    fn yaml_roundtrip_keeps_user_pack() {
        let original = builtin_named_presets();
        let text = serialize_preset_file(&original).unwrap();
        let parsed = parse_preset_file(&text).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn comma_string_patterns_are_accepted() {
        let parsed = parse_preset_file(
            "presets:\n  - name: logs\n    include: \"*.log, *.txt\"\n    exclude: []\n",
        )
        .unwrap();
        assert_eq!(parsed[0].include, vec!["*.log", "*.txt"]);
    }

    #[test]
    fn upsert_replaces_the_same_name() {
        let mut presets = builtin_named_presets();
        let added = upsert_preset(
            &mut presets,
            NamedPreset {
                name: "Git".into(),
                include: vec!["*.md".into()],
                exclude: vec![],
            },
        );
        assert!(!added);
        let git = presets.iter().find(|preset| preset.name == "Git").unwrap();
        assert_eq!(git.include, vec!["*.md"]);
        assert_eq!(
            presets
                .iter()
                .filter(|preset| preset.name.eq_ignore_ascii_case("git"))
                .count(),
            1
        );
    }

    #[test]
    fn validate_preset_name_rejects_empty_and_slashes() {
        assert!(validate_preset_name("  ").is_err());
        assert!(validate_preset_name("a/b").is_err());
        assert_eq!(validate_preset_name("  photos ").unwrap(), "photos");
    }

    #[test]
    fn empty_names_are_dropped() {
        let parsed =
            parse_preset_file("presets:\n  - name: \"  \"\n    exclude: [\".git/\"]\n").unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn bad_yaml_is_an_error() {
        assert!(parse_preset_file("presets: [").is_err());
    }
}
