//! Colours, spacing and widget styling shared by every panel.

use eframe::egui::{
    self, Color32, CornerRadius, FontId, Stroke, TextStyle, Theme, ThemePreference, Visuals,
    style::HandleShape,
};

pub(crate) const BACKGROUND: Color32 = Color32::from_rgb(12, 13, 16);
pub(crate) const CARD: Color32 = Color32::from_rgb(21, 23, 27);
pub(crate) const CARD_BORDER: Color32 = Color32::from_rgb(35, 38, 44);
pub(crate) const CONTROL: Color32 = Color32::from_rgb(33, 36, 42);
pub(crate) const CONTROL_HOVER: Color32 = Color32::from_rgb(44, 48, 56);
/// The chosen option in a segmented switch: raised, but not the accent.
pub(crate) const CONTROL_SELECTED: Color32 = Color32::from_rgb(70, 76, 88);
pub(crate) const TEXT: Color32 = Color32::from_rgb(228, 231, 235);
pub(crate) const TEXT_DIM: Color32 = Color32::from_rgb(134, 141, 151);
/// HyperX red, used for the primary action and every selected state.
pub(crate) const ACCENT: Color32 = Color32::from_rgb(226, 30, 60);
pub(crate) const ACCENT_HOVER: Color32 = Color32::from_rgb(242, 58, 86);
pub(crate) const GOOD: Color32 = Color32::from_rgb(88, 214, 141);
pub(crate) const INFO: Color32 = Color32::from_rgb(120, 180, 240);
pub(crate) const WARNING: Color32 = Color32::from_rgb(255, 190, 90);
pub(crate) const ERROR: Color32 = Color32::from_rgb(255, 104, 104);

pub(crate) const CARD_RADIUS: u8 = 10;
pub(crate) const CONTROL_RADIUS: u8 = 6;

/// Install the theme. The app is dark-only, so the theme is pinned rather than following
/// the system appearance.
pub(crate) fn apply(ctx: &egui::Context) {
    ctx.set_theme(ThemePreference::Dark);
    ctx.set_visuals_of(Theme::Dark, visuals());
    ctx.all_styles_mut(|style| {
        let spacing = &mut style.spacing;
        spacing.item_spacing = egui::vec2(8.0, 4.0);
        spacing.button_padding = egui::vec2(10.0, 5.0);
        spacing.slider_rail_height = 4.0;
        spacing.interact_size.y = 22.0;

        style.text_styles = [
            (TextStyle::Heading, FontId::proportional(19.0)),
            (TextStyle::Body, FontId::proportional(13.5)),
            (TextStyle::Button, FontId::proportional(13.5)),
            (TextStyle::Small, FontId::proportional(11.0)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();
    });
}

fn visuals() -> Visuals {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = CARD;
    visuals.window_stroke = Stroke::new(1.0, CARD_BORDER);
    visuals.window_corner_radius = CornerRadius::same(CARD_RADIUS);
    visuals.menu_corner_radius = CornerRadius::same(CARD_RADIUS);
    visuals.extreme_bg_color = Color32::from_rgb(16, 17, 21);
    visuals.faint_bg_color = CARD;
    visuals.hyperlink_color = INFO;
    visuals.warn_fg_color = WARNING;
    visuals.error_fg_color = ERROR;
    visuals.slider_trailing_fill = false;
    visuals.handle_shape = HandleShape::Circle;

    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);

    let control = CornerRadius::same(CONTROL_RADIUS);
    let widgets = &mut visuals.widgets;

    widgets.noninteractive.bg_fill = CARD;
    widgets.noninteractive.weak_bg_fill = CARD;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, CARD_BORDER);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.noninteractive.corner_radius = control;

    widgets.inactive.bg_fill = CONTROL;
    widgets.inactive.weak_bg_fill = CONTROL;
    widgets.inactive.bg_stroke = Stroke::NONE;
    widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.inactive.corner_radius = control;

    widgets.hovered.bg_fill = CONTROL_HOVER;
    widgets.hovered.weak_bg_fill = CONTROL_HOVER;
    widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(64, 69, 78));
    widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    widgets.hovered.corner_radius = control;

    widgets.active.bg_fill = ACCENT_HOVER;
    widgets.active.weak_bg_fill = Color32::from_rgb(54, 59, 68);
    widgets.active.bg_stroke = Stroke::new(1.0, ACCENT_HOVER);
    widgets.active.fg_stroke = Stroke::new(2.0, Color32::WHITE);
    widgets.active.corner_radius = control;

    widgets.open.bg_fill = CONTROL_HOVER;
    widgets.open.weak_bg_fill = CONTROL_HOVER;
    widgets.open.bg_stroke = Stroke::new(1.0, CARD_BORDER);
    widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.open.corner_radius = control;

    visuals
}
