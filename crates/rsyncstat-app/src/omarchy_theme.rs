//! Omarchy desktop theme → egui chrome.
//!
//! Reads `theme/colors.toml` under the Omarchy current-theme directory
//! (`~/.local/state/omarchy/current`, or `RSYNCSTAT_OMARCHY_THEME_DIR`) and
//! rebuilds [`egui::Visuals`] when that file changes. A theme switch replaces
//! the symlink, so each check reads the path again instead of holding an inode.
//!
//! On Hyprland, button, field, menu and focus corners follow
//! `hyprctl -j getoption decoration:rounding`. Without Hyprland they stay at
//! [`crate::ui::CONTROL_RADIUS`].
//!
//! Treemap file colors stay the deterministic extension palette in
//! `rsyncstat-model`. This module only colors window chrome, text, selection,
//! focus, and status.

use eframe::egui::{self, Color32};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Accent used when `colors.toml` is missing or has no `accent` key.
/// Same fallback Omarchy's own apps use.
pub const FALLBACK_ACCENT: Color32 = Color32::from_rgb(0xFF, 0xD6, 0x0A);

/// How often an idle window re-reads the theme file.
pub const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Wait until the file has stayed the same this long before applying it,
/// so a theme switch that rewrites the file mid-way is not applied half-written.
const DEBOUNCE: Duration = Duration::from_millis(80);

/// How often to ask Hyprland for `decoration:rounding`.
const ROUNDING_INTERVAL: Duration = Duration::from_secs(1);

/// Give up on a `hyprctl` call that does not finish. The previous radius stays.
const ROUNDING_TIMEOUT: Duration = Duration::from_millis(500);

/// Live palette plus the visuals built from it. `applied` is `None` when the
/// file is missing.
pub struct ThemeSource {
    root: PathBuf,
    applied: Option<Vec<u8>>,
    visuals: egui::Visuals,
    last_check: Instant,
    pending: Option<(Option<Vec<u8>>, Instant)>,
    /// `hyprctl` binary. `None` off Hyprland, so corners stay at [`crate::ui::CONTROL_RADIUS`].
    rounding_cmd: Option<PathBuf>,
    rounding_child: Option<Child>,
    rounding_started: Option<Instant>,
    next_rounding_at: Instant,
    /// Effective corner radius in logical pixels, clamped to `u8` for egui.
    radius: u8,
}

impl ThemeSource {
    /// The user's live Omarchy theme. Tests build a [`Self::with_hyprctl`] on a temp directory.
    pub fn live() -> Self {
        Self::with_hyprctl(live_root(), live_hyprctl())
    }

    /// `hyprctl` is queried once here, then again about once a second.
    /// Tests pass a script that prints `decoration:rounding` JSON.
    pub fn with_hyprctl(root: PathBuf, hyprctl: Option<PathBuf>) -> Self {
        let applied = read_colors(&root);
        let mut visuals = visuals_from_bytes(applied.as_deref());
        let now = Instant::now();
        let mut radius = crate::ui::CONTROL_RADIUS;
        if let Some(cmd) = &hyprctl {
            if let Some(value) = query_rounding(cmd, ROUNDING_TIMEOUT) {
                radius = value;
            }
        }
        stamp_radius(&mut visuals, radius);
        Self {
            root,
            applied,
            visuals,
            last_check: now,
            pending: None,
            rounding_cmd: hyprctl,
            rounding_child: None,
            rounding_started: None,
            next_rounding_at: now + ROUNDING_INTERVAL,
            radius,
        }
    }

    pub fn visuals(&self) -> &egui::Visuals {
        &self.visuals
    }

    pub fn corner_radius(&self) -> u8 {
        self.radius
    }

    /// Write the palette into both egui theme slots so a system light/dark
    /// flip does not restore the default egui blues.
    pub fn apply(&self, ctx: &egui::Context) {
        let visuals = self.visuals().clone();
        ctx.set_visuals_of(egui::Theme::Dark, visuals.clone());
        ctx.set_visuals_of(egui::Theme::Light, visuals);
    }

