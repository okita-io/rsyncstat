//! Include and exclude pattern editor.

use super::*;

pub enum PatternIcon<'a> {
    Gear { slashed: bool },
    Presets { items: &'a [NamedPreset] },
}

pub enum PresetMenuAction {
    None,
    Apply(String),
    SaveNew,
}

pub struct PatternEditResult {
    pub submitted: bool,
    pub icon_clicked: bool,
    pub toggled: Option<PatternToggle>,
    pub rect: Rect,
    /// True when the text field took egui keyboard focus this frame (click or Tab).
    pub text_gained_focus: bool,
    pub preset_action: PresetMenuAction,
}

/// Height shared by every button, text field and checkbox row.
pub const CONTROL_HEIGHT: f32 = 30.0;
/// Corner radius shared by buttons and text fields when Hyprland rounding
/// is unavailable. On Hyprland this is replaced by `decoration:rounding`.
pub const CONTROL_RADIUS: u8 = 4;

/// Radius of buttons and custom fields for the active visuals.
pub fn chrome_radius(visuals: &egui::Visuals) -> u8 {
    visuals.widgets.inactive.corner_radius.nw
}
/// Inner padding for custom-painted text fields. Left is larger so glyphs
/// do not sit on the stroke. Used on the frame because `.margin()` is ignored
/// when `Frame::NONE` is set.
pub const FIELD_TEXT_MARGIN: egui::Margin = egui::Margin {
    left: 12,
    right: 8,
    top: 4,
    bottom: 4,
};

pub fn field_text_frame() -> egui::Frame {
    egui::Frame::NONE.inner_margin(FIELD_TEXT_MARGIN)
}

/// Colors for text fields, picked to match the active theme.
pub struct FieldColors {
    pub label: Color32,
    pub fill: Color32,
    pub border: Color32,
    pub divider: Color32,
    pub toggle_on: Color32,
    pub toggle_hover: Color32,
    pub icon: Color32,
    pub icon_hover: Color32,
    pub toggle_text: Color32,
    pub toggle_text_active: Color32,
}

impl FieldColors {
    /// Pulls field chrome from the active visuals, so an Omarchy theme change
    /// recolors the pattern editors on the next frame.
    pub fn for_visuals(visuals: &egui::Visuals) -> Self {
        let text = visuals.text_color();
        let weak = visuals.weak_text_color();
        Self {
            label: text,
            fill: visuals
                .text_edit_bg_color
                .unwrap_or(visuals.extreme_bg_color),
            border: visuals.widgets.noninteractive.bg_stroke.color,
            divider: visuals.widgets.noninteractive.bg_stroke.color,
            toggle_on: visuals.selection.bg_fill,
            toggle_hover: visuals.widgets.hovered.bg_fill,
            icon: weak,
            icon_hover: text,
            toggle_text: weak,
            toggle_text_active: text,
        }
    }
}

