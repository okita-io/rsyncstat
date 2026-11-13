//! Reveal a scanned path in the file manager, and move one to the Trash.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Absolute path of a treemap tile. The first segment is the root's display name.
pub fn disk_path(root: &Path, tile_path: &[String]) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in tile_path.iter().skip(1) {
        path.push(part);
    }
    path
}

/// Open the parent folder with this path selected.
pub fn reveal(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg("-R").arg(path).spawn()?.wait()?;
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()?
            .wait()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let uri = file_uri(path);
        let shown = Command::new("dbus-send")
            .args([
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.FileManager1",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
                &format!("array:string:\"{uri}\""),
                "string:",
            ])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if shown {
            return Ok(());
        }
        let folder = path.parent().unwrap_or(path);
        Command::new("xdg-open").arg(folder).spawn()?.wait()?;
        Ok(())
    }
}

pub fn move_to_trash(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|err| err.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(byte as char);
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_path_drops_the_display_root_and_keeps_the_rest() {
        let path = disk_path(
            Path::new("/Volumes/Photos"),
            &["Photos".into(), "2024".into(), "img.jpg".into()],
        );
        assert_eq!(path, PathBuf::from("/Volumes/Photos/2024/img.jpg"));
    }
}