    /// Returns true when the palette changed and should be applied.
    pub fn poll(&mut self) -> bool {
        self.poll_at(Instant::now())
    }

    pub fn poll_at(&mut self, now: Instant) -> bool {
        let mut changed = self.reap_rounding(now);
        if self.pending.is_none() && elapsed(self.last_check, now) < POLL_INTERVAL {
            return changed;
        }
        self.last_check = now;
        changed |= self.poll_colors(now);
        self.maybe_spawn_rounding(now);
        changed
    }
}

impl Drop for ThemeSource {
    fn drop(&mut self) {
        if let Some(mut child) = self.rounding_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn poll_colors_on(source: &mut ThemeSource, now: Instant) -> bool {
    let current = read_colors(&source.root);
    if current == source.applied {
        source.pending = None;
        return false;
    }
    match &source.pending {
        Some((bytes, at)) if bytes == &current && elapsed(*at, now) >= DEBOUNCE => {
            source.applied = current;
            source.visuals = visuals_from_bytes(source.applied.as_deref());
            stamp_radius(&mut source.visuals, source.radius);
            source.pending = None;
            true
        }
        Some((bytes, _)) if bytes == &current => false,
        _ => {
            source.pending = Some((current, now));
            false
        }
    }
}

impl ThemeSource {
    fn poll_colors(&mut self, now: Instant) -> bool {
        poll_colors_on(self, now)
    }

    fn set_radius(&mut self, radius: u8) -> bool {
        if self.radius == radius {
            return false;
        }
        self.radius = radius;
        stamp_radius(&mut self.visuals, radius);
        true
    }

    fn reap_rounding(&mut self, now: Instant) -> bool {
        let waited = self.rounding_child.as_mut().map(wait_rounding);
        match waited {
            Some(Waited::Done(status)) => self.finish_rounding(status),
            Some(Waited::Running) => {
                let timed_out = self
                    .rounding_started
                    .is_some_and(|at| elapsed(at, now) >= ROUNDING_TIMEOUT);
                if timed_out {
                    if let Some(mut child) = self.rounding_child.take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    self.rounding_started = None;
                }
                false
            }
            Some(Waited::Failed) => {
                self.rounding_child = None;
                self.rounding_started = None;
                false
            }
            None => false,
        }
    }

    fn finish_rounding(&mut self, status: std::process::ExitStatus) -> bool {
        let Some(mut child) = self.rounding_child.take() else {
            return false;
        };
        self.rounding_started = None;
        let mut out = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = stdout.read_to_string(&mut out);
        }
        let _ = child.wait();
        if !status.success() {
            return false;
        }
        parse_rounding(&out).is_some_and(|radius| self.set_radius(radius))
    }

    fn maybe_spawn_rounding(&mut self, now: Instant) {
        if self.rounding_child.is_some()
            || self.rounding_cmd.is_none()
            || now < self.next_rounding_at
        {
            return;
        }
        let Some(cmd) = self.rounding_cmd.clone() else {
            return;
        };
        self.next_rounding_at = now + ROUNDING_INTERVAL;
        if let Some(child) = spawn_rounding(&cmd) {
            self.rounding_child = Some(child);
            self.rounding_started = Some(now);
        }
    }
}

fn live_hyprctl() -> Option<PathBuf> {
    if let Some(cmd) = std::env::var_os("RSYNCSTAT_HYPRCTL") {
        if !cmd.is_empty() {
            return Some(PathBuf::from(cmd));
        }
    }
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("hyprctl"))
        .find(|candidate| candidate.is_file())
}

enum Waited {
    Running,
    Done(std::process::ExitStatus),
    Failed,
}

fn wait_rounding(child: &mut Child) -> Waited {
    match child.try_wait() {
        Ok(None) => Waited::Running,
        Ok(Some(status)) => Waited::Done(status),
        Err(_) => Waited::Failed,
    }
}

