use eframe::egui;

use crate::{
    gui_widgets::{draw_microphone, pattern_tile, section_label},
    model::PolarPattern,
};

use super::{MicLiteApp, NoticeSeverity, UiNotice, is_expected_hyperx_device_name, notice_color};

impl MicLiteApp {
    const STAGE_MIC_GAP: f32 = 18.0;

    // Cheap change detector so the notices (and their Strings) are only rebuilt
    // when the underlying device state changes, not on every repaint.
    fn device_status_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.status
            .as_ref()
            .map(|status| (&status.device.state, &status.device.name))
            .hash(&mut hasher);
        self.status_error.hash(&mut hasher);
        self.lighting_device.is_none().hash(&mut hasher);
        self.input_monitor.is_none().hash(&mut hasher);
        hasher.finish()
    }

    fn refresh_device_notices(&mut self) {
        let key = self.device_status_fingerprint();
        if self.device_notices.is_empty() || key != self.device_notices_key {
            self.device_notices = self.device_status_notices();
            self.device_notices_key = key;
        }
    }

    fn device_status_notices(&self) -> Vec<UiNotice> {
        let mut notices = Vec::new();
        match (&self.status, &self.status_error) {
            (Some(status), _) => {
                if status.device.state != "active" {
                    notices.push(UiNotice {
                        severity: NoticeSeverity::Error,
                        message: format!(
                            "Microphone is {}: {}",
                            status.device.state, status.device.name
                        ),
                    });
                } else if !is_expected_hyperx_device_name(&status.device.name) {
                    notices.push(UiNotice {
                        severity: NoticeSeverity::Warning,
                        message: format!("Default input is not QuadCast S: {}", status.device.name),
                    });
                }
            }
            (None, Some(error)) => notices.push(UiNotice {
                severity: NoticeSeverity::Error,
                message: format!("Microphone unavailable: {error}"),
            }),
            (None, None) => notices.push(UiNotice {
                severity: NoticeSeverity::Error,
                message: "Microphone unavailable.".to_string(),
            }),
        }

        if self.lighting_device.is_none() {
            notices.push(UiNotice {
                severity: NoticeSeverity::Warning,
                message: "Lighting HID not detected; effects and persistent save are unavailable."
                    .to_string(),
            });
        }

        if self.input_monitor.is_none() {
            notices.push(UiNotice {
                severity: NoticeSeverity::Warning,
                message:
                    "Input meter unavailable; capture level monitoring is unsupported or busy."
                        .to_string(),
            });
        }

        if notices.is_empty() {
            notices.push(UiNotice {
                severity: NoticeSeverity::Good,
                message: "Audio and lighting devices ready.".to_string(),
            });
        }
        notices
    }

    fn ui_pattern_panel(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(230.0);
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
            section_label(ui, "POLAR PATTERN");
            ui.add_space(4.0);
            let last_used_color = egui::Color32::from_rgb(180, 184, 188);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Last used:")
                        .size(12.0)
                        .color(last_used_color),
                );
                ui.label(
                    egui::RichText::new(self.polar_pattern.label())
                        .size(12.0)
                        .color(last_used_color)
                        .strong(),
                );
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                pattern_tile(ui, PolarPattern::Stereo, self.polar_pattern);
                pattern_tile(ui, PolarPattern::Omni, self.polar_pattern);
            });
            ui.horizontal(|ui| {
                pattern_tile(ui, PolarPattern::Cardioid, self.polar_pattern);
                pattern_tile(ui, PolarPattern::Bidirectional, self.polar_pattern);
            });
        });
    }

    pub(super) fn ui_mic_stage(&mut self, ui: &mut egui::Ui, height: f32) {
        let available = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(available, height), egui::Sense::hover());
        let painter = ui.painter_at(rect);

        painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(22, 23, 23));
        let pattern_width = 160.0_f32.min(rect.width() * 0.42);
        let pattern_left = rect.right() - pattern_width - 16.0;
        let mic_area_right = pattern_left - Self::STAGE_MIC_GAP;
        let center = egui::pos2((rect.left() + mic_area_right) * 0.5, rect.center().y);
        let glow_radius = rect.height() * 0.36;
        for (index, color) in self.lighting.colors.iter().enumerate() {
            let angle = index as f32 / self.lighting.colors.len() as f32 * std::f32::consts::TAU;
            let pos = center
                + egui::vec2(
                    angle.cos() * glow_radius * 0.9,
                    angle.sin() * glow_radius * 0.45,
                );
            painter.circle_filled(pos, glow_radius * 0.55, color.linear_multiply(0.08));
        }

        draw_microphone(&painter, center, rect.height());

        let muted = self.is_muted();
        if let Some(status) = &self.status {
            let text = format!(
                "{} | {}% | {} | {}",
                status.device.name,
                status.volume,
                if muted { "Muted" } else { "Live" },
                self.polar_pattern.label(),
            );
            painter.text(
                rect.left_top() + egui::vec2(16.0, 16.0),
                egui::Align2::LEFT_TOP,
                text,
                egui::FontId::proportional(15.0),
                egui::Color32::from_rgb(210, 214, 218),
            );
        }

        let mut notice_y = rect.top() + 42.0;
        self.refresh_device_notices();
        for notice in self.device_notices.iter().take(4) {
            painter.text(
                rect.left_top() + egui::vec2(16.0, notice_y - rect.top()),
                egui::Align2::LEFT_TOP,
                &notice.message,
                egui::FontId::proportional(12.0),
                notice_color(notice.severity),
            );
            notice_y += 16.0;
        }

        let pattern_rect = egui::Rect::from_min_max(
            egui::pos2(pattern_left, rect.top() + 16.0),
            egui::pos2(pattern_left + pattern_width, rect.bottom() - 16.0),
        );
        // Render the pattern panel positioned absolutely inside the already-allocated
        // stage rect. Using `new_child` (instead of `scope_builder`) deliberately avoids
        // advancing the parent cursor, so it does not add phantom vertical space below the
        // stage that would push the audio/lighting panels down.
        let mut pattern_ui = ui.new_child(egui::UiBuilder::new().max_rect(pattern_rect));
        self.ui_pattern_panel(&mut pattern_ui);
    }
}
