use eframe::egui;
use std::time::{Duration, Instant};

use crate::{
    audio::{
        AudioClassControl, input_peak_value, mic_status, set_audio_control_volume, set_mic_mute,
        set_mic_volume_percent,
    },
    gui_widgets::{percent_slider, section_label},
    lighting::detect_lighting_device,
    logging::log_event,
};

use super::{MicLiteApp, NoticeSeverity, is_expected_hyperx_device_name, notice_color};

impl MicLiteApp {
    pub(super) fn refresh_input_peak(&mut self) {
        if self.last_peak_update.elapsed() < Duration::from_millis(80) {
            return;
        }
        self.last_peak_update = Instant::now();
        if let Some(monitor) = &self.input_monitor {
            self.input_peak = monitor.peak().clamp(0.0, 1.0);
        } else if let Ok(peak) = input_peak_value() {
            self.input_peak = peak.clamp(0.0, 1.0);
        }
    }

    pub(super) fn refresh_status(&mut self) {
        match mic_status() {
            Ok(status) => {
                self.mic_volume = status.volume;
                self.status = Some(status);
                self.status_error = None;
            }
            Err(error) => self.status_error = Some(error.to_string()),
        }
    }

    pub(super) fn refresh_devices(&mut self) {
        self.refresh_status();
        self.lighting_device = detect_lighting_device();
        self.lighting_notice = None;
        log_event(
            "info",
            "gui.devices.refresh",
            &[(
                "lighting_detected",
                self.lighting_device.is_some().to_string(),
            )],
        );
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
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.vertical(|ui| {
                section_label(ui, "AUDIO");
                ui.add_space(4.0);
                section_label(ui, "MIC VOLUME");
                if percent_slider(ui, &mut self.mic_volume, 160.0).changed() {
                    self.set_volume();
                    self.save_config_snapshot();
                }

                ui.add_space(6.0);
                section_label(ui, "INPUT LEVEL");
                let display_peak = self.input_peak.sqrt().clamp(0.0, 1.0);
                ui.add(
                    egui::ProgressBar::new(display_peak)
                        .desired_width(160.0)
                        .text(format!("{:.1}%", display_peak * 100.0)),
                );
                ui.small("Bottom dial controls hardware gain.");

                ui.add_space(6.0);
                section_label(ui, "MIC MONITORING");
                if percent_slider(ui, &mut self.mic_monitoring, 160.0).changed() {
                    self.set_mic_monitoring();
                    self.save_config_snapshot();
                }

                ui.add_space(6.0);
                section_label(ui, "HEADPHONE VOLUME");
                if percent_slider(ui, &mut self.headphone_volume, 160.0).changed() {
                    self.set_headphone_volume();
                    self.save_config_snapshot();
                }

                ui.add_space(8.0);
                let button_text = if muted {
                    "Unmute microphone"
                } else {
                    "Mute microphone"
                };
                if ui
                    .add_sized([150.0, 24.0], egui::Button::new(button_text))
                    .clicked()
                {
                    self.set_mute(!muted);
                }

                if let Some(error) = &self.status_error {
                    ui.add_space(8.0);
                    ui.colored_label(egui::Color32::from_rgb(255, 120, 120), error);
                } else if self
                    .status
                    .as_ref()
                    .is_some_and(|status| !is_expected_hyperx_device_name(&status.device.name))
                {
                    ui.add_space(8.0);
                    ui.colored_label(
                        notice_color(NoticeSeverity::Warning),
                        "Controls target the current Windows default input.",
                    );
                }
            });
        });
    }
}
