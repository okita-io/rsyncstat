//! Keyboard-first UI: Tab focus order, Alt mnemonics, and global shortcuts.
//!
//! egui 0.37 does not ship access keys; see <https://github.com/emilk/egui/issues/429>.

use eframe::egui::{
    self, text::LayoutJob, Button, Event, Key, Modifiers, Rect, Response, RichText, Sense, Stroke,
    StrokeKind, TextFormat, Ui,
};
use std::collections::HashSet;

/// Stable focus targets in tab order (subset may be hidden in single-pane mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FocusId {
    OpenSourceA,
    OpenTargetB,
    DualPane,
    MnemonicVisToggle,
    SyncMethod,
    ExcludeTypes,
    ExcludeDirs,
    ApplyRescan,
    RunSync,
    TreeTabA,
    TreeTabB,
    DirectoryTree,
    PaneAChoose,
    PaneARescan,
    TreemapA,
    Splitter,
    PaneBChoose,
    PaneBRescan,
    TreemapB,
}

/// Full tab order; [`visible_tab_order`] omits pane B when not side-by-side.
pub const TAB_ORDER: &[FocusId] = &[
    FocusId::OpenSourceA,
    FocusId::OpenTargetB,
    FocusId::DualPane,
    FocusId::MnemonicVisToggle,
    FocusId::SyncMethod,
    FocusId::ExcludeTypes,
    FocusId::ExcludeDirs,
    FocusId::ApplyRescan,
    FocusId::RunSync,
    FocusId::TreeTabA,
    FocusId::TreeTabB,
    FocusId::DirectoryTree,
    FocusId::PaneAChoose,
    FocusId::PaneARescan,
    FocusId::TreemapA,
    FocusId::Splitter,
    FocusId::PaneBChoose,
    FocusId::PaneBRescan,
    FocusId::TreemapB,
];