fn spawn_rounding(cmd: &Path) -> Option<Child> {
    Command::new(cmd)
        .args(["-j", "getoption", "decoration:rounding"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

fn query_rounding(cmd: &Path, timeout: Duration) -> Option<u8> {
    let mut child = spawn_rounding(cmd)?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut out);
                }
                return parse_rounding(&out);
            }
            Ok(None) if elapsed(started, Instant::now()) >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => return None,
        }
    }
}

/// `{"option":"decoration:rounding","int":8,"set":true}` → `8`.
/// Missing, negative, or non-numeric values are ignored.
fn parse_rounding(text: &str) -> Option<u8> {
    let key = text.find("\"int\"")?;
    let after = text[key + 5..].trim_start();
    let rest = after.strip_prefix(':')?.trim_start();
    if rest.starts_with('-') {
        return None;
    }
    let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let value: u32 = digits.parse().ok()?;
    Some(u8::try_from(value).unwrap_or(u8::MAX))
}

fn stamp_radius(visuals: &mut egui::Visuals, radius: u8) {
    let corner = egui::CornerRadius::same(radius);
    visuals.window_corner_radius = corner;
    visuals.menu_corner_radius = corner;
    visuals.widgets.noninteractive.corner_radius = corner;
    visuals.widgets.inactive.corner_radius = corner;
    visuals.widgets.hovered.corner_radius = corner;
    visuals.widgets.active.corner_radius = corner;
    visuals.widgets.open.corner_radius = corner;
}

fn live_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("RSYNCSTAT_OMARCHY_THEME_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".local/state/omarchy/current")
}

fn elapsed(since: Instant, now: Instant) -> Duration {
    now.checked_duration_since(since).unwrap_or(Duration::ZERO)
}

fn read_colors(root: &Path) -> Option<Vec<u8>> {
    std::fs::read(root.join("theme/colors.toml")).ok()
}

fn visuals_from_bytes(bytes: Option<&[u8]>) -> egui::Visuals {
    match bytes {
        Some(bytes) => visuals_from_toml(&String::from_utf8_lossy(bytes)),
        None => fallback_visuals(),
    }
}

fn fallback_visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();
    paint_accent(&mut visuals, FALLBACK_ACCENT);
    visuals
}

fn visuals_from_toml(text: &str) -> egui::Visuals {
    let map = parse_assignments(text);
    let background = token(&map, "background");
    let dark = is_dark(map.get("mode").map(String::as_str), background);
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    let accent = token(&map, "accent").unwrap_or(FALLBACK_ACCENT);
    let background = background.unwrap_or(visuals.window_fill);
    let darker = token(&map, "darker_background").unwrap_or(visuals.extreme_bg_color);
    let lighter =
        token(&map, "lighter_background").unwrap_or(visuals.widgets.inactive.weak_bg_fill);
    let foreground = token(&map, "foreground").unwrap_or(visuals.text_color());
    let dark_foreground = token(&map, "dark_foreground").unwrap_or(visuals.weak_text_color());
    let muted = token(&map, "muted").unwrap_or(visuals.widgets.noninteractive.bg_stroke.color);
    let selection = token(&map, "selection").unwrap_or(visuals.selection.bg_fill);
    let red = token(&map, "red").unwrap_or(visuals.error_fg_color);
    let yellow = token(&map, "yellow").unwrap_or(visuals.warn_fg_color);
    let blue = token(&map, "blue").unwrap_or(accent);
    let on_accent = readable_on(accent);
    let on_selection = readable_on(selection);

    visuals.dark_mode = dark;
    visuals.override_text_color = Some(foreground);
    visuals.weak_text_color = Some(dark_foreground);
    visuals.window_fill = background;
    visuals.panel_fill = background;
    visuals.faint_bg_color = lighter;
    visuals.extreme_bg_color = darker;
    visuals.text_edit_bg_color = Some(darker);
    visuals.code_bg_color = darker;
    visuals.window_stroke.color = muted;
    visuals.hyperlink_color = blue;
    visuals.warn_fg_color = yellow;
    visuals.error_fg_color = red;
    visuals.selection.bg_fill = selection;
    visuals.selection.stroke.color = accent;
    visuals.text_cursor.stroke.color = accent;

    paint_widget(
        &mut visuals.widgets.noninteractive,
        background,
        background,
        muted,
        foreground,
    );
    paint_widget(
        &mut visuals.widgets.inactive,
        lighter,
        lighter,
        muted,
        foreground,
    );
    paint_widget(
        &mut visuals.widgets.hovered,
        lighter.lerp_to_gamma(accent, 0.28),
        lighter.lerp_to_gamma(accent, 0.16),
        accent,
        foreground,
    );
    paint_widget(
        &mut visuals.widgets.active,
        accent,
        accent.lerp_to_gamma(background, 0.2),
        accent,
        on_accent,
    );
    paint_widget(
        &mut visuals.widgets.open,
        selection,
        selection,
        accent,
        on_selection,
    );
    visuals
}

