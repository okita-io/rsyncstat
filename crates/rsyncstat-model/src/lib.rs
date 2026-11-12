//! Shared scan tree model (arena-oriented layout for later milestones).

use serde::{Deserialize, Serialize};

/// Stable node identifier for future arena storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

/// A node in a directory tree built from a scan.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TreeNode {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub excluded: bool,
    /// Human-readable hint for which rule excluded this path (M0: pattern text).
    pub exclude_rule: Option<String>,
    pub children: Vec<TreeNode>,
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
        if self.excluded {
            return 0;
        }
        if self.is_dir && !self.children.is_empty() {
            self.children.iter().map(TreeNode::total_size).sum()
        } else {
            self.size
        }
    }
}

/// Extension → bucket name for the type legend (WinDirStat-style, simplified for M0).
pub fn file_type_bucket(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or(name);
    match ext.to_ascii_lowercase().as_str() {
        "" if name.contains('.') => "no ext",
        "" => "folders",
        "rs" | "toml" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "cpp" | "h" | "go" | "java" => {
            "code"
        }
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "heic" => "images",
        "mp4" | "mkv" | "mov" | "avi" | "webm" => "video",
        "mp3" | "flac" | "wav" | "ogg" => "audio",
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" => "archives",
        "pdf" | "doc" | "docx" | "md" | "txt" => "documents",
        "db" | "sqlite" | "lancedb" => "databases",
        _ => "other",
    }
}
