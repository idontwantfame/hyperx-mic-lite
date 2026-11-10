use eframe::egui;
use std::time::{Duration, Instant};

use crate::{
    audio::{
        AudioClassControl, input_peak_value, mic_status, set_audio_control_volume, set_mic_mute,
        set_mic_volume_percent,
    },
    gui_widgets::{level_meter, secondary_button, value_row, value_slider},
};

use super::{MicLiteApp, theme};

impl MicLiteApp {
    pub(super) fn refresh_input_peak(&mut self) {
        if self.last_peak_update.elapsed() < Duration::from_millis(80) {
            return;
        }
        self.last_peak_update = Instant::now();
        // A failed stream keeps reporting its last sample, so fall through to the endpoint
        // meter rather than showing a frozen level until the retry reopens the stream.
        match self
            .input_monitor
            .as_ref()
            .filter(|_| self.input_monitor_alive())
        {
            Some(monitor) => self.input_peak = monitor.peak().clamp(0.0, 1.0),
            None => {
                if let Ok(peak) = input_peak_value() {
                    self.input_peak = peak.clamp(0.0, 1.0);
                }
            }
        }
    }

    pub(super) fn refresh_status(&mut self) {
        match mic_status() {
            Ok(status) => {
                self.mic_volume = status.volume;
                self.status = Some(status);
                self.status_error = None;
            }
            Err(error) => {
                // Clear the old status rather than keeping it alongside the error. A stale
                // `Some` outranks the error in every consumer (notices, the stage header,
                // MQTT availability), which is how an unplugged mic used to keep being
                // reported as present.
                self.status = None;
                self.status_error = Some(error.to_string());
            }
        }
    }

    pub(super) fn refresh_status_periodic(&mut self) {
        if self.last_status_update.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_status_update = Instant::now();
        self.refresh_status();
    }

    pub(super) fn set_mute(&mut self, muted: bool) {
        match set_mic_mute(muted) {
            Ok(()) => self.refresh_status(),
            Err(error) => self.status_error = Some(error.to_string()),
        }
    }

    pub(super) fn set_volume(&mut self) {
        match set_mic_volume_percent(self.mic_volume) {
            Ok(()) => self.refresh_status(),
            Err(error) => self.status_error = Some(error.to_string()),
        }
    }

    pub(super) fn set_mic_monitoring(&mut self) {
        match set_audio_control_volume(AudioClassControl::Monitoring, self.mic_monitoring) {
            Ok(()) => self.status_error = None,
            Err(error) => self.status_error = Some(error.to_string()),
        }
    }

    pub(super) fn set_headphone_volume(&mut self) {
        match set_audio_control_volume(AudioClassControl::Headphone, self.headphone_volume) {
            Ok(()) => self.status_error = None,
            Err(error) => self.status_error = Some(error.to_string()),
        }
    }

    pub(super) fn ui_audio_panel(&mut self, ui: &mut egui::Ui) {
        let muted = self.is_muted();
        if value_slider(ui, "Mic level", &mut self.mic_volume).changed() {
            self.set_volume();
            self.save_config_snapshot();
        }

        ui.add_space(6.0);
        let display_peak = self.input_peak.sqrt().clamp(0.0, 1.0);
        value_row(ui, "Input", &format!("{:.0}%", display_peak * 100.0));
        level_meter(ui, display_peak).on_hover_text("The dial on the mic sets hardware gain.");

        ui.add_space(8.0);
        if value_slider(ui, "Monitoring", &mut self.mic_monitoring).changed() {
            self.set_mic_monitoring();
            self.save_config_snapshot();
        }

        ui.add_space(6.0);
        if value_slider(ui, "Headphones", &mut self.headphone_volume).changed() {
            self.set_headphone_volume();
            self.save_config_snapshot();
        }

        ui.add_space(12.0);
        let width = ui.available_width();
        let clicked = if muted {
            ui.add_sized(
                [width, 30.0],
                egui::Button::new(
                    egui::RichText::new("Unmute microphone")
                        .strong()
                        .color(egui::Color32::WHITE),
                )
                .fill(theme::ERROR.gamma_multiply(0.7)),
            )
            .clicked()
        } else {
            secondary_button(ui, "Mute microphone", width).clicked()
        };
        if clicked {
            self.set_mute(!muted);
        }

        if let Some(error) = &self.status_error {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(error).size(11.5).color(theme::ERROR));
        }
    }
}
