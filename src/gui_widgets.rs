use eframe::egui;

use crate::lighting::pattern_description;
use crate::model::{LightTarget, PolarPattern};
pub(crate) fn section_label(ui: &mut egui::Ui, label: &str) {
    ui.label(egui::RichText::new(label).color(egui::Color32::from_rgb(180, 184, 188)));
}

pub(crate) fn percent_slider(ui: &mut egui::Ui, value: &mut u8, width: f32) -> egui::Response {
    ui.horizontal(|ui| {
        let response = ui.add_sized(
            [width, 20.0],
            egui::Slider::new(value, 0..=100).show_value(false),
        );
        ui.add_sized([34.0, 20.0], egui::Label::new(format!("{}", *value)));
        response
    })
    .inner
}

pub(crate) fn target_button(
    ui: &mut egui::Ui,
    current: &mut LightTarget,
    target: LightTarget,
) -> bool {
    let response = ui.add_sized(
        [58.0, 30.0],
        egui::Button::new(target.label()).selected(*current == target),
    );
    if response.clicked() {
        *current = target;
        true
    } else {
        false
    }
}

pub(crate) fn color_swatch(
    ui: &mut egui::Ui,
    color: egui::Color32,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 34.0), egui::Sense::click());
    ui.painter().rect_filled(rect, 0.0, color);
    if selected {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            0.0,
            egui::Stroke::new(2.0, egui::Color32::WHITE),
            egui::StrokeKind::Outside,
        );
    }
    response
}

pub(crate) fn pattern_tile(ui: &mut egui::Ui, pattern: PolarPattern, current: PolarPattern) {
    let selected = current == pattern;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(74.0, 64.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let fill = if selected {
        egui::Color32::from_rgb(52, 70, 80)
    } else {
        egui::Color32::from_rgb(38, 39, 40)
    };
    painter.rect_filled(rect, 4.0, fill);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(
            if selected { 2.0 } else { 1.0 },
            if selected {
                egui::Color32::from_rgb(0, 162, 255)
            } else {
                egui::Color32::from_rgb(70, 72, 74)
            },
        ),
        egui::StrokeKind::Outside,
    );
    draw_pattern_icon(&painter, rect, pattern, selected);
    painter.text(
        rect.center_bottom() - egui::vec2(0.0, 8.0),
        egui::Align2::CENTER_BOTTOM,
        pattern.label(),
        egui::FontId::proportional(11.0),
        egui::Color32::from_rgb(220, 224, 228),
    );
    if response.hovered() {
        response.on_hover_text(pattern_description(pattern));
    }
}

pub(crate) fn draw_pattern_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    pattern: PolarPattern,
    selected: bool,
) {
    let center = rect.center_top() + egui::vec2(0.0, 24.0);
    let active = if selected {
        egui::Color32::from_rgb(235, 242, 246)
    } else {
        egui::Color32::from_rgb(155, 160, 164)
    };
    let muted = egui::Color32::from_rgb(72, 75, 78);
    let stroke = egui::Stroke::new(2.0, active);
    match pattern {
        PolarPattern::Stereo => {
            painter.circle_stroke(center - egui::vec2(9.0, 0.0), 10.0, stroke);
            painter.circle_stroke(center + egui::vec2(9.0, 0.0), 10.0, stroke);
        }
        PolarPattern::Omni => {
            painter.circle_stroke(center, 14.0, stroke);
        }
        PolarPattern::Cardioid => {
            painter.circle_stroke(center + egui::vec2(0.0, 2.0), 12.0, stroke);
            painter.circle_filled(center + egui::vec2(0.0, 10.0), 6.0, muted);
        }
        PolarPattern::Bidirectional => {
            painter.circle_stroke(center - egui::vec2(0.0, 8.0), 7.0, stroke);
            painter.circle_stroke(center + egui::vec2(0.0, 8.0), 7.0, stroke);
            painter.circle_filled(center, 4.0, muted);
        }
        PolarPattern::Unknown(_) => {
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                "?",
                egui::FontId::proportional(24.0),
                active,
            );
        }
    }
}

pub(crate) fn draw_microphone(painter: &egui::Painter, center: egui::Pos2, stage_height: f32) {
    let body_width = stage_height * 0.18;
    let body_height = stage_height * 0.58;
    let top = center.y - body_height * 0.42;
    let left = center.x - body_width / 2.0;
    let body =
        egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(body_width, body_height));

    painter.rect_filled(body, 18.0, egui::Color32::from_rgb(18, 18, 18));
    painter.rect_stroke(
        body,
        18.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 50, 50)),
        egui::StrokeKind::Outside,
    );

    let grille = egui::Rect::from_min_max(
        body.left_top() + egui::vec2(8.0, 42.0),
        body.right_bottom() - egui::vec2(8.0, body_height * 0.34),
    );
    let dot_color = egui::Color32::from_rgb(86, 30, 54);
    let mut y = grille.top();
    while y < grille.bottom() {
        let mut x = grille.left();
        while x < grille.right() {
            painter.circle_filled(egui::pos2(x, y), 2.4, dot_color);
            x += 8.0;
        }
        y += 7.0;
    }

    let mount_y = body.bottom() - body_height * 0.22;
    painter.rect_filled(
        egui::Rect::from_center_size(
            egui::pos2(center.x, mount_y),
            egui::vec2(body_width * 1.35, 16.0),
        ),
        3.0,
        egui::Color32::from_rgb(9, 9, 9),
    );
    painter.rect_filled(
        egui::Rect::from_center_size(
            egui::pos2(center.x, body.bottom() + 36.0),
            egui::vec2(18.0, 80.0),
        ),
        3.0,
        egui::Color32::from_rgb(12, 12, 12),
    );
    painter.rect_filled(
        egui::Rect::from_center_size(
            egui::pos2(center.x, body.bottom() + 84.0),
            egui::vec2(body_width * 1.6, 14.0),
        ),
        7.0,
        egui::Color32::from_rgb(18, 18, 18),
    );
}
