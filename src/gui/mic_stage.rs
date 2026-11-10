use eframe::egui;

use std::time::Instant;

use crate::{
    audio::peak_from_bits,
    gui_widgets::{card_frame, draw_microphone, pattern_tile, pill, section_label, value_row},
    model::PolarPattern,
};

use super::{MicLiteApp, NoticeSeverity, UiNotice, notice_color, theme};

impl MicLiteApp {
    const STAGE_STATUS_WIDTH: f32 = 236.0;
    const STAGE_PATTERN_WIDTH: f32 = 160.0;
    const PREVIEW_IDLE_INTENSITY: f32 = 0.45;

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
        self.input_monitor_alive().hash(&mut hasher);
        // One counter stands in for the whole snapshot, which is not itself hashable.
        self.device_generation.hash(&mut hasher);
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
        // A missing mic explains every downstream failure, so report it on its own instead
        // of stacking the lighting and meter warnings it causes on top.
        if !self.devices.connected() {
            return vec![
                UiNotice {
                    severity: NoticeSeverity::Error,
                    message: "QuadCast S disconnected. Reconnect the USB cable.".to_string(),
                },
                UiNotice {
                    severity: NoticeSeverity::Info,
                    message: "Controls stay idle until it is back.".to_string(),
                },
            ];
        }

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

        if !self.input_monitor_alive() {
            notices.push(UiNotice {
                severity: NoticeSeverity::Warning,
                message: "Input meter unavailable; retrying.".to_string(),
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

    fn ui_status_panel(&mut self, ui: &mut egui::Ui) {
        section_label(ui, "Status");
        ui.add_space(2.0);
        let muted = self.is_muted();
        ui.horizontal(|ui| {
            if !self.devices.connected() {
                pill(ui, "Disconnected", theme::ERROR);
            } else if muted {
                pill(ui, "Muted", theme::ERROR);
            } else {
                pill(ui, "Live", theme::GOOD);
            }
        });
        ui.add_space(4.0);
        let level = self
            .status
            .as_ref()
            .map_or_else(|| "—".to_string(), |status| format!("{}%", status.volume));
        value_row(ui, "Mic level", &level);
        let pattern = match self.polar_pattern {
            PolarPattern::Unknown(_) => "Turn the knob".to_string(),
            pattern => pattern.label().to_string(),
        };
        value_row(ui, "Polar pattern", &pattern);
        let lighting = if self.lighting_cancel.is_some() {
            if self.lighting.split_layers {
                "Split layers".to_string()
            } else {
                self.lighting.effect.label().to_string()
            }
        } else {
            "Off".to_string()
        };
        value_row(ui, "Lighting", &lighting);

        ui.add_space(6.0);
        self.refresh_device_notices();
        for notice in self.device_notices.iter().take(3) {
            ui.label(
                egui::RichText::new(&notice.message)
                    .size(11.5)
                    .color(notice_color(notice.severity)),
            );
        }
    }

    fn ui_pattern_panel(&mut self, ui: &mut egui::Ui) {
        section_label(ui, "Polar pattern");
        ui.add_space(2.0);
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        ui.horizontal(|ui| {
            pattern_tile(ui, PolarPattern::Stereo, self.polar_pattern);
            pattern_tile(ui, PolarPattern::Omni, self.polar_pattern);
        });
        ui.horizontal(|ui| {
            pattern_tile(ui, PolarPattern::Cardioid, self.polar_pattern);
            pattern_tile(ui, PolarPattern::Bidirectional, self.polar_pattern);
        });
    }

    /// How strongly the drawn microphone shows the preview: dark while muted, as the
    /// hardware is, and dimmed while no stream is running, since then it only previews
    /// what Apply would send.
    fn preview_intensity(&self) -> f32 {
        if self.is_muted() {
            0.0
        } else if self.lighting_cancel.is_some() {
            1.0
        } else {
            Self::PREVIEW_IDLE_INTENSITY
        }
    }

    pub(super) fn ui_mic_stage(&mut self, ui: &mut egui::Ui, height: f32) {
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(height);
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(Self::STAGE_STATUS_WIDTH, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.ui_status_panel(ui),
                );

                let mic_width = (ui.available_width() - Self::STAGE_PATTERN_WIDTH - 8.0).max(80.0);
                let (mic_rect, mic_response) =
                    ui.allocate_exact_size(egui::vec2(mic_width, height), egui::Sense::hover());
                let program = self.lighting_program();
                let input_peak = peak_from_bits(&self.input_peak_bits);
                let (top, bottom) =
                    self.lighting_preview
                        .zones(&program, input_peak, Instant::now());
                draw_microphone(
                    &ui.painter_at(mic_rect),
                    mic_rect,
                    [rgb(top), rgb(bottom)],
                    self.preview_intensity(),
                );
                mic_response.on_hover_text("Preview of the selected lighting");

                ui.allocate_ui_with_layout(
                    egui::vec2(Self::STAGE_PATTERN_WIDTH, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.ui_pattern_panel(ui),
                );
            });
        });
    }
}

fn rgb([r, g, b]: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(r, g, b)
}