/// One pattern row: a label, a full-width field, match toggles, and an icon
/// sitting inside the field's right edge.
pub fn pattern_edit(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    hint: &str,
    text: &mut String,
    options: &PatternOptions,
    icon: PatternIcon<'_>,
    icon_tip: &str,
    want_keyboard_focus: bool,
) -> PatternEditResult {
    let colors = FieldColors::for_visuals(ui.visuals());
    ui.label(egui::RichText::new(label).color(colors.label));
    ui.add_space(4.0);
    // Inset the field so the focus ring clears the panel edge.
    let inset = 4.0;
    let mut result = PatternEditResult {
        submitted: false,
        icon_clicked: false,
        toggled: None,
        rect: Rect::NOTHING,
        text_gained_focus: false,
        preset_action: PresetMenuAction::None,
    };
    ui.horizontal(|ui| {
        ui.add_space(inset);
        let width = (ui.available_width() - inset).max(32.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, CONTROL_HEIGHT), Sense::hover());
        let radius = f32::from(chrome_radius(ui.visuals()));
        ui.painter().rect_filled(rect, radius, colors.fill);
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, colors.border),
            StrokeKind::Inside,
        );

        let icon_w = 26.0;
        let toggle_w = 24.0;
        let toggles_w = toggle_w * 3.0;
        let icon_rect = Rect::from_min_max(
            Pos2::new(rect.right() - icon_w, rect.top()),
            rect.right_bottom(),
        );
        let toggles_rect = Rect::from_min_max(
            Pos2::new(icon_rect.left() - toggles_w, rect.top()),
            Pos2::new(icon_rect.left(), rect.bottom()),
        );
        let text_rect = Rect::from_min_max(
            rect.left_top(),
            Pos2::new(toggles_rect.left(), rect.bottom()),
        );
        let text_response = ui.put(
            text_rect,
            egui::TextEdit::singleline(text)
                .id_source(id)
                .hint_text(egui::RichText::new(hint).color(colors.toggle_text))
                .text_color(colors.icon_hover)
                .frame(field_text_frame())
                .align(egui::Align2::LEFT_CENTER)
                .desired_width(text_rect.width()),
        );
        if want_keyboard_focus {
            if !text_response.has_focus() {
                text_response.request_focus();
            }
        } else if text_response.has_focus() {
            text_response.surrender_focus();
        }
        let submitted =
            text_response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        let text_gained_focus = text_response.gained_focus() || text_response.clicked();

        let toggled = paint_pattern_toggles(ui, id, toggles_rect, options);
        ui.painter().vline(
            icon_rect.left(),
            icon_rect.y_range().shrink(6.0),
            Stroke::new(1.0, colors.divider),
        );

        let icon_response =
            ui.interact(icon_rect, ui.scope_id().with((id, "icon")), Sense::click());
        if icon_response.hovered() {
            icon_response.clone().on_hover_text(icon_tip);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let color = if icon_response.hovered() {
            colors.icon_hover
        } else {
            colors.icon
        };
        let mut preset_action = PresetMenuAction::None;
        match icon {
            PatternIcon::Gear { slashed } => {
                paint_gear(ui.painter(), icon_rect, color, slashed);
            }
            PatternIcon::Presets { items } => {
                let popup = egui::Popup::menu(&icon_response);
                let open = popup.is_open() || icon_response.clicked();
                paint_chevron(ui.painter(), icon_rect, color, open);
                if let Some(inner) = popup.show(|ui| preset_menu(ui, items)) {
                    preset_action = inner.inner;
                }
            }
        }
        result = PatternEditResult {
            submitted,
            icon_clicked: icon_response.clicked(),
            toggled,
            rect,
            text_gained_focus,
            preset_action,
        };
    });
    result
}

fn preset_menu(ui: &mut egui::Ui, items: &[NamedPreset]) -> PresetMenuAction {
    ui.set_min_width(168.0);
    ui.spacing_mut().item_spacing.y = 2.0;
    let mut action = PresetMenuAction::None;
    for preset in items {
        let label = format!("+  {}", preset.name);
        if ui
            .add(egui::Button::new(label).fill(Color32::TRANSPARENT))
            .on_hover_text("Append this pack to the include and exclude fields")
            .clicked()
        {
            action = PresetMenuAction::Apply(preset.name.clone());
            ui.close();
        }
    }
    if !items.is_empty() {
        ui.separator();
    }
    if ui.button("Add to presets…").clicked() {
        action = PresetMenuAction::SaveNew;
        ui.close();
    }
    action
}

pub enum SavePresetOutcome {
    Stay,
    Cancel,
    Save,
}

/// Name a pack from the current include and exclude fields.
pub fn save_preset_dialog(
    ctx: &egui::Context,
    name: &mut String,
    error: &str,
    request_focus: bool,
) -> SavePresetOutcome {
    let response = egui::Modal::new(egui::Id::unique("save_preset")).show(ctx, |ui| {
        ui.set_min_width(320.0);
        ui.heading("Save preset");
        ui.label("Name this include/exclude pack. It is stored in include-exclude.yaml under your config directory.");
        ui.add_space(8.0);
        let edit = ui.add(
            egui::TextEdit::singleline(name)
                .hint_text("git, node, photos…")
                .desired_width(280.0),
        );
        if request_focus {
            edit.request_focus();
        }
        let submitted =
            edit.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if !error.is_empty() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(error)
                    .small()
                    .color(ui.visuals().warn_fg_color),
            );
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                ui.close();
                return Some(false);
            }
            let save = ui.add_enabled(!name.trim().is_empty(), egui::Button::new("Save"));
            if save.clicked() || submitted {
                return Some(true);
            }
            None
        })
        .inner
    });
    if response.inner == Some(true) {
        SavePresetOutcome::Save
    } else if response.inner == Some(false) || response.should_close() {
        SavePresetOutcome::Cancel
    } else {
        SavePresetOutcome::Stay
    }
}