fn paint_accent(visuals: &mut egui::Visuals, accent: Color32) {
    visuals.hyperlink_color = accent;
    visuals.selection.stroke.color = accent;
    visuals.text_cursor.stroke.color = accent;
    visuals.widgets.hovered.bg_stroke.color = accent;
    visuals.widgets.active.bg_stroke.color = accent;
    visuals.widgets.active.weak_bg_fill = accent;
    visuals.widgets.active.fg_stroke.color = readable_on(accent);
}

fn paint_widget(
    widget: &mut egui::style::WidgetVisuals,
    bg: Color32,
    weak: Color32,
    stroke: Color32,
    fg: Color32,
) {
    widget.bg_fill = bg;
    widget.weak_bg_fill = weak;
    widget.bg_stroke.color = stroke;
    widget.fg_stroke.color = fg;
}

fn token(map: &BTreeMap<String, String>, key: &str) -> Option<Color32> {
    map.get(key).and_then(|value| parse_hex(value))
}

fn is_dark(mode: Option<&str>, background: Option<Color32>) -> bool {
    match mode.map(str::to_ascii_lowercase).as_deref() {
        Some("dark") => true,
        Some("light") => false,
        _ => background.is_none_or(|color| luminance(color) < 0.4),
    }
}

/// Black or white, whichever reads on `color` (WCAG relative luminance).
fn readable_on(color: Color32) -> Color32 {
    if luminance(color) > 0.179 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

fn luminance(color: Color32) -> f32 {
    let [r, g, b, _] = color.to_array();
    0.2126 * srgb_channel(r) + 0.7152 * srgb_channel(g) + 0.0722 * srgb_channel(b)
}

fn srgb_channel(channel: u8) -> f32 {
    let v = f32::from(channel) / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn parse_assignments(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = strip_comment(line).trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.contains(' ') {
            continue;
        }
        let value = value
            .trim()
            .trim_matches(|ch| ch == '"' || ch == '\'')
            .trim();
        map.insert(key.to_string(), value.to_string());
    }
    map
}

fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    for (index, ch) in line.char_indices() {
        match ch {
            '"' | '\'' if quote.is_none() => quote = Some(ch),
            q if quote == Some(q) => quote = None,
            '#' if quote.is_none() => return &line[..index],
            _ => {}
        }
    }
    line
}

