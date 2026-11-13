//! Include and exclude pattern editor.

use super::*;

pub enum PatternIcon {
    Book,
    Gear { slashed: bool },
}

pub struct PatternEditResult {
    pub submitted: bool,
    pub icon_clicked: bool,
    pub toggled: Option<PatternToggle>,
    pub rect: Rect,
    /// True when the text field took egui keyboard focus this frame (click or Tab).
    pub text_gained_focus: bool,
}

/// Colors for the pattern fields, picked to match the active theme.
struct FieldColors {
    label: Color32,
    fill: Color32,
    border: Color32,
    divider: Color32,
    toggle_on: Color32,
    toggle_hover: Color32,
    icon: Color32,
    icon_hover: Color32,
    toggle_text: Color32,
    toggle_text_active: Color32,
}

impl FieldColors {
    fn for_visuals(visuals: &egui::Visuals) -> Self {
        let g = Color32::from_gray;
        if visuals.dark_mode {
            Self {
                label: g(204),
                fill: g(60),
                border: g(78),
                divider: g(90),
                toggle_on: g(92),
                toggle_hover: g(74),
                icon: g(176),
                icon_hover: g(235),
                toggle_text: g(150),
                toggle_text_active: g(235),
            }
        } else {
            Self {
                label: g(70),
                fill: g(255),
                border: g(190),
                divider: g(205),
                toggle_on: g(214),
                toggle_hover: g(232),
                icon: g(100),
                icon_hover: g(20),
                toggle_text: g(110),
                toggle_text_active: g(20),
            }
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
    icon: PatternIcon,
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
    };
    ui.horizontal(|ui| {
        ui.add_space(inset);
        let width = (ui.available_width() - inset).max(32.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::hover());
        ui.painter().rect_filled(rect, 4.0, colors.fill);
        ui.painter().rect_stroke(
            rect,
            4.0,
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
                .frame(egui::Frame::NONE)
                .margin(egui::Margin::symmetric(8, 4))
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

        let clickable = matches!(icon, PatternIcon::Gear { .. });
        let icon_response = ui.interact(
            icon_rect,
            ui.scope_id().with((id, "icon")),
            if clickable {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        if icon_response.hovered() {
            icon_response.clone().on_hover_text(icon_tip);
            if clickable {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
        let color = if icon_response.hovered() {
            colors.icon_hover
        } else {
            colors.icon
        };
        match icon {
            PatternIcon::Book => paint_book(ui.painter(), icon_rect, color),
            PatternIcon::Gear { slashed } => paint_gear(ui.painter(), icon_rect, color, slashed),
        }
        result = PatternEditResult {
            submitted,
            icon_clicked: icon_response.clicked(),
            toggled,
            rect,
            text_gained_focus,
        };
    });
    result
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

pub(super) fn paint_book(painter: &egui::Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let stroke = Stroke::new(1.2, color);
    let top = c.y - 5.5;
    let bottom = c.y + 5.0;
    let left = c.x - 7.0;
    let right = c.x + 7.0;
    painter.line_segment([Pos2::new(c.x, top + 1.0), Pos2::new(c.x, bottom)], stroke);
    painter.line_segment(
        [Pos2::new(c.x, top + 1.0), Pos2::new(left, top + 3.0)],
        stroke,
    );
    painter.line_segment(
        [Pos2::new(left, top + 3.0), Pos2::new(left, bottom)],
        stroke,
    );
    painter.line_segment([Pos2::new(left, bottom), Pos2::new(c.x, bottom)], stroke);
    painter.line_segment(
        [Pos2::new(c.x, top + 1.0), Pos2::new(right, top + 3.0)],
        stroke,
    );
    painter.line_segment(
        [Pos2::new(right, top + 3.0), Pos2::new(right, bottom)],
        stroke,
    );
    painter.line_segment([Pos2::new(right, bottom), Pos2::new(c.x, bottom)], stroke);
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
