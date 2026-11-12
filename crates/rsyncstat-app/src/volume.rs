//! Disk capacity for treemap tiles.
//!
//! Free space and the gap between two drives are display-only. They are not
//! files, so they never enter the compare index or the sync plan.

use std::path::{Path, PathBuf};

/// Capacity and remaining space of one mounted volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeSpace {
    pub capacity: u64,
    pub available: u64,
}

/// Extra root tile drawn beside the scanned tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceTile {
    /// Bytes still free on this volume.
    Free(u64),
    /// This volume's capacity is smaller than the other drive by this many bytes.
    Gap(u64),
}

impl SpaceTile {
    pub fn bytes(self) -> u64 {
        match self {
            SpaceTile::Free(bytes) | SpaceTile::Gap(bytes) => bytes,
        }
    }
}

/// Tiles to add when `this` is a whole volume.
///
/// `comparing` is set only while two panes are visible. The gap is drawn on
/// the smaller drive so both maps share the larger drive's scale.
pub fn space_tiles(
    this: Option<VolumeSpace>,
    other: Option<VolumeSpace>,
    comparing: bool,
) -> Vec<SpaceTile> {
    let Some(this) = this else {
        return Vec::new();
    };
    let mut tiles = Vec::new();
    if this.available > 0 {
        tiles.push(SpaceTile::Free(this.available));
    }
    if comparing {
        if let Some(other) = other {
            let gap = other.capacity.saturating_sub(this.capacity);
            if gap > 0 {
                tiles.push(SpaceTile::Gap(gap));
            }
        }
    }
    tiles
}

/// Space for `path` when that path is the root of a mounted volume.
pub fn volume_root(path: &Path) -> Option<VolumeSpace> {
    let canon = std::fs::canonicalize(path).ok()?;
    if !is_mount_point(&canon) {
        return None;
    }
    volume_space(&canon)
}

#[cfg(unix)]
fn volume_space(path: &Path) -> Option<VolumeSpace> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a valid NUL-terminated path and `buf` is a writable statvfs.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut buf) };
    if rc != 0 {
        return None;
    }
    let unit = if buf.f_frsize == 0 {
        buf.f_bsize
    } else {
        buf.f_frsize
    };
    if unit == 0 || buf.f_blocks == 0 {
        return None;
    }
    let capacity = (buf.f_blocks as u64).saturating_mul(unit as u64);
    let available = (buf.f_bavail as u64)
        .saturating_mul(unit as u64)
        .min(capacity);
    Some(VolumeSpace {
        capacity,
        available,
    })
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn is_mount_point(path: &Path) -> bool {
    mount_points().iter().any(|mount| paths_equal(mount, path))
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn mount_points() -> Vec<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;

    let mut buf: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: getmntinfo writes a system-owned array of `count` statfs records.
    // The pointer is only read in this call and the paths are copied out.
    let count = unsafe { libc::getmntinfo(&mut buf, libc::MNT_NOWAIT) };
    if count <= 0 || buf.is_null() {
        return Vec::new();
    }
    let mut mounts = Vec::with_capacity(count as usize);
    for index in 0..count as isize {
        let entry = unsafe { &*buf.offset(index) };
        let name = unsafe { CStr::from_ptr(entry.f_mntonname.as_ptr()) };
        let bytes = name.to_bytes();
        if bytes.is_empty() {
            continue;
        }
        mounts.push(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)));
    }
    mounts
}

#[cfg(target_os = "linux")]
fn is_mount_point(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return device_differs_from_parent(path);
    };
    text.lines()
        .filter_map(mount_point_from_mountinfo)
        .any(|mount| paths_equal(&mount, path))
}

#[cfg(all(
    unix,
    not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "linux"
    ))
))]
fn is_mount_point(path: &Path) -> bool {
    path == Path::new("/") || device_differs_from_parent(path)
}

#[cfg(any(
    target_os = "linux",
    all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))
    )
))]
fn device_differs_from_parent(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    if path == Path::new("/") {
        return true;
    }
    let Some(parent) = path.parent() else {
        return true;
    };
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(parent_meta) = std::fs::metadata(parent) else {
        return false;
    };
    meta.dev() != parent_meta.dev()
}

#[cfg(unix)]
fn paths_equal(a: &Path, b: &Path) -> bool {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    fn strip(path: &Path) -> std::ffi::OsString {
        let bytes = path.as_os_str().as_bytes();
        if bytes.len() > 1 && bytes.last() == Some(&b'/') {
            OsStr::from_bytes(&bytes[..bytes.len() - 1]).to_os_string()
        } else {
            path.as_os_str().to_os_string()
        }
    }

    strip(a) == strip(b)
}

