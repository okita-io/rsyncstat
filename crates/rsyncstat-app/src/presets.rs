//! Load and save `include-exclude.yaml` next to the RSyncStat binary.

use rsyncstat_filter::{
    builtin_named_presets, builtin_preset_yaml, parse_preset_file, serialize_preset_file,
    NamedPreset,
};
use std::fs;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "include-exclude.yaml";

/// `include-exclude.yaml` beside the running binary.
pub fn default_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            let mut dir = exe;
            if dir.pop() {
                Some(dir.join(FILE_NAME))
            } else {
                None
            }
        })
        .unwrap_or_else(|| std::env::temp_dir().join(FILE_NAME))
}

/// Read the file, or write the built-in packs if it is missing.
pub fn load_or_create(path: &Path) -> (Vec<NamedPreset>, Option<String>) {
    match fs::read_to_string(path) {
        Ok(text) => match parse_preset_file(&text) {
            Ok(presets) if !presets.is_empty() => (presets, None),
            Ok(_) => (
                builtin_named_presets(),
                Some(format!(
                    "{} is empty, so the built-in packs are used.",
                    FILE_NAME
                )),
            ),
            Err(message) => (builtin_named_presets(), Some(message)),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Err(err) = write_builtin(path) {
                (
                    builtin_named_presets(),
                    Some(format!("Could not create {FILE_NAME}: {err}")),
                )
            } else {
                (builtin_named_presets(), None)
            }
        }
        Err(err) => (
            builtin_named_presets(),
            Some(format!("Could not read {FILE_NAME}: {err}")),
        ),
    }
}

pub fn save(path: &Path, presets: &[NamedPreset]) -> Result<(), String> {
    let text = serialize_preset_file(presets)?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("Could not write {FILE_NAME}: {err}"))?;
        }
    }
    fs::write(path, text).map_err(|err| format!("Could not write {FILE_NAME}: {err}"))
}

fn write_builtin(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(path, builtin_preset_yaml())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsyncstat_filter::{field_patterns, upsert_preset, NamedPreset};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Sandbox(PathBuf);

    impl Sandbox {
        fn new(label: &str) -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "rsyncstat-presets-{label}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn yaml(&self) -> PathBuf {
            self.0.join(FILE_NAME)
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn default_path_uses_the_file_name() {
        assert_eq!(
            default_path().file_name().and_then(|name| name.to_str()),
            Some(FILE_NAME)
        );
    }

    #[test]
    fn missing_file_is_created_with_git_and_node() {
        let dir = Sandbox::new("create");
        let path = dir.yaml();
        let (presets, warning) = load_or_create(&path);
        assert!(warning.is_none(), "{warning:?}");
        assert!(path.is_file());
        assert!(presets.iter().any(|preset| preset.name == "git"));
        assert!(presets.iter().any(|preset| preset.name == "node"));
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("name: vite"));
    }

    #[test]
    fn save_appends_a_user_pack() {
        let dir = Sandbox::new("save");
        let path = dir.yaml();
        let (mut presets, _) = load_or_create(&path);
        upsert_preset(
            &mut presets,
            NamedPreset {
                name: "photos".into(),
                include: field_patterns("*.jpg, *.png"),
                exclude: field_patterns(".DS_Store"),
            },
        );
        save(&path, &presets).unwrap();
        let (loaded, warning) = load_or_create(&path);
        assert!(warning.is_none(), "{warning:?}");
        let photos = loaded
            .iter()
            .find(|preset| preset.name == "photos")
            .expect("photos pack");
        assert_eq!(photos.include, vec!["*.jpg", "*.png"]);
        assert_eq!(photos.exclude, vec![".DS_Store"]);
    }

    #[test]
    fn corrupt_file_falls_back_to_builtins() {
        let dir = Sandbox::new("bad");
        let path = dir.yaml();
        fs::write(&path, "presets: [").unwrap();
        let (presets, warning) = load_or_create(&path);
        assert!(warning.is_some());
        assert!(presets.iter().any(|preset| preset.name == "git"));
    }
}
