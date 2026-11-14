//! Keyboard-first UI: Tab focus order, Alt mnemonics, and global shortcuts.
//!
//! egui 0.37 does not ship access keys; see <https://github.com/emilk/egui/issues/429>.

use eframe::egui::{
    self, text::LayoutJob, Button, Event, Key, Modifiers, Rect, Response, RichText, Sense, Stroke,
    StrokeKind, TextFormat, Ui, Vec2,
};
use std::collections::HashSet;

/// Stable focus targets in tab order. The directory tree is omitted until a scan exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FocusId {
    RunSync,
    TreeTabA,
    TreeTabB,
    ExcludeTypes,
    ExcludeDirs,
    ApplyRescan,
    DirectoryTree,
    PaneAChoose,
    OpenSourceA,
    PaneARescan,
    TreemapA,
    Splitter,
    PaneBChoose,
    OpenTargetB,
    PaneBRescan,
    TreemapB,
}

/// Row by row, left to right, matching the painted layout: the top toolbar row
/// across all three columns, then the pane button row, then the left panel
/// body, then the treemaps.
pub const TAB_ORDER: &[FocusId] = &[
    // Row 1: tree tabs, Run sync, then each pane's folder button.
    FocusId::TreeTabA,
    FocusId::TreeTabB,
    FocusId::RunSync,
    FocusId::PaneAChoose,
    FocusId::PaneBChoose,
    // Row 2: pane A and pane B action buttons.
    FocusId::OpenSourceA,
    FocusId::PaneARescan,
    FocusId::OpenTargetB,
    FocusId::PaneBRescan,
    // Left panel body.
    FocusId::ExcludeTypes,
    FocusId::ExcludeDirs,
    FocusId::ApplyRescan,
    FocusId::DirectoryTree,
    // Treemaps and the splitter between them.
    FocusId::TreemapA,
    FocusId::Splitter,
    FocusId::TreemapB,
];

