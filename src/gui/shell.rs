use eframe::egui;
use std::{
    process::Command,
    time::{Duration, Instant},
};

use crate::{
    audio::set_mic_mute,
    config::{export_config, import_config},
    diagnostics::export_diagnostics_bundle,
    gui_widgets::section_label,
    logging::log_event,
    model::HidEvent,
    paths::log_file_path,
    tray::TrayHandle,
};

use super::{LightingUiEvent, MicLiteApp, NoticeSeverity, UiNotice};

impl MicLiteApp {
    pub(super) fn ensure_tray_started(&mut self) {
        if self.tray_handle.is_none() {
            self.tray_handle = Some(TrayHandle::start());
        }
    }

    pub(super) fn restore_from_tray(&mut self, ctx: &egui::Context) {
        self.hidden_to_tray = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        log_event("info", "gui.restore_from_tray", &[]);
    }

    pub(super) fn remember_window_position(&mut self, ctx: &egui::Context) {
        if self.hidden_to_tray || self.last_window_position_save.elapsed() < Duration::from_secs(2)
        {
            return;
        }

        let position = ctx.input(|input| input.viewport().outer_rect.map(|rect| rect.left_top()));
        let Some(position) = position else {
            return;
        };
        if !position.x.is_finite() || !position.y.is_finite() {
            return;
        }

        let changed = match (self.window_x, self.window_y) {
            (Some(x), Some(y)) => (x - position.x).abs() >= 1.0 || (y - position.y).abs() >= 1.0,
            _ => true,
        };
        self.last_window_position_save = Instant::now();
        if changed {
            self.window_x = Some(position.x);
            self.window_y = Some(position.y);
            self.save_config_snapshot();
        }
    }

    pub(super) fn drain_hid_events(&mut self) {
        while let Ok(event) = self.hid_events.try_recv() {
            match event {
                HidEvent::Mute(is_live) => {
                    self.hid_muted = !is_live;
                    match set_mic_mute(!is_live) {
                        Ok(()) => self.refresh_status(),
                        Err(error) => self.status_error = Some(error.to_string()),
                    }
                    log_event("info", "gui.hid.mute", &[("live", is_live.to_string())]);
                    if self.lighting.live_when_muted {
                        self.apply_live_mute_lighting(is_live);
                    }
                }
                HidEvent::Pattern(pattern) => {
                    self.polar_pattern = pattern;
                    self.save_config_snapshot();
                }
            }
        }
    }

    pub(super) fn drain_lighting_events(&mut self) {
        while let Ok(event) = self.lighting_events.try_recv() {
            self.lighting_notice = Some(match event {
                LightingUiEvent::Applied => UiNotice {
                    severity: NoticeSeverity::Good,
                    message: "Lighting stream is running.".to_string(),
                },
                LightingUiEvent::ApplyFailed(error) => UiNotice {
                    severity: NoticeSeverity::Error,
                    message: format!("Lighting write failed: {error}"),
                },
                LightingUiEvent::Saved => UiNotice {
                    severity: NoticeSeverity::Good,
                    message: "Lighting saved to microphone memory.".to_string(),
                },
                LightingUiEvent::SaveFailed(error) => UiNotice {
                    severity: NoticeSeverity::Error,
                    message: format!("Persistent save failed: {error}"),
                },
            });
        }
    }

