use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, RichText, Sense, Stroke, vec2,
};

use crate::gui::theme;
use crate::lighting::pattern_description;
use crate::model::PolarPattern;

/// Number of blocks in the input level meter.
const METER_SEGMENTS: usize = 24;

/// Small, dim, upper-case heading for a group of controls.
pub(crate) fn section_label(ui: &mut egui::Ui, label: &str) {
    ui.label(
        RichText::new(label.to_uppercase())
            .size(11.0)
            .strong()
            .color(theme::TEXT_DIM),
    );
}

/// The rounded panel every card is drawn in.
pub(crate) fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::CARD)
        .stroke(Stroke::new(1.0, theme::CARD_BORDER))
        .corner_radius(CornerRadius::same(theme::CARD_RADIUS))
        .inner_margin(egui::Margin::same(14))
}

/// A 0-100 slider spanning the available width, with its label and value above it.
pub(crate) fn value_slider(ui: &mut egui::Ui, label: &str, value: &mut u8) -> egui::Response {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        value_row(ui, label, &format!("{value}"));
        ui.spacing_mut().slider_width = ui.available_width();
        ui.add(egui::Slider::new(value, 0..=100).show_value(false))
    })
    .inner
}

/// A label on the left and a dim value on the right.
pub(crate) fn value_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(theme::TEXT));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).color(theme::TEXT_DIM));
        });
    });
}

/// A segmented level meter: green, then amber, then red towards the top.
pub(crate) fn level_meter(ui: &mut egui::Ui, level: f32) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 10.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let gap = 2.0;
    let segment_width = (width - gap * (METER_SEGMENTS - 1) as f32) / METER_SEGMENTS as f32;
    let lit = (level.clamp(0.0, 1.0) * METER_SEGMENTS as f32).ceil() as usize;
    for index in 0..METER_SEGMENTS {
        let left = rect.left() + index as f32 * (segment_width + gap);
        let segment = Rect::from_min_size(
            egui::pos2(left, rect.top()),
            vec2(segment_width, rect.height()),
        );
        let position = index as f32 / (METER_SEGMENTS - 1) as f32;
        let color = if index < lit {
            meter_color(position)
        } else {
            theme::CONTROL
        };
        painter.rect_filled(segment, CornerRadius::same(2), color);
    }
    response
}

fn meter_color(position: f32) -> Color32 {
    if position < 0.65 {
        theme::GOOD
    } else if position < 0.85 {
        theme::WARNING
    } else {
        theme::ERROR
    }
}

/// A row of mutually exclusive options. Returns whether the selection changed.
pub(crate) fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    id_salt: &str,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let height = 28.0;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        CornerRadius::same(theme::CONTROL_RADIUS),
        theme::CONTROL,
    );

    let segment_width = width / options.len() as f32;
    let mut changed = false;
    for (index, (option, label)) in options.iter().enumerate() {
        let segment = Rect::from_min_size(
            rect.min + vec2(segment_width * index as f32, 0.0),
            vec2(segment_width, height),
        )
        .shrink(2.0);
        let response = ui
            .interact(segment, ui.id().with((id_salt, index)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let selected = *value == *option;
        if selected {
            painter.rect_filled(
                segment,
                CornerRadius::same(theme::CONTROL_RADIUS - 1),
                theme::CONTROL_SELECTED,
            );
        } else if response.hovered() {
            painter.rect_filled(
                segment,
                CornerRadius::same(theme::CONTROL_RADIUS - 1),
                theme::CONTROL_HOVER,
            );
        }
        painter.text(
            segment.center(),
            Align2::CENTER_CENTER,
            *label,
            FontId::proportional(13.0),
            if selected {
                Color32::WHITE
            } else {
                theme::TEXT
            },
        );
        if response.clicked() && !selected {
            *value = *option;
            changed = true;
        }
    }
    changed
}

/// A rounded status badge with a coloured dot.
pub(crate) fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    let font = FontId::proportional(12.5);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font, theme::TEXT);
    let size = vec2(galley.size().x + 30.0, 24.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(12), color.gamma_multiply(0.16));
    painter.rect_stroke(
        rect,
        CornerRadius::same(12),
        Stroke::new(1.0, color.gamma_multiply(0.45)),
        egui::StrokeKind::Inside,
    );
    painter.circle_filled(rect.left_center() + vec2(12.0, 0.0), 3.5, color);
    painter.galley(
        egui::pos2(rect.left() + 21.0, rect.center().y - galley.size().y / 2.0),
        galley,
        theme::TEXT,
    );
}