/// Toolbar / pane labels with `&` access keys. Kept here so tests can check collisions.
pub const MNEMONIC_LABELS: &[(&'static str, FocusId)] = &[
    ("&Open source (A)…", FocusId::OpenSourceA),
    ("Open &target (B)…", FocusId::OpenTargetB),
    ("&Split view", FocusId::DualPane),
    ("&Underline keys", FocusId::MnemonicVisToggle),
    ("Sync &mode", FocusId::SyncMethod),
    ("E&xclude types", FocusId::ExcludeTypes),
    ("Exclude &folders", FocusId::ExcludeDirs),
    ("Appl&y && rescan", FocusId::ApplyRescan),
    ("Ru&n sync", FocusId::RunSync),
    ("Tree &A", FocusId::TreeTabA),
    ("Tree &B", FocusId::TreeTabB),
    ("&Hierarchy", FocusId::DirectoryTree),
    ("&Choose folder…", FocusId::PaneAChoose),
    ("&Rescan", FocusId::PaneARescan),
    ("T&reemap A", FocusId::TreemapA),
    ("S&plitter", FocusId::Splitter),
    ("&Choose folder…", FocusId::PaneBChoose),
    ("&Rescan", FocusId::PaneBRescan),
    ("Treemap (&view)", FocusId::TreemapB),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MnemonicVisibility {
    Always,
    WhenAltHeld,
}

impl Default for MnemonicVisibility {
    fn default() -> Self {
        #[cfg(target_os = "linux")]
        {
            MnemonicVisibility::Always
        }
        #[cfg(not(target_os = "linux"))]
        {
            MnemonicVisibility::WhenAltHeld
        }
    }
}

#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub enum KeyAction {
    #[default]
    None,
    OpenFolder(crate::ActivePane),
    Rescan(crate::ActivePane),
    ToggleDualPane,
    ToggleMnemonicVis,
    SetActivePane(crate::ActivePane),
    FocusTreeTab(crate::ActivePane),
    TreemapParent(crate::ActivePane),
    TreemapChild(crate::ActivePane),
    RefreshCompare,
    StartSync,
    ApplyExcludes,
    CancelScan(crate::ActivePane),
    CancelSync,
    ActivateFocused,
    ToggleHelp,
}

#[derive(Clone, Debug)]
pub struct KeyboardState {
    pub focused: FocusId,
    pub mnemonic_vis: MnemonicVisibility,
    pub help_open: bool,
    pub pending: KeyAction,
    last_mnemonic: Option<(char, FocusId)>,
}

impl Default for KeyboardState {
    fn default() -> Self {
        Self {
            focused: FocusId::OpenSourceA,
            mnemonic_vis: MnemonicVisibility::default(),
            help_open: false,
            pending: KeyAction::None,
            last_mnemonic: None,
        }
    }
}

#[allow(deprecated)]
pub fn focus_egui_id(slot: FocusId) -> egui::Id {
    egui::Id::new("rsyncstat_focus").with(u32::from(slot as u8))
}

pub fn visible_tab_order(dual_pane: bool) -> Vec<FocusId> {
    TAB_ORDER
        .iter()
        .copied()
        .filter(|id| {
            if dual_pane {
                true
            } else {
                !matches!(
                    id,
                    FocusId::OpenTargetB
                        | FocusId::PaneBChoose
                        | FocusId::PaneBRescan
                        | FocusId::TreemapB
                        | FocusId::Splitter
                        | FocusId::TreeTabB
                )
            }
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct ParsedLabel {
    pub display: String,
    pub access_key: Option<char>,
    pub underline_index: Option<usize>,
}

/// Parse `&letter` mnemonics; `&&` → literal `&`.
pub fn parse_access_label(raw: &str) -> ParsedLabel {
    let mut display = String::with_capacity(raw.len());
    let mut access_key = None;
    let mut underline_index = None;
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '&' {
            if chars.peek() == Some(&'&') {
                chars.next();
                display.push('&');
            } else if let Some(&letter) = chars.peek() {
                chars.next();
                underline_index = Some(display.len());
                access_key = Some(letter.to_ascii_lowercase());
                display.push(letter);
            } else {
                display.push('&');
            }
        } else {
            display.push(ch);
        }
    }
    ParsedLabel {
        display,
        access_key,
        underline_index,
    }
}

pub fn layout_job_for_label(
    ui: &Ui,
    parsed: &ParsedLabel,
    show_underline: bool,
    style: egui::TextStyle,
) -> LayoutJob {
    let font_id = style.resolve(ui.style());
    let color = ui.visuals().text_color();
    let underline_stroke = Stroke::new(1.0, color);
    let mut job = LayoutJob::default();
    let base = TextFormat {
        font_id: font_id.clone(),
        color,
        ..Default::default()
    };
    let chars: Vec<char> = parsed.display.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if show_underline && parsed.underline_index == Some(i) {
            let ch = chars[i].to_string();
            job.append(
                &ch,
                0.0,
                TextFormat {
                    underline: underline_stroke,
                    ..base.clone()
                },
            );
            i += 1;
        } else {
            let start = i;
            while i < chars.len() && parsed.underline_index != Some(i) {
                i += 1;
            }
            job.append(&parsed.display[start..i], 0.0, base.clone());
        }
    }
    job
}

pub fn show_mnemonic_underlines(state: &KeyboardState, ctx: &egui::Context) -> bool {
    match state.mnemonic_vis {
        MnemonicVisibility::Always => true,
        MnemonicVisibility::WhenAltHeld => {
            ctx.input(|i| i.modifiers.alt)
                || state
                    .last_mnemonic
                    .is_some_and(|_| ctx.input(|i| i.modifiers.alt))
        }
    }
}

pub fn paint_focus_ring(ui: &Ui, rect: Rect, focused: bool) {
    if !focused {
        return;
    }
    let stroke = Stroke::new(2.0, ui.visuals().selection.stroke.color.gamma_multiply(1.1));
    ui.painter()
        .rect_stroke(rect.expand(2.0), 4.0, stroke, StrokeKind::Outside);
}

pub fn mnemonic_button(
    ui: &mut Ui,
    slot: FocusId,
    raw_label: &str,
    show_underline: bool,
) -> Response {
    let id = focus_egui_id(slot);
    let parsed = parse_access_label(raw_label);
    let job = layout_job_for_label(ui, &parsed, show_underline, egui::TextStyle::Button);
    ui.push_id(id, |ui| ui.add(Button::new(job).sense(Sense::click())))
        .inner
}

pub fn mnemonic_checkbox(
    ui: &mut Ui,
    slot: FocusId,
    raw_label: &str,
    selected: &mut bool,
    show_underline: bool,
) -> Response {
    let id = focus_egui_id(slot);
    let parsed = parse_access_label(raw_label);
    let job = layout_job_for_label(ui, &parsed, show_underline, egui::TextStyle::Body);
    ui.push_id(id, |ui| ui.checkbox(selected, job)).inner
}

pub fn mnemonic_selectable(
    ui: &mut Ui,
    slot: FocusId,
    raw_label: &str,
    selected: bool,
    show_underline: bool,
) -> Response {
    let id = focus_egui_id(slot);
    let parsed = parse_access_label(raw_label);
    let job = layout_job_for_label(ui, &parsed, show_underline, egui::TextStyle::Body);
    ui.push_id(id, |ui| ui.selectable_label(selected, job))
        .inner
}

/// Invisible focus target for treemap / splitter regions.
pub fn focus_region(ui: &mut Ui, slot: FocusId, min_size: egui::Vec2, sense: Sense) -> Response {
    let (rect, _response) = ui.allocate_exact_size(min_size, sense);
    let _ = rect;
    let id = focus_egui_id(slot);
    ui.interact(rect, id, sense)
}

pub fn raw_label_for(slot: FocusId) -> &'static str {
    MNEMONIC_LABELS
        .iter()
        .find_map(|(label, id)| (*id == slot).then_some(*label))
        .expect("every FocusId has a MNEMONIC_LABELS entry")
}

pub fn focus_for_mnemonic(letter: char, dual_pane: bool) -> Option<FocusId> {
    let letter = letter.to_ascii_lowercase();
    let visible: HashSet<FocusId> = visible_tab_order(dual_pane).into_iter().collect();
    MNEMONIC_LABELS
        .iter()
        .filter(|(_, id)| visible.contains(id))
        .find_map(|(label, id)| {
            let parsed = parse_access_label(label);
            (parsed.access_key == Some(letter)).then_some(*id)
        })
}

/// All access keys that must be unique within a view (per-pane duplicates excluded).
pub fn toolbar_mnemonic_keys(dual_pane: bool) -> Vec<char> {
    let mut keys = Vec::new();
    for (label, id) in MNEMONIC_LABELS {
        if matches!(id, FocusId::PaneBChoose | FocusId::PaneBRescan) && !dual_pane {
            continue;
        }
        if matches!(
            id,
            FocusId::PaneAChoose
                | FocusId::PaneARescan
                | FocusId::PaneBChoose
                | FocusId::PaneBRescan
        ) {
            // Pane rows may reuse Choose/Rescan letters; skip for global uniqueness table.
            continue;
        }
        if let Some(k) = parse_access_label(label).access_key {
            keys.push(k);
        }
    }
    keys
}

pub fn handle_frame_input(
    ctx: &egui::Context,
    state: &mut KeyboardState,
    dual_pane: bool,
    active_pane: crate::ActivePane,
    source_scanning: bool,
    target_scanning: bool,
    syncing: bool,
) {
    state.pending = KeyAction::None;
    let order = visible_tab_order(dual_pane);

    ctx.input_mut(|input| {
        if input.key_pressed(Key::F1)
            || (input.key_pressed(Key::Questionmark) && !input.modifiers.ctrl)
        {
            state.help_open = !state.help_open;
            input.consume_key(Modifiers::NONE, Key::F1);
        }

        if input.key_pressed(Key::Escape) {
            if state.help_open {
                state.help_open = false;
            } else if syncing {
                state.pending = KeyAction::CancelSync;
            } else if source_scanning {
                state.pending = KeyAction::CancelScan(crate::ActivePane::Source);
            } else if target_scanning {
                state.pending = KeyAction::CancelScan(crate::ActivePane::Target);
            }
            input.consume_key(Modifiers::NONE, Key::Escape);
        }

        if input.key_pressed(Key::Tab) {
            let idx = order
                .iter()
                .position(|&id| id == state.focused)
                .unwrap_or(0);
            let next = if input.modifiers.shift {
                idx.checked_sub(1).unwrap_or(order.len() - 1)
            } else {
                (idx + 1) % order.len()
            };
            state.focused = order[next];
            input.consume_key(input.modifiers, Key::Tab);
        }

        if input.modifiers.alt && input.modifiers.ctrl == false && input.modifiers.mac_cmd == false
        {
            for event in &input.events {
                if let Event::Text(text) = event {
                    if let Some(ch) = text.chars().next() {
                        if ch.is_ascii_alphabetic() {
                            if let Some(slot) = focus_for_mnemonic(ch, dual_pane) {
                                state.focused = slot;
                                state.pending = KeyAction::ActivateFocused;
                                state.last_mnemonic = Some((ch.to_ascii_lowercase(), slot));
                            }
                        }
                    }
                }
            }
        }

        let ctrl = input.modifiers.ctrl || input.modifiers.mac_cmd;
        if ctrl && input.key_pressed(Key::Num1) {
            state.focused = FocusId::TreemapA;
            state.pending = KeyAction::SetActivePane(crate::ActivePane::Source);
        } else if ctrl && input.key_pressed(Key::Num2) && dual_pane {
            state.focused = FocusId::TreemapB;
            state.pending = KeyAction::SetActivePane(crate::ActivePane::Target);
        } else if ctrl && input.key_pressed(Key::O) {
            state.pending = KeyAction::OpenFolder(active_pane);
        } else if ctrl && input.key_pressed(Key::R) || input.key_pressed(Key::F5) {
            state.pending = KeyAction::Rescan(active_pane);
        } else if ctrl && input.modifiers.shift && input.key_pressed(Key::C) {
            state.pending = KeyAction::RefreshCompare;
        }

        let treemap_pane = if state.focused == FocusId::TreemapA {
            Some(crate::ActivePane::Source)
        } else if state.focused == FocusId::TreemapB {
            Some(crate::ActivePane::Target)
        } else {
            None
        };
        if let Some(pane) = treemap_pane {
            if input.key_pressed(Key::ArrowLeft) || input.key_pressed(Key::ArrowUp) {
                state.pending = KeyAction::TreemapParent(pane);
            } else if input.key_pressed(Key::ArrowRight) || input.key_pressed(Key::ArrowDown) {
                state.pending = KeyAction::TreemapChild(pane);
            }
        }

        if state.focused != FocusId::ExcludeTypes && state.focused != FocusId::ExcludeDirs {
            if input.key_pressed(Key::Enter) || input.key_pressed(Key::Space) {
                state.pending = KeyAction::ActivateFocused;
            }
        }
    });
}

pub fn take_pending(state: &mut KeyboardState) -> KeyAction {
    std::mem::replace(&mut state.pending, KeyAction::None)
}

pub fn is_focused(state: &KeyboardState, slot: FocusId) -> bool {
    state.focused == slot
}

pub fn cheat_sheet_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("Tab / Shift+Tab", "Move focus"),
        ("Enter / Space", "Activate focused control"),
        ("Escape", "Close help · cancel sync/scan"),
        ("Alt+letter", "Mnemonic (underline in labels)"),
        ("F1 or ?", "This cheat sheet"),
        ("Ctrl+1 / Ctrl+2", "Focus treemap A / B"),
        ("Ctrl+O", "Choose folder (active pane)"),
        ("Ctrl+R / F5", "Rescan active pane"),
        ("Ctrl+Shift+C", "Refresh compare"),
        ("← ↑ / → ↓", "Treemap: parent / child selection"),
    ]
}

