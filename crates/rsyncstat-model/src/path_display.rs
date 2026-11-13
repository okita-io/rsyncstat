//! Middle-elided path text when horizontal space is limited.

const ELLIPSIS: &str = "…";

/// Display path with middle elision when `measure(display) > max_width`.
pub fn elide_path_for_width(path: &str, max_width: f32, measure: impl Fn(&str) -> f32) -> String {
    if path.is_empty() || max_width <= 0.0 {
        return path.to_string();
    }
    if measure(path) <= max_width {
        return path.to_string();
    }

    let is_win = path.contains('\\') || path.as_bytes().get(1).copied() == Some(b':');
    let sep = if is_win { '\\' } else { '/' };
    let (root_prefix, segments) = parse_segments(path, sep);

    if segments.is_empty() {
        return truncate_chars(path, max_width, &measure);
    }

    let mut best: Option<String> = None;
    let mut best_score = 0usize;

    for tail_len in 1..=segments.len() {
        let max_head = segments.len().saturating_sub(tail_len);
        for head_len in 1..=max_head {
            if head_len + tail_len >= segments.len() {
                continue;
            }
            let head = format_path(&root_prefix, &segments[..head_len], sep);
            let tail = format_path("", &segments[segments.len() - tail_len..], sep);
            let candidate = if head.is_empty() {
                format!("{ELLIPSIS}{sep}{tail}")
            } else {
                format!("{head}{ELLIPSIS}{sep}{tail}")
            };
            if measure(&candidate) <= max_width {
                let score = head.chars().count() + tail.chars().count();
                if score > best_score {
                    best_score = score;
                    best = Some(candidate);
                }
            }
        }
    }

    best.unwrap_or_else(|| truncate_chars(path, max_width, &measure))
}

fn parse_segments(path: &str, sep: char) -> (String, Vec<String>) {
    let norm = if sep == '\\' {
        path.replace('/', "\\")
    } else {
        path.replace('\\', "/")
    };

    let mut root_prefix = String::new();
    let mut rest = norm.as_str();

    if sep == '/' && norm.starts_with('/') {
        root_prefix = "/".to_string();
        rest = &norm[1..];
    } else if sep == '\\' {
        if let Some(colon) = norm.find(':') {
            root_prefix = norm[..=colon].to_string();
            let after = &norm[colon + 1..];
            if let Some(stripped) = after.strip_prefix('\\') {
                root_prefix.push('\\');
                rest = stripped;
            } else {
                rest = after;
            }
        }
    }

    let segments: Vec<String> = rest
        .split(sep)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    (root_prefix, segments)
}

fn format_path(root_prefix: &str, segments: &[String], sep: char) -> String {
    if segments.is_empty() {
        return root_prefix.to_string();
    }
    let body = segments.join(&sep.to_string());
    if root_prefix.is_empty() {
        body
    } else if root_prefix.ends_with(sep) || root_prefix == "/" {
        format!("{root_prefix}{body}")
    } else {
        format!("{root_prefix}{sep}{body}")
    }
}

fn truncate_chars(path: &str, max_width: f32, measure: &impl Fn(&str) -> f32) -> String {
    if measure(path) <= max_width {
        return path.to_string();
    }

    let chars: Vec<char> = path.chars().collect();
    for tail_n in (1..=chars.len()).rev() {
        let tail: String = chars[chars.len() - tail_n..].iter().collect();
        let candidate = format!("{ELLIPSIS}{tail}");
        if measure(&candidate) <= max_width {
            return candidate;
        }
    }

    let mut end = path.len();
    while end > 0 {
        while end > 0 && !path.is_char_boundary(end) {
            end -= 1;
        }
        let prefix = &path[..end];
        if measure(prefix) <= max_width {
            return if end == path.len() {
                path.to_string()
            } else {
                format!("{prefix}{ELLIPSIS}")
            };
        }
        end = end.saturating_sub(1);
    }
    ELLIPSIS.to_string()
}

#[cfg(test)]
fn monospace_measure(ch_width: f32) -> impl Fn(&str) -> f32 + Copy {
    move |s: &str| s.chars().count() as f32 * ch_width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_no_elision_when_fits() {
        let m = monospace_measure(8.0);
        let path = "/home/alex/proj";
        assert_eq!(elide_path_for_width(path, 200.0, m), path);
    }

    #[test]
    fn unix_middle_elide() {
        let m = monospace_measure(8.0);
        let path = "/home/alex/git/repos/deep/nested/readme.md";
        let out = elide_path_for_width(path, 184.0, m);
        assert!(out.contains(ELLIPSIS), "out={out:?}");
        assert!(
            out.starts_with("/home/alex") || out.starts_with("/home"),
            "out={out:?}"
        );
        assert!(out.ends_with("readme.md"), "out={out:?}");
    }

    #[test]
    fn windows_style_backslash() {
        let m = monospace_measure(7.0);
        let path = r"C:\Users\alexokita\git_repos\deep\folder\file.bin";
        let out = elide_path_for_width(path, 130.0, m);
        assert!(out.contains(ELLIPSIS));
        assert!(out.starts_with(r"C:\Users"));
        assert!(out.ends_with("file.bin"));
    }

    #[test]
    fn narrow_width_still_shows_ends() {
        let m = monospace_measure(8.0);
        let path = "/vol/data/αβ/γ/file.txt";
        let out = elide_path_for_width(path, 104.0, m);
        assert!(out.contains(ELLIPSIS), "out={out:?}");
        assert!(out.ends_with("file.txt"), "out={out:?}");
    }

    #[test]
    fn non_ascii_prefix_preserved_when_possible() {
        let m = monospace_measure(9.0);
        let path = "/home/ユーザー/docs/文書/資料.pdf";
        let out = elide_path_for_width(path, 140.0, m);
        assert!(out.ends_with("資料.pdf"));
    }

    #[test]
    fn very_narrow_falls_back_to_truncation() {
        let m = monospace_measure(8.0);
        let path = "/a/medium/length/path/data.bin";
        let out = elide_path_for_width(path, 24.0, m);
        assert!(m(&out) <= 24.0 + 0.01);
    }
}