/// The one emphasised action in a group.
pub(crate) fn primary_button(ui: &mut egui::Ui, text: &str, width: f32) -> egui::Response {
    ui.add_sized(
        [width, 30.0],
        egui::Button::new(RichText::new(text).strong().color(Color32::WHITE)).fill(theme::ACCENT),
    )
}

pub(crate) fn secondary_button(ui: &mut egui::Ui, text: &str, width: f32) -> egui::Response {
    ui.add_sized([width, 28.0], egui::Button::new(text))
}

pub(crate) fn color_swatch(ui: &mut egui::Ui, color: Color32, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let center = rect.center();
    let painter = ui.painter();
    painter.circle_filled(center, 9.0, color);
    let ring = if selected {
        Stroke::new(2.0, Color32::WHITE)
    } else if response.hovered() {
        Stroke::new(1.0, theme::TEXT_DIM)
    } else {
        Stroke::new(1.0, Color32::from_white_alpha(28))
    };
    painter.circle_stroke(center, if selected { 11.5 } else { 9.5 }, ring);
    response
}

pub(crate) fn pattern_tile(ui: &mut egui::Ui, pattern: PolarPattern, current: PolarPattern) {
    let selected = current == pattern;
    let (rect, response) = ui.allocate_exact_size(vec2(76.0, 66.0), Sense::hover());
    let painter = ui.painter_at(rect.expand(1.0));
    let (fill, stroke) = if selected {
        (
            theme::ACCENT.gamma_multiply(0.18),
            Stroke::new(1.5, theme::ACCENT),
        )
    } else {
        (theme::CONTROL, Stroke::NONE)
    };
    painter.rect_filled(rect, CornerRadius::same(8), fill);
    painter.rect_stroke(
        rect,
        CornerRadius::same(8),
        stroke,
        egui::StrokeKind::Inside,
    );
    draw_pattern_icon(&painter, rect, pattern, selected);
    painter.text(
        rect.center_bottom() - vec2(0.0, 7.0),
        Align2::CENTER_BOTTOM,
        pattern.label(),
        FontId::proportional(11.0),
        if selected {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        },
    );
    if response.hovered() {
        response.on_hover_text(pattern_description(pattern));
    }
}

pub(crate) fn draw_pattern_icon(
    painter: &egui::Painter,
    rect: Rect,
    pattern: PolarPattern,
    selected: bool,
) {
    let center = rect.center_top() + vec2(0.0, 25.0);
    let active = if selected {
        Color32::WHITE
    } else {
        Color32::from_rgb(150, 156, 164)
    };
    let muted = Color32::from_rgb(78, 82, 90);
    let stroke = Stroke::new(1.8, active);
    match pattern {
        PolarPattern::Stereo => {
            painter.circle_stroke(center - vec2(8.0, 0.0), 10.0, stroke);
            painter.circle_stroke(center + vec2(8.0, 0.0), 10.0, stroke);
        }
        PolarPattern::Omni => {
            painter.circle_stroke(center, 13.0, stroke);
        }
        PolarPattern::Cardioid => {
            painter.circle_stroke(center + vec2(0.0, 1.0), 12.0, stroke);
            painter.circle_filled(center + vec2(0.0, 9.0), 5.0, muted);
        }
        PolarPattern::Bidirectional => {
            painter.circle_stroke(center - vec2(0.0, 7.5), 7.0, stroke);
            painter.circle_stroke(center + vec2(0.0, 7.5), 7.0, stroke);
            painter.circle_filled(center, 3.5, muted);
        }
        PolarPattern::Unknown(_) => {
            painter.text(
                center,
                Align2::CENTER_CENTER,
                "?",
                FontId::proportional(22.0),
                active,
            );
        }
    }
}

/// LED dots that are switched off.
const UNLIT_LED: Color32 = Color32::from_rgb(52, 46, 52);