fn parse_hex(text: &str) -> Option<Color32> {
    let hex = text.trim().trim_start_matches('#');
    match hex.len() {
        6 => {
            let n = u32::from_str_radix(hex, 16).ok()?;
            Some(Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
        }
        8 => {
            let n = u32::from_str_radix(hex, 16).ok()?;
            Some(Color32::from_rgba_unmultiplied(
                (n >> 24) as u8,
                (n >> 16) as u8,
                (n >> 8) as u8,
                n as u8,
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "rsyncstat-omarchy-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            std::fs::create_dir_all(dir.join("theme")).expect("temp theme dir");
            Self(dir)
        }

        fn write(&self, body: &str) {
            std::fs::write(self.0.join("theme/colors.toml"), body).expect("write colors");
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_file_uses_egui_dark_and_fallback_accent() {
        let dir = Scratch::new("missing");
        let theme = ThemeSource::with_hyprctl(dir.0.clone(), None);
        let visuals = theme.visuals();
        assert!(visuals.dark_mode);
        assert_eq!(visuals.window_fill, egui::Visuals::dark().window_fill);
        assert_eq!(visuals.selection.stroke.color, FALLBACK_ACCENT);
        assert_eq!(visuals.hyperlink_color, FALLBACK_ACCENT);
        assert_eq!(readable_on(FALLBACK_ACCENT), Color32::BLACK);
    }

    #[test]
    fn light_theme_maps_chrome_and_status_colors() {
        let dir = Scratch::new("light");
        dir.write(
            r##"
            mode = "light" # desktop
            accent = "#205EA6"
            selection = "#CECDC3"
            muted = "#B7B5AC"
            background = "#FFFCF0"
            darker_background = "#E5E2D8"
            lighter_background = "#E6E4D9"
            foreground = "#100F0F"
            dark_foreground = "#878580"
            red = "#D14D41"
            yellow = "#D0A215"
            blue = "#205EA6"
            not a assignment
            accent_note = "nope"
            "##,
        );
        let visuals = ThemeSource::with_hyprctl(dir.0.clone(), None)
            .visuals()
            .clone();
        assert!(!visuals.dark_mode);
        assert_eq!(visuals.window_fill, Color32::from_rgb(0xFF, 0xFC, 0xF0));
        assert_eq!(visuals.panel_fill, visuals.window_fill);
        assert_eq!(
            visuals.extreme_bg_color,
            Color32::from_rgb(0xE5, 0xE2, 0xD8)
        );
        assert_eq!(visuals.faint_bg_color, Color32::from_rgb(0xE6, 0xE4, 0xD9));
        assert_eq!(visuals.text_color(), Color32::from_rgb(0x10, 0x0F, 0x0F));
        assert_eq!(
            visuals.weak_text_color(),
            Color32::from_rgb(0x87, 0x85, 0x80)
        );
        assert_eq!(
            visuals.selection.bg_fill,
            Color32::from_rgb(0xCE, 0xCD, 0xC3)
        );
        assert_eq!(
            visuals.selection.stroke.color,
            Color32::from_rgb(0x20, 0x5E, 0xA6)
        );
        assert_eq!(visuals.warn_fg_color, Color32::from_rgb(0xD0, 0xA2, 0x15));
        assert_eq!(visuals.error_fg_color, Color32::from_rgb(0xD1, 0x4D, 0x41));
        assert_eq!(visuals.hyperlink_color, Color32::from_rgb(0x20, 0x5E, 0xA6));
        assert_eq!(
            visuals.widgets.active.bg_fill,
            visuals.selection.stroke.color
        );
        assert_eq!(visuals.widgets.active.fg_stroke.color, Color32::WHITE);
        assert_eq!(
            visuals.widgets.noninteractive.bg_stroke.color,
            Color32::from_rgb(0xB7, 0xB5, 0xAC)
        );
    }

    #[test]
    fn mode_key_wins_and_background_infers_when_mode_is_absent() {
        let dark = visuals_from_toml("mode = \"dark\"\nbackground = \"#FFFFFF\"\n");
        assert!(dark.dark_mode);
        assert_eq!(dark.window_fill, Color32::from_rgb(0xFF, 0xFF, 0xFF));

        let inferred = visuals_from_toml("background = \"#FFFCF0\"\naccent = \"#112233\"\n");
        assert!(!inferred.dark_mode);
        assert_eq!(
            inferred.selection.stroke.color,
            Color32::from_rgb(0x11, 0x22, 0x33)
        );
        assert_eq!(
            readable_on(Color32::from_rgb(0x11, 0x22, 0x33)),
            Color32::WHITE
        );
    }

    #[test]
    fn reload_waits_until_the_new_file_stays_put() {
        let dir = Scratch::new("reload");
        dir.write("mode = \"dark\"\naccent = \"#112233\"\nbackground = \"#111111\"\n");
        let mut theme = ThemeSource::with_hyprctl(dir.0.clone(), None);
        assert_eq!(
            theme.visuals().selection.stroke.color,
            Color32::from_rgb(0x11, 0x22, 0x33)
        );

        dir.write("mode = \"light\"\naccent = \"#ABCDEF\"\nbackground = \"#FFFFFF\"\n");
        let t0 = Instant::now() + POLL_INTERVAL;
        assert!(!theme.poll_at(t0));
        assert_eq!(
            theme.visuals().selection.stroke.color,
            Color32::from_rgb(0x11, 0x22, 0x33)
        );
        assert!(!theme.poll_at(t0 + Duration::from_millis(79)));
        assert!(theme.poll_at(t0 + DEBOUNCE));
        assert!(!theme.visuals().dark_mode);
        assert_eq!(
            theme.visuals().selection.stroke.color,
            Color32::from_rgb(0xAB, 0xCD, 0xEF)
        );
        assert_eq!(theme.corner_radius(), crate::ui::CONTROL_RADIUS);
    }

    #[test]
    fn rounding_json_reads_the_int_field() {
        assert_eq!(
            parse_rounding(r#"{"option":"decoration:rounding","int": 12,"set":true}"#),
            Some(12)
        );
        assert_eq!(parse_rounding(r#"{"int":0}"#), Some(0));
        assert_eq!(parse_rounding(r#"{"int":400}"#), Some(255));
        assert_eq!(parse_rounding(r#"{"int":-1}"#), None);
        assert_eq!(parse_rounding("nope"), None);
    }

    #[test]
    fn missing_hyprctl_keeps_the_app_radius() {
        let dir = Scratch::new("no-hypr");
        let theme = ThemeSource::with_hyprctl(dir.0.clone(), Some(dir.0.join("missing")));
        assert_eq!(theme.corner_radius(), crate::ui::CONTROL_RADIUS);
        assert_eq!(
            theme.visuals().widgets.inactive.corner_radius.nw,
            crate::ui::CONTROL_RADIUS
        );
        assert_eq!(
            theme.visuals().window_corner_radius.nw,
            crate::ui::CONTROL_RADIUS
        );
    }

    #[cfg(unix)]
    #[test]
    fn rounding_follows_hyprctl_and_reloads() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Scratch::new("round");
        let response = dir.0.join("response.json");
        std::fs::write(&response, r#"{"int": 8}"#).unwrap();
        let script = dir.0.join("hyprctl");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ncat '{}'\n", response.display()),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let mut theme = ThemeSource::with_hyprctl(dir.0.clone(), Some(script));
        assert_eq!(theme.corner_radius(), 8);
        assert_eq!(theme.visuals().menu_corner_radius.nw, 8);
        assert_eq!(theme.visuals().widgets.active.corner_radius.nw, 8);

        std::fs::write(&response, r#"{"int":0}"#).unwrap();
        let t0 = Instant::now() + ROUNDING_INTERVAL;
        assert!(!theme.poll_at(t0));
        let mut updated = false;
        for step in 1..20 {
            if theme.poll_at(t0 + Duration::from_millis(15 * step)) && theme.corner_radius() == 0 {
                updated = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(updated);
        assert_eq!(theme.visuals().widgets.inactive.corner_radius.nw, 0);
    }

    #[cfg(unix)]
    #[test]
    fn rounding_query_gives_up_when_hyprctl_stalls() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Scratch::new("stall");
        let script = dir.0.join("hyprctl");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        let started = Instant::now();
        assert!(query_rounding(&script, Duration::from_millis(80)).is_none());
        assert!(started.elapsed() < Duration::from_millis(500));
    }
}