    pub(super) fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("HyperX QuadCast S");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(12.0);
                ui.menu_button("⚙", |ui| {
                    self.ui_settings_menu(ui);
                })
                .response
                .on_hover_text("Settings");
                ui.add_space(6.0);
                if ui.button("⟳").on_hover_text("Refresh").clicked() {
                    self.refresh_devices();
                }
                if self.layout_edit {
                    ui.label(egui::RichText::new("Layout edit").color(egui::Color32::YELLOW));
                }
            });
        });
    }

    fn ui_settings_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(230.0);
        section_label(ui, "SETTINGS");
        ui.separator();
        if ui
            .checkbox(&mut self.minimize_to_tray, "Minimize to tray")
            .changed()
        {
            if self.minimize_to_tray {
                self.ensure_tray_started();
            } else if self.hidden_to_tray {
                self.restore_from_tray(ui.ctx());
            }
            self.save_config_snapshot();
            log_event(
                "info",
                "tray.option",
                &[("enabled", self.minimize_to_tray.to_string())],
            );
        }
        if ui
            .checkbox(
                &mut self.mute_on_app_start,
                "Mute microphone when app starts",
            )
            .changed()
        {
            self.save_config_snapshot();
        }
        ui.separator();
        section_label(ui, "INTEGRATIONS");
        let mqtt_label = if self.mqtt_config.enabled {
            "MQTT settings… (enabled)"
        } else {
            "MQTT settings…"
        };
        if ui.button(mqtt_label).clicked() {
            self.mqtt_settings_open = true;
            ui.close();
        }
        ui.separator();
        section_label(ui, "CONFIG");
        if ui.button("Export config…").clicked() {
            self.export_config_action();
            ui.close();
        }
        if ui.button("Import config…").clicked() {
            self.import_config_action();
            ui.close();
        }
        if ui.button("Export diagnostics…").clicked() {
            self.export_diagnostics_action();
            ui.close();
        }
        if ui.button("Open logs in terminal").clicked() {
            self.open_log_terminal();
            ui.close();
        }
    }

    fn export_config_action(&mut self) {
        if let Some(dest) = rfd::FileDialog::new()
            .set_title("Export configuration")
            .set_file_name("hyperx-config.json")
            .add_filter("JSON", &["json"])
            .save_file()
        {
            if let Err(error) = export_config(&dest) {
                log_event("error", "config.export.error", &[("message", error)]);
            }
        }
    }

    fn import_config_action(&mut self) {
        if let Some(source) = rfd::FileDialog::new()
            .set_title("Import configuration")
            .add_filter("JSON", &["json"])
            .pick_file()
        {
            if let Err(error) = import_config(&source) {
                log_event("error", "config.import.error", &[("message", error)]);
            }
        }
    }

    fn export_diagnostics_action(&mut self) {
        if let Some(folder) = rfd::FileDialog::new()
            .set_title("Choose a folder for the diagnostics bundle")
            .pick_folder()
        {
            let dest = folder.join("hyperx-diagnostics");
            if let Err(error) = export_diagnostics_bundle(&dest) {
                log_event("error", "diagnostics.export.error", &[("message", error)]);
            }
        }
    }

    fn open_log_terminal(&mut self) {
        let log = log_file_path();
        let command = format!("Get-Content -Path '{}' -Wait -Tail 100", log.display());
        if let Err(error) = Command::new("cmd")
            .args([
                "/C",
                "start",
                "powershell",
                "-NoExit",
                "-Command",
                command.as_str(),
            ])
            .spawn()
        {
            log_event(
                "error",
                "logs.terminal.error",
                &[("message", error.to_string())],
            );
        }
    }

    pub(super) fn ui_layout_editor(&mut self, ui: &mut egui::Ui) {
        if !self.layout_edit {
            return;
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            section_label(ui, "LAYOUT");
            ui.small(
                "Drag the stage bottom edge, the audio/lighting splitter, and the polar panel.",
            );
            if ui.button("Reset").clicked() {
                self.dashboard_stage_height = 250.0;
                self.dashboard_audio_width = 285.0;
                self.dashboard_lighting_width = 590.0;
                self.dashboard_column_gap = 18.0;
                self.stage_pattern_left_factor = 0.56;
                self.stage_pattern_width = 235.0;
                self.stage_mic_gap = 18.0;
                self.save_config_snapshot();
            }
        });
    }

    pub(super) fn ui_dashboard(&mut self, ui: &mut egui::Ui) {
        self.drain_hid_events();
        self.drain_lighting_events();
        self.drain_mqtt_commands();
        self.refresh_status_periodic();
        self.refresh_input_peak();
        self.publish_mqtt_state_periodic();
        let stage_height = self.dashboard_stage_height.clamp(240.0, 264.0);
        self.ui_mic_stage(ui, stage_height);
        if self.layout_edit {
            let (handle_rect, drag) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 10.0), egui::Sense::drag());
            ui.painter().rect_filled(
                handle_rect,
                0.0,
                egui::Color32::from_rgba_unmultiplied(255, 255, 0, 32),
            );
            ui.painter().line_segment(
                [
                    handle_rect.center_top() + egui::vec2(12.0, 5.0),
                    handle_rect.center_top() + egui::vec2(handle_rect.width() - 12.0, 5.0),
                ],
                egui::Stroke::new(1.0, egui::Color32::YELLOW),
            );
            if drag.dragged() {
                self.dashboard_stage_height =
                    (self.dashboard_stage_height + drag.drag_delta().y).clamp(240.0, 264.0);
                self.save_config_snapshot();
            }
        } else {
            ui.separator();
        }
        let total_width = ui.available_width().min(650.0);
        let gap = self.dashboard_column_gap.clamp(12.0, 18.0);
        let min_audio = 190.0;
        let min_lighting = 340.0;
        let max_audio = (total_width - gap - min_lighting).max(min_audio);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.allocate_ui(egui::vec2(190.0, 320.0), |ui| {
                self.ui_audio_panel(ui);
            });

            if self.layout_edit {
                let (split_rect, drag) =
                    ui.allocate_exact_size(egui::vec2(gap.max(10.0), 360.0), egui::Sense::drag());
                ui.painter().rect_filled(
                    split_rect,
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 12),
                );
                ui.painter().line_segment(
                    [split_rect.center_top(), split_rect.center_bottom()],
                    egui::Stroke::new(1.0, egui::Color32::YELLOW),
                );
                if drag.dragged() {
                    let next_audio = (self.dashboard_audio_width + drag.drag_delta().x)
                        .clamp(min_audio, max_audio);
                    self.dashboard_audio_width = next_audio;
                    self.dashboard_lighting_width =
                        (total_width - next_audio - gap).max(min_lighting);
                    self.save_config_snapshot();
                }
            } else {
                ui.add_space(gap);
            }

            self.ui_lighting_panel(ui, gap);
        });
    }
}