pub(super) fn paint_pattern_toggles(
    ui: &mut egui::Ui,
    id: &str,
    rect: Rect,
    options: &PatternOptions,
) -> Option<PatternToggle> {
    let colors = FieldColors::for_visuals(ui.visuals());
    let buttons = [
        (
            PatternToggle::Case,
            "Aa",
            false,
            options.match_case,
            "Match Case",
        ),
        (
            PatternToggle::Word,
            "ab",
            true,
            options.whole_word,
            "Match Whole Word",
        ),
        (
            PatternToggle::Regex,
            ".*",
            false,
            options.regex,
            "Use Regular Expression",
        ),
    ];
    let mut toggled = None;
    let width = rect.width() / buttons.len() as f32;
    for (index, (kind, label, underline, on, tip)) in buttons.into_iter().enumerate() {
        let button = Rect::from_min_size(
            Pos2::new(rect.left() + width * index as f32, rect.top()),
            Vec2::new(width, rect.height()),
        );
        let response = ui.interact(button, ui.scope_id().with((id, label)), Sense::click());
        let fill = if on {
            colors.toggle_on
        } else if response.hovered() {
            colors.toggle_hover
        } else {
            Color32::TRANSPARENT
        };
        if fill != Color32::TRANSPARENT {
            ui.painter()
                .rect_filled(button.shrink2(Vec2::new(1.0, 4.0)), 3.0, fill);
        }
        let color = if on || response.hovered() {
            colors.toggle_text_active
        } else {
            colors.toggle_text
        };
        let text_rect = ui.painter().text(
            button.center(),
            egui::Align2::CENTER_CENTER,
            label,
            FontId::proportional(12.0),
            color,
        );
        if underline {
            ui.painter().hline(
                text_rect.x_range(),
                text_rect.bottom() + 1.0,
                Stroke::new(1.0, color),
            );
        }
        if response.hovered() {
            response.clone().on_hover_text(tip);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if response.clicked() {
            toggled = Some(kind);
        }
    }
    toggled
}

fn paint_chevron(painter: &egui::Painter, rect: Rect, color: Color32, open: bool) {
    let c = rect.center();
    let stroke = Stroke::new(1.3, color);
    let half = 4.0;
    let drop = 2.4;
    if open {
        painter.line_segment(
            [c + Vec2::new(-half, drop), c + Vec2::new(0.0, -drop)],
            stroke,
        );
        painter.line_segment(
            [c + Vec2::new(0.0, -drop), c + Vec2::new(half, drop)],
            stroke,
        );
    } else {
        painter.line_segment(
            [c + Vec2::new(-half, -drop), c + Vec2::new(0.0, drop)],
            stroke,
        );
        painter.line_segment(
            [c + Vec2::new(0.0, drop), c + Vec2::new(half, -drop)],
            stroke,
        );
    }
}

pub(super) fn paint_gear(painter: &egui::Painter, rect: Rect, color: Color32, slashed: bool) {
    let c = rect.center();
    let stroke = Stroke::new(1.15, color);
    painter.circle_stroke(c, 3.3, stroke);
    painter.circle_stroke(c, 1.35, stroke);
    for i in 0..8 {
        let angle = i as f32 * std::f32::consts::TAU / 8.0;
        let dir = Vec2::new(angle.cos(), angle.sin());
        painter.line_segment([c + dir * 3.1, c + dir * 5.5], stroke);
    }
    if slashed {
        painter.line_segment(
            [c + Vec2::new(-6.0, 6.0), c + Vec2::new(6.0, -6.0)],
            Stroke::new(1.5, color),
        );
    }
}
