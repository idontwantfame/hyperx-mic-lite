use eframe::egui;
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use crate::{
    audio::set_mic_mute,
    config::{export_config, import_config},
    diagnostics::export_diagnostics_bundle,
    gui_widgets::{card_frame, pill, section_label},
    logging::log_event,
    model::HidEvent,
    paths::log_file_path,
    tray::{TrayCommand, TrayHandle, TrayState},
};

use super::{LightingUiEvent, MicLiteApp, NoticeSeverity, UiNotice, outside_event_loop, theme};

#[cfg(windows)]
const MINIMIZE_TO_TRAY_LABEL: &str = "Minimize to tray";
#[cfg(target_os = "macos")]
const MINIMIZE_TO_TRAY_LABEL: &str = "Keep running in the menu bar";

#[cfg(windows)]
const OPEN_LOGS_LABEL: &str = "Open logs in terminal";
#[cfg(target_os = "macos")]
const OPEN_LOGS_LABEL: &str = "Open logs in Console";

/// A PowerShell window following the log file.
#[cfg(windows)]
fn log_viewer_command(log: &Path) -> Command {
    // PowerShell single-quoted strings escape ' by doubling it; this keeps unusual
    // (e.g. redirected %APPDATA%) paths from breaking out of the quoted argument.
    let escaped_path = log.display().to_string().replace('\'', "''");
    let command = format!("Get-Content -Path '{escaped_path}' -Wait -Tail 100");
    let mut viewer = Command::new("cmd");
    viewer.args([
        "/C",
        "start",
        "powershell",
        "-NoExit",
        "-Command",
        command.as_str(),
    ]);
    viewer
}

/// Console.app, which follows a plain log file as it grows. The path goes in as its own
/// argument, so no shell quoting is involved.
#[cfg(target_os = "macos")]
fn log_viewer_command(log: &Path) -> Command {
    let mut viewer = Command::new("/usr/bin/open");
    viewer.args(["-a", "Console"]).arg(log);
    viewer
}

impl MicLiteApp {
    const DASHBOARD_STAGE_HEIGHT: f32 = 158.0;
    const DASHBOARD_COLUMN_GAP: f32 = 12.0;
    /// Height of the three control cards, sized to the tallest (colour and actions).
    const DASHBOARD_CARD_HEIGHT: f32 = 310.0;

    pub(super) fn ensure_tray_started(&mut self) {
        if self.tray_handle.is_none() {
            self.tray_handle = Some(TrayHandle::start());
        }
    }

    /// What the tray icon and its menu should show right now.
    pub(super) fn tray_state(&self) -> TrayState {
        TrayState {
            connected: self.devices.connected(),
            muted: self.is_muted(),
            lighting_on: self.lighting_cancel.is_some(),
            effect: (!self.lighting.split_layers).then_some(self.lighting.effect),
        }
    }

    pub(super) fn apply_tray_command(&mut self, command: TrayCommand) {
        log_event(
            "info",
            "tray.command",
            &[("command", format!("{command:?}"))],
        );
        match command {
            TrayCommand::ToggleMute => {
                let muted = self.is_muted();
                self.set_mute(!muted);
            }
            TrayCommand::ToggleLighting => {
                if self.lighting_cancel.is_some() {
                    self.stop_lighting_stream();
                } else {
                    self.apply_lighting_to_microphone();
                }
            }
            TrayCommand::SelectEffect(effect) => {
                // A menu pick is a whole-mic effect, so leave split mode for it to show.
                self.lighting.effect = effect;
                self.lighting.split_layers = false;
                self.save_config_snapshot();
                self.apply_lighting_to_microphone();
            }
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
            ui.label(
                egui::RichText::new("QuadCast S")
                    .size(20.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.label(egui::RichText::new("HyperX Mic Lite").color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button(egui::RichText::new("⚙").size(16.0), |ui| {
                    self.ui_settings_menu(ui);
                })
                .response
                .on_hover_text("Settings");
                if self.devices.connected() {
                    pill(ui, "Connected", theme::GOOD);
                } else {
                    pill(ui, "Disconnected", theme::ERROR);
                }
            });
        });
    }