/// Mount point (field 5) from one `/proc/self/mountinfo` line.
#[cfg(any(test, target_os = "linux"))]
fn mount_point_from_mountinfo(line: &str) -> Option<PathBuf> {
    let field = line.split(' ').nth(4)?;
    if field.is_empty() {
        return None;
    }
    Some(PathBuf::from(unescape_mount(field)))
}

#[cfg(any(test, target_os = "linux"))]
fn unescape_mount(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = String::with_capacity(field.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let octal = &field[index + 1..index + 4];
            if let Ok(value) = u8::from_str_radix(octal, 8) {
                out.push(value as char);
                index += 4;
                continue;
            }
        }
        out.push(bytes[index] as char);
        index += 1;
    }
    out
}

#[cfg(windows)]
fn volume_space(path: &Path) -> Option<VolumeSpace> {
    let wide = wide_null(path);
    let mut available = 0u64;
    let mut capacity = 0u64;
    let mut total_free = 0u64;
    // SAFETY: `wide` is NUL-terminated and the three outputs are writable u64s.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            &mut capacity,
            &mut total_free,
        )
    };
    if ok == 0 || capacity == 0 {
        return None;
    }
    Some(VolumeSpace {
        capacity,
        available: available.min(capacity),
    })
}

#[cfg(windows)]
fn is_mount_point(path: &Path) -> bool {
    let wide = wide_null(path);
    let mut volume = [0u16; 512];
    // SAFETY: `wide` is NUL-terminated and `volume` is a writable buffer of that length.
    let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) };
    if ok == 0 {
        return false;
    }
    same_volume_root(path, &wide_to_path(&volume))
}

#[cfg(windows)]
fn wide_null(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn wide_to_path(buf: &[u16]) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    let end = buf.iter().position(|&unit| unit == 0).unwrap_or(buf.len());
    std::ffi::OsString::from_wide(&buf[..end]).into()
}

#[cfg(windows)]
fn same_volume_root(path: &Path, volume: &Path) -> bool {
    fn key(path: &Path) -> String {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let mut text = canon.to_string_lossy().replace('/', "\\");
        if let Some(rest) = text.strip_prefix("\\\\?\\UNC\\") {
            text = format!("\\\\{rest}");
        } else if let Some(rest) = text.strip_prefix("\\\\?\\") {
            text = rest.to_string();
        }
        text.trim_end_matches('\\').to_ascii_lowercase()
    }
    key(path) == key(volume)
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetDiskFreeSpaceExW(
        directory: *const u16,
        free_to_caller: *mut u64,
        total: *mut u64,
        total_free: *mut u64,
    ) -> i32;
    fn GetVolumePathNameW(filename: *const u16, volume: *mut u16, len: u32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_unescapes_spaces() {
        let line = "36 35 98:0 / /mnt/My\\040Disk rw - ext4 /dev/sda1 rw";
        let path = mount_point_from_mountinfo(line).unwrap();
        assert_eq!(path, PathBuf::from("/mnt/My Disk"));
    }

    #[test]
    fn folder_scan_has_no_space_tiles() {
        assert!(space_tiles(None, None, true).is_empty());
    }

    #[test]
    fn drive_scan_includes_free_space() {
        let drive = VolumeSpace {
            capacity: 1_000,
            available: 400,
        };
        assert_eq!(
            space_tiles(Some(drive), None, true),
            vec![SpaceTile::Free(400)]
        );
    }

    #[test]
    fn smaller_drive_gains_an_empty_gap_while_comparing() {
        let small = VolumeSpace {
            capacity: 100,
            available: 40,
        };
        let big = VolumeSpace {
            capacity: 250,
            available: 10,
        };
        assert_eq!(
            space_tiles(Some(small), Some(big), false),
            vec![SpaceTile::Free(40)]
        );
        assert_eq!(
            space_tiles(Some(small), Some(big), true),
            vec![SpaceTile::Free(40), SpaceTile::Gap(150)]
        );
        assert_eq!(
            space_tiles(Some(big), Some(small), true),
            vec![SpaceTile::Free(10)]
        );
    }

    #[test]
    fn full_drive_still_shows_the_gap() {
        let full = VolumeSpace {
            capacity: 100,
            available: 0,
        };
        let big = VolumeSpace {
            capacity: 250,
            available: 10,
        };
        assert_eq!(
            space_tiles(Some(full), Some(big), true),
            vec![SpaceTile::Gap(150)]
        );
    }

    #[test]
    fn nested_directory_is_not_a_volume() {
        let dir = std::env::temp_dir().join(format!("rsyncstat-vol-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(volume_root(&dir).is_none());
        let _ = std::fs::remove_dir(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_root_is_a_volume() {
        let volume = volume_root(Path::new("/")).expect("root is a mounted volume");
        assert!(volume.capacity > 0);
        assert!(volume.available <= volume.capacity);
    }
}