/// Toolbar / pane labels with `&` access keys. Kept here so tests can check collisions.
pub const MNEMONIC_LABELS: &[(&str, FocusId)] = &[
    ("Ru&n sync", FocusId::RunSync),
    ("So&urce", FocusId::TreeTabA),
    ("T&arget", FocusId::TreeTabB),
    ("Files to &include", FocusId::ExcludeTypes),
    ("Files to e&xclude", FocusId::ExcludeDirs),
    ("Appl&y && rescan", FocusId::ApplyRescan),
    ("&Hierarchy", FocusId::DirectoryTree),
    ("Sour&ce…", FocusId::PaneAChoose),
    ("&Open source (A)…", FocusId::OpenSourceA),
    ("&Rescan", FocusId::PaneARescan),
    ("T&reemap A", FocusId::TreemapA),
    ("S&plitter", FocusId::Splitter),
    ("Tar&get…", FocusId::PaneBChoose),
    ("Open &target (B)…", FocusId::OpenTargetB),
    ("&Rescan", FocusId::PaneBRescan),
    ("Treemap (&view)", FocusId::TreemapB),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub enum KeyAction {
    #[default]
    None,
    OpenFolder(crate::app::ActivePane),
    Rescan(crate::app::ActivePane),
    SetActivePane(crate::app::ActivePane),
    FocusTreeTab(crate::app::ActivePane),
    TreemapParent(crate::app::ActivePane),
    TreemapChild(crate::app::ActivePane),
    RefreshCompare,
    StartSync,
    ApplyExcludes,
    CancelScan(crate::app::ActivePane),
    CancelSync,
    ActivateFocused,
    ToggleHelp,
}

#[derive(Clone, Debug)]
pub struct KeyboardState {
    pub focused: FocusId,
    pub help_open: bool,
    pub pending: KeyAction,
}

impl Default for KeyboardState {
    fn default() -> Self {
        Self {
            focused: FocusId::TreeTabA,
            help_open: false,
            pending: KeyAction::None,
        }
    }
}

/// Pattern fields that need egui keyboard focus for typing.
pub fn is_text_focus(slot: FocusId) -> bool {
    matches!(slot, FocusId::ExcludeTypes | FocusId::ExcludeDirs)
}

#[allow(deprecated)]
pub fn focus_egui_id(slot: FocusId) -> egui::Id {
    egui::Id::new("rsyncstat_focus").with(u32::from(slot as u8))
}

/// Tab targets currently on screen.
/// `has_directory_tree` is false when the active pane has no scan tree yet.
pub fn visible_tab_order(has_directory_tree: bool) -> Vec<FocusId> {
    TAB_ORDER
        .iter()
        .copied()
        .filter(|id| has_directory_tree || !matches!(id, FocusId::DirectoryTree))
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
                underline_index = Some(display.chars().count());
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
    for run in label_runs(&parsed.display, parsed.underline_index, show_underline) {
        if run.underline {
            job.append(
                &run.text,
                0.0,
                TextFormat {
                    underline: underline_stroke,
                    ..base.clone()
                },
            );
        } else {
            job.append(&run.text, 0.0, base.clone());
        }
    }
    job
}

struct LabelRun {
    text: String,
    underline: bool,
}

/// Split a label into plain and underlined pieces.
///
/// When the underline is hidden, the access-key character stays in the plain
/// run. Stopping on that index without consuming it loops forever on an empty
/// chunk, which is what kept the first menu button from finishing.
fn label_runs(
    display: &str,
    underline_index: Option<usize>,
    show_underline: bool,
) -> Vec<LabelRun> {
    let chars: Vec<char> = display.chars().collect();
    let mut runs = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if show_underline && underline_index == Some(i) {
            runs.push(LabelRun {
                text: chars[i].to_string(),
                underline: true,
            });
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && !(show_underline && underline_index == Some(i)) {
            i += 1;
        }
        if start < i {
            runs.push(LabelRun {
                text: chars[start..i].iter().collect(),
                underline: false,
            });
        }
    }
    runs
}

/// Clearance outside a widget for the focus ring (stroke + corner radius).
pub const FOCUS_RING_OUTSET: f32 = 4.0;

/// Extra layout padding so rings are not clipped by panel edges or neighbors.
pub const FOCUS_PAD: f32 = 5.0;

pub fn paint_focus_ring(ui: &Ui, rect: Rect, focused: bool) {
    if !focused {
        return;
    }
    let stroke = Stroke::new(2.0, ui.visuals().selection.stroke.color.gamma_multiply(1.1));
    // Rings are drawn outside the widget; expand the clip so panel edges do not shave them.
    let painter = ui
        .painter()
        .with_clip_rect(ui.clip_rect().expand(FOCUS_RING_OUTSET + FOCUS_PAD));
    painter.rect_stroke(
        rect.expand(FOCUS_RING_OUTSET / 2.0),
        4.0,
        stroke,
        StrokeKind::Outside,
    );
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

pub fn mnemonic_selectable(
    ui: &mut Ui,
    slot: FocusId,
    raw_label: &str,
    selected: bool,
    show_underline: bool,
) -> Response {
    let id = focus_egui_id(slot);
    let parsed = parse_access_label(raw_label);
    let job = layout_job_for_label(ui, &parsed, show_underline, egui::TextStyle::Button);
    let size = Vec2::new(ui.available_width(), ui.spacing().interact_size.y);
    // A framed toggle: the idle tab keeps the normal button fill so it reads
    // like its neighbours; the active tab uses the theme's selection colors.
    ui.push_id(id, |ui| {
        ui.add(Button::new(job).selected(selected).min_size(size))
    })
    .inner
}

/// Focus target over a rectangle that another widget will paint.
/// Does not advance the layout cursor.
pub fn focus_region_at(ui: &mut Ui, slot: FocusId, rect: Rect, sense: Sense) -> Response {
    ui.interact(rect, focus_egui_id(slot), sense)
}

pub fn raw_label_for(slot: FocusId) -> &'static str {
    MNEMONIC_LABELS
        .iter()
        .find_map(|(label, id)| (*id == slot).then_some(*label))
        .expect("every FocusId has a MNEMONIC_LABELS entry")
}

pub fn focus_for_mnemonic(letter: char, has_directory_tree: bool) -> Option<FocusId> {
    let letter = letter.to_ascii_lowercase();
    let visible: HashSet<FocusId> = visible_tab_order(has_directory_tree).into_iter().collect();
    MNEMONIC_LABELS
        .iter()
        .filter(|(_, id)| visible.contains(id))
        .find_map(|(label, id)| {
            let parsed = parse_access_label(label);
            (parsed.access_key == Some(letter)).then_some(*id)
        })
}

/// All access keys that must be unique within a view (per-pane duplicates excluded).
#[cfg(test)]
pub fn toolbar_mnemonic_keys() -> Vec<char> {
    let mut keys = Vec::new();
    for (label, id) in MNEMONIC_LABELS {
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
    has_directory_tree: bool,
    active_pane: crate::app::ActivePane,
    source_scanning: bool,
    target_scanning: bool,
    syncing: bool,
) {
    state.pending = KeyAction::None;
    let order = visible_tab_order(has_directory_tree);

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
                state.pending = KeyAction::CancelScan(crate::app::ActivePane::Source);
            } else if target_scanning {
                state.pending = KeyAction::CancelScan(crate::app::ActivePane::Target);
            }
            input.consume_key(Modifiers::NONE, Key::Escape);
        }

        if input.key_pressed(Key::Tab) {
            // Drop the directory-tree slot when that pane has no scan yet.
            if !order.contains(&state.focused) {
                state.focused = order.first().copied().unwrap_or(FocusId::TreeTabA);
            }
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

        if input.modifiers.alt && !input.modifiers.ctrl && !input.modifiers.mac_cmd {
            for event in &input.events {
                if let Event::Text(text) = event {
                    if let Some(ch) = text.chars().next() {
                        if ch.is_ascii_alphabetic() {
                            if let Some(slot) = focus_for_mnemonic(ch, has_directory_tree) {
                                state.focused = slot;
                                state.pending = KeyAction::ActivateFocused;
                            }
                        }
                    }
                }
            }
        }

        let ctrl = input.modifiers.ctrl || input.modifiers.mac_cmd;
        if ctrl && input.key_pressed(Key::Num1) {
            state.focused = FocusId::TreemapA;
            state.pending = KeyAction::SetActivePane(crate::app::ActivePane::Source);
        } else if ctrl && input.key_pressed(Key::Num2) {
            state.focused = FocusId::TreemapB;
            state.pending = KeyAction::SetActivePane(crate::app::ActivePane::Target);
        } else if ctrl && input.key_pressed(Key::O) {
            state.pending = KeyAction::OpenFolder(active_pane);
        } else if ctrl && input.key_pressed(Key::R) || input.key_pressed(Key::F5) {
            state.pending = KeyAction::Rescan(active_pane);
        } else if ctrl && input.modifiers.shift && input.key_pressed(Key::C) {
            state.pending = KeyAction::RefreshCompare;
        }

        let treemap_pane = if state.focused == FocusId::TreemapA {
            Some(crate::app::ActivePane::Source)
        } else if state.focused == FocusId::TreemapB {
            Some(crate::app::ActivePane::Target)
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

        if !is_text_focus(state.focused)
            && (input.key_pressed(Key::Enter) || input.key_pressed(Key::Space))
        {
            state.pending = KeyAction::ActivateFocused;
        }
    });

    // egui's begin_pass already queued Tab/arrow focus moves from the same
    // key events. Clear that so only our FocusId ring drives Tab order.
    ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
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
    let Some(node) = root.find_by_path(&path) else {
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
    fn hidden_underline_stays_one_run() {
        let runs = label_runs("Open source", Some(5), false);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "Open source");
        assert!(!runs[0].underline);
    }

    #[test]
    fn shown_underline_splits_around_the_access_key() {
        let runs = label_runs("Open source", Some(5), true);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].text, "Open ");
        assert!(!runs[0].underline);
        assert_eq!(runs[1].text, "s");
        assert!(runs[1].underline);
        assert_eq!(runs[2].text, "ource");
        assert!(!runs[2].underline);
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
    fn toolbar_mnemonics_are_unique() {
        let keys = toolbar_mnemonic_keys();
        let mut seen = HashSet::new();
        for k in keys {
            assert!(seen.insert(k), "duplicate mnemonic '{k}'");
        }
    }

    #[test]
    fn focus_order_runs_row_by_row_across_the_window() {
        let dual = visible_tab_order(true);
        let pos = |id: FocusId| dual.iter().position(|&x| x == id).unwrap();
        // Row 1 crosses every column before anything below it.
        let row1 = [
            FocusId::TreeTabA,
            FocusId::TreeTabB,
            FocusId::RunSync,
            FocusId::PaneAChoose,
            FocusId::PaneBChoose,
        ];
        let row2 = [
            FocusId::OpenSourceA,
            FocusId::PaneARescan,
            FocusId::OpenTargetB,
            FocusId::PaneBRescan,
        ];
        for pair in row1.windows(2).chain(row2.windows(2)) {
            assert!(
                pos(pair[0]) < pos(pair[1]),
                "{:?} before {:?}",
                pair[0],
                pair[1]
            );
        }
        assert!(pos(FocusId::PaneBChoose) < pos(FocusId::OpenSourceA));
        // Run sync goes right to the panes, not down into the filters.
        assert_eq!(pos(FocusId::RunSync) + 1, pos(FocusId::PaneAChoose));
        assert!(pos(FocusId::PaneBRescan) < pos(FocusId::ExcludeTypes));
        assert!(pos(FocusId::ExcludeTypes) < pos(FocusId::DirectoryTree));
        assert!(pos(FocusId::DirectoryTree) < pos(FocusId::TreemapA));
        assert!(pos(FocusId::TreemapA) < pos(FocusId::Splitter));
        assert!(pos(FocusId::Splitter) < pos(FocusId::TreemapB));
    }

    #[test]
    fn tab_order_starts_with_source_target_run_sync_and_follows_visual_layout() {
        assert_eq!(TAB_ORDER[0], FocusId::TreeTabA);
        assert_eq!(TAB_ORDER[1], FocusId::TreeTabB);
        assert_eq!(TAB_ORDER[2], FocusId::RunSync);
        assert!(TAB_ORDER.contains(&FocusId::TreemapA));
        assert!(TAB_ORDER.contains(&FocusId::TreemapB));
        let dual = visible_tab_order(true);
        assert!(dual.contains(&FocusId::TreeTabB));
        assert!(dual.contains(&FocusId::OpenTargetB));
        // Each pane still reads Choose → Open → Rescan, with its treemap after
        // the button rows.
        let pos = |id: FocusId| dual.iter().position(|&x| x == id).unwrap();
        assert!(pos(FocusId::PaneAChoose) < pos(FocusId::OpenSourceA));
        assert_eq!(pos(FocusId::OpenSourceA) + 1, pos(FocusId::PaneARescan));
        assert!(pos(FocusId::PaneARescan) < pos(FocusId::TreemapA));
        assert_eq!(pos(FocusId::OpenTargetB) + 1, pos(FocusId::PaneBRescan));
    }

    #[test]
    fn directory_tree_omitted_from_tab_order_until_scan_exists() {
        let empty = visible_tab_order(false);
        assert!(!empty.contains(&FocusId::DirectoryTree));
        assert!(empty.contains(&FocusId::TreeTabB));
        let with_tree = visible_tab_order(true);
        assert!(with_tree.contains(&FocusId::DirectoryTree));
        let include = with_tree
            .iter()
            .position(|&id| id == FocusId::ExcludeTypes)
            .unwrap();
        let apply = with_tree
            .iter()
            .position(|&id| id == FocusId::ApplyRescan)
            .unwrap();
        let tree = with_tree
            .iter()
            .position(|&id| id == FocusId::DirectoryTree)
            .unwrap();
        assert!(include < apply);
        assert!(apply < tree);
    }

    #[test]
    fn focus_ids_align_with_mnemonic_table() {
        for &id in TAB_ORDER {
            let in_table = MNEMONIC_LABELS.iter().any(|(_, slot)| *slot == id);
            assert!(in_table, "FocusId {:?} missing from MNEMONIC_LABELS", id);
        }
    }
}