/// A QuadCast S silhouette lit like the real mic, drawn centred in `bounds`: the upper half
/// of the grille shows the top zone and the lower half the bottom zone. `intensity` fades
/// the lights between off (0, how the hardware looks muted) and fully lit (1).
pub(crate) fn draw_microphone(
    painter: &egui::Painter,
    bounds: Rect,
    [top, bottom]: [Color32; 2],
    intensity: f32,
) {
    let intensity = intensity.clamp(0.0, 1.0);
    let center = bounds.center();
    let height = bounds.height();
    let body_width = height * 0.27;
    let body_height = height * 0.60;
    let body = Rect::from_center_size(
        center - vec2(0.0, height * 0.10),
        vec2(body_width, body_height),
    );
    let radius = CornerRadius::same((body_width / 2.0).min(255.0) as u8);
    let grille = Rect::from_min_max(
        body.left_top() + vec2(9.0, body_height * 0.12),
        egui::pos2(body.right() - 9.0, body.top() + body_height * 0.66),
    );
    let zone_split = grille.center().y;

    for (color, y) in [
        (top, (grille.top() + zone_split) / 2.0),
        (bottom, (zone_split + grille.bottom()) / 2.0),
    ] {
        let glow_center = egui::pos2(center.x, y);
        // Fade out before the edge of `bounds` rather than being cut off by it.
        let radius = (body_width * 1.9)
            .min(glow_center.x - bounds.left())
            .min(bounds.right() - glow_center.x)
            .min(glow_center.y - bounds.top())
            .min(bounds.bottom() - glow_center.y);
        draw_glow(painter, glow_center, radius, color, intensity);
    }

    painter.rect_filled(body, radius, Color32::from_rgb(24, 25, 29));
    painter.rect_stroke(
        body,
        radius,
        Stroke::new(1.0, Color32::from_rgb(56, 59, 66)),
        egui::StrokeKind::Inside,
    );

    let top_dot = led_dot(top, intensity);
    let bottom_dot = led_dot(bottom, intensity);
    let mut y = grille.top();
    let mut row = 0;
    while y <= grille.bottom() {
        let dot = if y < zone_split { top_dot } else { bottom_dot };
        let offset = if row % 2 == 0 { 0.0 } else { 3.5 };
        let mut x = grille.left() + offset;
        while x <= grille.right() {
            painter.circle_filled(egui::pos2(x, y), 1.8, dot);
            x += 7.0;
        }
        y += 6.0;
        row += 1;
    }

    // Shock mount band, stem and base.
    let band = Rect::from_center_size(
        egui::pos2(center.x, body.top() + body_height * 0.80),
        vec2(body_width * 1.30, 12.0),
    );
    painter.rect_filled(band, CornerRadius::same(4), Color32::from_rgb(14, 15, 18));
    painter.rect_stroke(
        band,
        CornerRadius::same(4),
        Stroke::new(1.0, Color32::from_rgb(48, 51, 58)),
        egui::StrokeKind::Inside,
    );
    let stem = Rect::from_min_size(
        egui::pos2(center.x - 5.0, body.bottom()),
        vec2(10.0, height * 0.20),
    );
    painter.rect_filled(stem, CornerRadius::same(2), Color32::from_rgb(30, 32, 36));
    let base = Rect::from_center_size(
        egui::pos2(center.x, stem.bottom() + 4.0),
        vec2(body_width * 1.9, 9.0),
    );
    painter.rect_filled(base, CornerRadius::same(5), Color32::from_rgb(30, 32, 36));
}

fn is_dark(color: Color32) -> bool {
    u16::from(color.r()) + u16::from(color.g()) + u16::from(color.b()) < 24
}

fn led_dot(color: Color32, intensity: f32) -> Color32 {
    if is_dark(color) {
        UNLIT_LED
    } else {
        UNLIT_LED.lerp_to_gamma(color, intensity)
    }
}

/// A soft halo from stacked translucent discs, densest at the centre.
fn draw_glow(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    color: Color32,
    intensity: f32,
) {
    if intensity <= 0.0 || is_dark(color) {
        return;
    }
    let steps = 14;
    for step in 0..steps {
        let t = step as f32 / steps as f32;
        painter.circle_filled(
            center,
            radius * (1.0 - 0.75 * t),
            color.gamma_multiply(0.02 * intensity),
        );
    }
}