pub fn draw_help_window(ctx: &egui::Context, open: &mut bool) {
    if !*open {
        return;
    }
    egui::Window::new("Keyboard shortcuts")
        .collapsible(false)
        .resizable(true)
        .default_width(420.0)
        .show(ctx, |ui| {
            ui.label(
                RichText::new("Global shortcuts use Ctrl so Super stays free on Hyprland.")
                    .small()
                    .weak(),
            );
            ui.add_space(6.0);
            egui::Grid::new("kbd_help_grid")
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for (key, action) in cheat_sheet_entries() {
                        ui.monospace(*key);
                        ui.label(*action);
                        ui.end_row();
                    }
                });
            ui.add_space(8.0);
            if ui.button("Close (Esc)").clicked() {
                *open = false;
            }
        });
}

/// Move treemap selection up one level (parent).
pub fn treemap_nav_parent(selected: &mut Option<Vec<String>>) {
    if let Some(path) = selected.as_mut() {
        if path.len() > 1 {
            path.pop();
        }
    }
}

/// Move treemap selection into the largest non-excluded child directory.
pub fn treemap_nav_child(root: &rsyncstat_model::TreeNode, selected: &mut Option<Vec<String>>) {
    let path = match selected {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            *selected = Some(vec![root.name.clone()]);
            return;
        }
    };
    let Some(node) = crate::find_by_path(root, &path) else {
        return;
    };
    if !node.is_dir || node.excluded {
        return;
    }
    let child = node
        .children
        .iter()
        .filter(|c| c.is_dir && !c.excluded)
        .max_by_key(|c| c.size);
    if let Some(child) = child {
        let mut next = path;
        next.push(child.name.clone());
        *selected = Some(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ampersand_mnemonic() {
        let p = parse_access_label("&Scan");
        assert_eq!(p.display, "Scan");
        assert_eq!(p.access_key, Some('s'));
        assert_eq!(p.underline_index, Some(0));
    }

    #[test]
    fn parse_escaped_ampersand() {
        let p = parse_access_label("&Apply && rescan");
        assert_eq!(p.display, "Apply & rescan");
        assert_eq!(p.access_key, Some('a'));
    }

    #[test]
    fn parse_double_ampersand_literal() {
        let p = parse_access_label("A && B");
        assert_eq!(p.display, "A & B");
        assert_eq!(p.access_key, None);
    }

    #[test]
    fn toolbar_mnemonics_are_unique_single_pane() {
        let keys = toolbar_mnemonic_keys(false);
        let mut seen = HashSet::new();
        for k in keys {
            assert!(
                seen.insert(k),
                "duplicate mnemonic '{k}' in single-pane toolbar"
            );
        }
    }

    #[test]
    fn toolbar_mnemonics_are_unique_dual_pane() {
        let keys = toolbar_mnemonic_keys(true);
        let mut seen = HashSet::new();
        for k in keys {
            assert!(
                seen.insert(k),
                "duplicate mnemonic '{k}' in dual-pane toolbar"
            );
        }
    }

    #[test]
    fn focus_order_runs_toolbar_then_pane_a_before_pane_b() {
        let dual = visible_tab_order(true);
        let a_choose = dual
            .iter()
            .position(|&id| id == FocusId::PaneAChoose)
            .unwrap();
        let a_map = dual.iter().position(|&id| id == FocusId::TreemapA).unwrap();
        let b_choose = dual
            .iter()
            .position(|&id| id == FocusId::PaneBChoose)
            .unwrap();
        assert!(a_choose < a_map);
        assert!(a_map < b_choose);
        assert!(
            dual.iter()
                .position(|&id| id == FocusId::OpenSourceA)
                .unwrap()
                < dual.iter().position(|&id| id == FocusId::TreemapA).unwrap()
        );
    }

    #[test]
    fn tab_order_starts_with_open_source_and_includes_treemaps() {
        assert_eq!(TAB_ORDER[0], FocusId::OpenSourceA);
        assert!(TAB_ORDER.contains(&FocusId::TreemapA));
        assert!(TAB_ORDER.contains(&FocusId::TreemapB));
        let single = visible_tab_order(false);
        assert!(!single.contains(&FocusId::TreemapB));
        let dual = visible_tab_order(true);
        assert!(
            dual.iter().position(|&a| a == FocusId::TreemapA)
                < dual.iter().position(|&b| b == FocusId::PaneBChoose)
        );
    }

    #[test]
    fn focus_ids_align_with_mnemonic_table() {
        for &id in TAB_ORDER {
            let in_table = MNEMONIC_LABELS.iter().any(|(_, slot)| *slot == id);
            assert!(in_table, "FocusId {:?} missing from MNEMONIC_LABELS", id);
        }
    }
}