    fn ui_settings_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(230.0);
        section_label(ui, "SETTINGS");
        ui.separator();
        if ui
            .checkbox(&mut self.minimize_to_tray, MINIMIZE_TO_TRAY_LABEL)
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
        if ui.button(OPEN_LOGS_LABEL).clicked() {
            self.open_log_terminal();
            ui.close();
        }
        ui.separator();
        section_label(ui, "ABOUT");
        ui.small(format!("Version {}", env!("HYPERX_BUILD_VERSION")));
        ui.small(format!(
            "Revision {}",
            option_env!("HYPERX_BUILD_REVISION").unwrap_or("unknown")
        ));
    }

    fn export_config_action(&mut self) {
        if let Some(dest) = rfd::FileDialog::new()
            .set_title("Export configuration")
            .set_file_name("hyperx-config.json")
            .add_filter("JSON", &["json"])
            .save_file()
            && let Err(error) = export_config(&dest)
        {
            log_event(
                "error",
                "config.export.error",
                &[("message", error.to_string())],
            );
        }
    }

    fn import_config_action(&mut self) {
        if let Some(source) = rfd::FileDialog::new()
            .set_title("Import configuration")
            .add_filter("JSON", &["json"])
            .pick_file()
            && let Err(error) = import_config(&source)
        {
            log_event(
                "error",
                "config.import.error",
                &[("message", error.to_string())],
            );
        }
    }

    fn export_diagnostics_action(&mut self) {
        if let Some(folder) = rfd::FileDialog::new()
            .set_title("Choose a folder for the diagnostics bundle")
            .pick_folder()
        {
            let dest = folder.join("hyperx-diagnostics");
            if let Err(error) = outside_event_loop(|| export_diagnostics_bundle(&dest)) {
                log_event("error", "diagnostics.export.error", &[("message", error)]);
            }
        }
    }

    fn open_log_terminal(&mut self) {
        if let Err(error) = log_viewer_command(&log_file_path()).spawn() {
            log_event(
                "error",
                "logs.terminal.error",
                &[("message", error.to_string())],
            );
        }
    }

    /// Service events and periodic refreshes. Runs every tick, including while hidden.
    pub(super) fn tick_background(&mut self) {
        // First: a connect or disconnect invalidates the status, capture stream and HID
        // handles that everything below reads.
        self.drain_device_events();
        self.drain_hid_events();
        self.drain_lighting_events();
        self.drain_mqtt_commands();
        self.refresh_status_periodic();
        self.ensure_input_monitor();
        self.refresh_input_peak();
        self.publish_mqtt_state_periodic();
    }

    pub(super) fn ui_dashboard(&mut self, ui: &mut egui::Ui) {
        self.ui_mic_stage(ui, Self::DASHBOARD_STAGE_HEIGHT);
        ui.add_space(Self::DASHBOARD_COLUMN_GAP);
        let gap = Self::DASHBOARD_COLUMN_GAP;
        let width = (ui.available_width() - 2.0 * gap) / 3.0;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            dashboard_card(ui, "Audio", width, |ui| self.ui_audio_panel(ui));
            dashboard_card(ui, "Lighting", width, |ui| self.ui_effect_panel(ui));
            dashboard_card(ui, "Color", width, |ui| self.ui_color_panel(ui));
        });
    }
}

/// One of the three equal control cards under the stage.
fn dashboard_card(
    ui: &mut egui::Ui,
    title: &str,
    width: f32,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    // The frame adds its margin and border around the content.
    let chrome = 2.0 * 14.0 + 2.0;
    ui.allocate_ui_with_layout(
        egui::vec2(width, MicLiteApp::DASHBOARD_CARD_HEIGHT),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            card_frame().show(ui, |ui| {
                ui.set_width(width - chrome);
                ui.set_min_height(MicLiteApp::DASHBOARD_CARD_HEIGHT - chrome);
                section_label(ui, title);
                ui.add_space(6.0);
                add_contents(ui);
            });
        },
    );
}
