//! Readable tile colors from a file extension, or from the file name when
//! there is no extension. Hue comes from the first byte of an MD5 hash.
//! Saturation is 30% below the guide's 75%, and lightness stays at 50%.

use md5::{Digest, Md5};

/// 30% less saturated than the 75% in the color guide.
const SATURATION: f32 = 75.0 * 0.7;

/// One row in the file-type list.
///
/// Files that share an extension share a row and a color. A name with no
/// extension is its own row, hashed on the name, which is how the map colors
/// those files.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileTypeId {
    extension: bool,
    key: String,
}

impl FileTypeId {
    /// `.png` for an extension, or the file name when there is no extension.
    pub fn label(&self) -> String {
        if self.extension {
            format!(".{}", self.key)
        } else {
            self.key.clone()
        }
    }

    /// Same RGB the map uses for a file of this type.
    pub fn color_rgb(&self) -> [u8; 3] {
        readable_color_rgb(&self.key)
    }
}

/// Type row for a file name. `photo.PNG` and `other.png` are both `.png`.
pub fn file_type_id(name: &str) -> FileTypeId {
    FileTypeId {
        extension: name_has_extension(name),
        key: file_color_key(name),
    }
}

/// RGB for a file on the map.
///
/// `photo.PNG` and `other.png` share the `png` color. `Makefile` and
/// `.gitignore` have no extension, so each name is hashed on its own.
pub fn file_color_rgb(name: &str) -> [u8; 3] {
    file_type_id(name).color_rgb()
}

/// RGB for an arbitrary label, such as a legend category or a directory name.
pub fn readable_color_rgb(key: &str) -> [u8; 3] {
    let key = key.trim().to_ascii_lowercase();
    let key = if key.is_empty() {
        "unknown"
    } else {
        key.as_str()
    };
    let digest = Md5::digest(key.as_bytes());
    let hue = (digest[0] as f32 / 255.0) * 360.0;
    hsl_to_rgb(hue, SATURATION, 50.0)
}

/// Extension after the last dot, or the whole name when there is no extension.
///
/// A leading dot alone (`.gitignore`) is part of the name, not an extension.
fn name_has_extension(name: &str) -> bool {
    let name = name.trim();
    match name.rfind('.') {
        Some(idx) if idx > 0 => !name[idx + 1..].is_empty(),
        _ => false,
    }
}

fn file_color_key(name: &str) -> String {
    let name = name.trim().to_ascii_lowercase();
    match name.rfind('.') {
        Some(idx) if idx > 0 => {
            let ext = &name[idx + 1..];
            if ext.is_empty() {
                name
            } else {
                ext.to_string()
            }
        }
        _ => name,
    }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let s = s / 100.0;
    let l = l / 100.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = if (0.0..60.0).contains(&h) {
        (c, x, 0.0)
    } else if (60.0..120.0).contains(&h) {
        (x, c, 0.0)
    } else if (120.0..180.0).contains(&h) {
        (0.0, c, x)
    } else if (180.0..240.0).contains(&h) {
        (0.0, x, c)
    } else if (240.0..300.0).contains(&h) {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    [channel(r + m), channel(g + m), channel(b + m)]
}

fn channel(v: f32) -> u8 {
    (v * 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(rgb: [u8; 3]) -> String {
        format!("{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
    }

    #[test]
    fn extension_colors_match_the_guide() {
        assert_eq!(hex(file_color_rgb("photo.png")), "7e3cc2");
        assert_eq!(hex(file_color_rgb("photo.PNG")), "7e3cc2");
        assert_eq!(hex(file_color_rgb("notes.pnh")), "c24c3c");
        assert_eq!(hex(file_color_rgb("data.cache")), "c26b3c");
        assert_eq!(hex(file_color_rgb("store.lancedb")), "3c7bc2");
    }

    #[test]
    fn extensionless_name_is_hashed_on_its_own() {
        assert_ne!(file_color_rgb("Makefile"), file_color_rgb("README"));
        assert_eq!(file_color_rgb("Makefile"), file_color_rgb("makefile"));
        assert_ne!(file_color_rgb(".gitignore"), file_color_rgb("gitignore"));
        assert_ne!(file_color_rgb(".png"), file_color_rgb("photo.png"));
    }

    #[test]
    fn type_label_is_the_extension_and_uses_the_map_color() {
        let png = file_type_id("photo.PNG");
        assert_eq!(png.label(), ".png");
        assert_eq!(png, file_type_id("other.png"));
        assert_eq!(png.color_rgb(), file_color_rgb("photo.png"));

        let make = file_type_id("Makefile");
        assert_eq!(make.label(), "makefile");
        assert_eq!(make.color_rgb(), file_color_rgb("Makefile"));

        let ignore = file_type_id(".gitignore");
        assert_eq!(ignore.label(), ".gitignore");
        assert_eq!(ignore.color_rgb(), file_color_rgb(".gitignore"));
    }
}
