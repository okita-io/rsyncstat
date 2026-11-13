//! Built-in exclude preset lines (see `docs/SPEC.md`).

pub fn default_preset_rules() -> Vec<&'static str> {
    vec![
        // system junk
        ".DS_Store",
        "._*",
        ".Spotlight-V100/",
        ".Trashes/",
        ".fseventsd/",
        ".TemporaryItems/",
        "Thumbs.db",
        "desktop.ini",
        "$RECYCLE.BIN/",
        "System Volume Information/",
        // temp and live database side files
        "*.tmp",
        "*.swp",
        "~$*",
        "*.sqlite-wal",
        "*.sqlite-shm",
        "*.sqlite-journal",
        "*.db-wal",
        "*.db-shm",
        "*.db-journal",
        // rebuildable data and build output
        "*.lancedb",
        "node_modules/",
        "target/",
        "__pycache__/",
        ".venv/",
        // RSyncStat's own state (always on)
        ".rsyncstat/",
    ]
}
