use eframe::egui;
use std::{
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    audio::{
        AudioClassControl, AudioPeakMonitor, input_peak_value, mic_status,
        set_audio_control_volume, set_mic_mute, set_mic_volume_percent, start_audio_peak_monitor,
    },
    config::{
        AppConfig, AudioConfig, LightingConfig, UiConfig, export_config, import_config,
        load_or_create_config, save_config,
    },
    constants::CONFIG_SCHEMA_VERSION,
    diagnostics::export_diagnostics_bundle,
    gui_widgets::{
        color_swatch, draw_microphone, pattern_tile, percent_slider, section_label, target_button,
    },
    lighting::{
        LightingProgram, LightingState, StreamDuration, color_to_hex, detect_lighting_device,
        live_mute_lighting_color, parse_rgb_hex, save_lighting_to_microphone,
        spawn_hid_event_listener, stream_lighting_program_cancelable, write_solid_lighting_once,
    },
    logging::log_event,
    model::{Effect, HidEvent, LightTarget, LightingDevice, MicStatus, PolarPattern, Tab},
    paths::log_file_path,
    tray::TrayHandle,
};
pub(crate) struct MicLiteApp {
    tab: Tab,
    status: Option<MicStatus>,
    status_error: Option<String>,
    mic_volume: u8,
    mic_monitoring: u8,
    headphone_volume: u8,
    mute_on_app_start: bool,
    hid_muted: bool,
    input_peak: f32,
    input_monitor: Option<AudioPeakMonitor>,
    last_peak_update: Instant,
    last_status_update: Instant,
    polar_pattern: PolarPattern,
    hid_events: Receiver<HidEvent>,
    lighting: LightingState,
    lighting_device: Option<LightingDevice>,
    lighting_cancel: Option<Arc<AtomicBool>>,
    lighting_autostart_applied: bool,
    minimize_to_tray: bool,
    hidden_to_tray: bool,
    force_exit: bool,
    tray_handle: Option<TrayHandle>,
    start_minimized: bool,
    start_minimized_applied: bool,
    layout_edit: bool,
    stage_pattern_left_factor: f32,
    stage_pattern_width: f32,
    stage_mic_gap: f32,
    dashboard_stage_height: f32,
    dashboard_audio_width: f32,
    dashboard_lighting_width: f32,
    dashboard_column_gap: f32,
}

impl MicLiteApp {
    pub(crate) fn new(start_minimized: bool, layout_edit: bool) -> Self {
        let config = load_or_create_config().unwrap_or_else(|_| AppConfig::default());
        let colors = config
            .lighting
            .colors
            .iter()
            .filter_map(|color| parse_rgb_hex(color).ok())
            .map(|rgb| egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
            .collect::<Vec<_>>();
        let mut app = Self {
            tab: Tab::from_config(&config.ui.selected_tab),
            status: None,
            status_error: None,
            mic_volume: config.audio.mic_volume,
            mic_monitoring: config.audio.mic_monitoring,
            headphone_volume: config.audio.headphone_volume,
            mute_on_app_start: config.audio.mute_on_app_start,
            hid_muted: false,
            input_peak: 0.0,
            input_monitor: match start_audio_peak_monitor() {
                Ok(monitor) => {
                    log_event("info", "gui.audio.capture.start", &[]);
                    Some(monitor)
                }
                Err(error) => {
                    log_event("warn", "gui.audio.capture.error", &[("message", error)]);
                    None
                }
            },
            last_peak_update: Instant::now(),
            last_status_update: Instant::now(),
            polar_pattern: PolarPattern::from_config(&config.ui.last_polar_pattern),
            hid_events: spawn_hid_event_listener(),
            lighting: LightingState {
                effect: Effect::from_config(&config.lighting.effect),
                target: LightTarget::from_config(&config.lighting.target),
                colors: if colors.is_empty() {
                    AppConfig::default()
                        .lighting
                        .colors
                        .iter()
                        .filter_map(|color| parse_rgb_hex(color).ok())
                        .map(|rgb| egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
                        .collect()
                } else {
                    colors
                },
                selected_color: config.lighting.selected_color,
                opacity: config.lighting.opacity,
                speed: config.lighting.speed,
                brightness: config.lighting.brightness,
                live_when_muted: config.lighting.live_when_muted,
            },
            lighting_device: detect_lighting_device(),
            lighting_cancel: None,
            lighting_autostart_applied: false,
            minimize_to_tray: config.ui.minimize_to_tray,
            hidden_to_tray: false,
            force_exit: false,
            tray_handle: if config.ui.minimize_to_tray {
                Some(TrayHandle::start())
            } else {
                None
            },
            start_minimized,
            start_minimized_applied: false,
            layout_edit,
            stage_pattern_left_factor: config.ui.stage_pattern_left_factor,
            stage_pattern_width: config.ui.stage_pattern_width,
            stage_mic_gap: config.ui.stage_mic_gap,
            dashboard_stage_height: config.ui.dashboard_stage_height,
            dashboard_audio_width: config.ui.dashboard_audio_width,
            dashboard_lighting_width: config.ui.dashboard_lighting_width,
            dashboard_column_gap: config.ui.dashboard_column_gap,
        };
        app.refresh_status();
        if app.mute_on_app_start {
            app.set_mute(true);
        }
        if app.lighting.selected_color >= app.lighting.colors.len() {
            app.lighting.selected_color = 0;
        }
        app
    }

    fn save_config_snapshot(&self) {
        let config = AppConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            audio: AudioConfig {
                mic_volume: self.mic_volume,
                mic_monitoring: self.mic_monitoring,
                headphone_volume: self.headphone_volume,
                mute_on_app_start: self.mute_on_app_start,
            },
            lighting: LightingConfig {
                effect: self.lighting.effect.as_config().to_string(),
                target: self.lighting.target.as_config().to_string(),
                colors: self
                    .lighting
                    .colors
                    .iter()
                    .map(|color| color_to_hex(*color))
                    .collect(),
                selected_color: self.lighting.selected_color,
                opacity: self.lighting.opacity,
                speed: self.lighting.speed,
                brightness: self.lighting.brightness,
                live_when_muted: self.lighting.live_when_muted,
            },
            ui: UiConfig {
                selected_tab: self.tab.as_config().to_string(),
                window_width: 1120.0,
                window_height: 760.0,
                minimize_to_tray: self.minimize_to_tray,
                last_polar_pattern: self.polar_pattern.as_config().to_string(),
                stage_pattern_left_factor: self.stage_pattern_left_factor,
                stage_pattern_width: self.stage_pattern_width,
                stage_mic_gap: self.stage_mic_gap,
                dashboard_stage_height: self.dashboard_stage_height,
                dashboard_audio_width: self.dashboard_audio_width,
                dashboard_lighting_width: self.dashboard_lighting_width,
                dashboard_column_gap: self.dashboard_column_gap,
            },
            service: load_or_create_config()
                .map(|config| config.service)
                .unwrap_or_else(|_| AppConfig::default().service),
            device: load_or_create_config()
                .map(|config| config.device)
                .unwrap_or_else(|_| AppConfig::default().device),
        };
        let _ = save_config(&config);
    }

    fn ensure_tray_started(&mut self) {
        if self.tray_handle.is_none() {
            self.tray_handle = Some(TrayHandle::start());
        }
    }

    fn restore_from_tray(&mut self, ctx: &egui::Context) {
        self.hidden_to_tray = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        log_event("info", "gui.restore_from_tray", &[]);
    }

    fn drain_hid_events(&mut self) {
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

    fn refresh_input_peak(&mut self) {
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

    fn refresh_status(&mut self) {
        match mic_status() {
            Ok(status) => {
                self.mic_volume = status.volume;
                self.status = Some(status);
                self.status_error = None;
            }
            Err(error) => self.status_error = Some(error.to_string()),
        }
    }

    fn refresh_status_periodic(&mut self) {
        if self.last_status_update.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_status_update = Instant::now();
        self.refresh_status();
    }

    fn is_muted(&self) -> bool {
        self.status.as_ref().is_some_and(|status| status.muted) || self.hid_muted
    }

    fn set_mute(&mut self, muted: bool) {
        match set_mic_mute(muted) {
            Ok(()) => self.refresh_status(),
            Err(error) => self.status_error = Some(error.to_string()),
        }
    }

    fn set_volume(&mut self) {
        match set_mic_volume_percent(self.mic_volume) {
            Ok(()) => self.refresh_status(),
            Err(error) => self.status_error = Some(error.to_string()),
        }
    }

    fn set_mic_monitoring(&mut self) {
        match set_audio_control_volume(AudioClassControl::Monitoring, self.mic_monitoring) {
            Ok(()) => self.status_error = None,
            Err(error) => self.status_error = Some(error.to_string()),
        }
    }

    fn set_headphone_volume(&mut self) {
        match set_audio_control_volume(AudioClassControl::Headphone, self.headphone_volume) {
            Ok(()) => self.status_error = None,
            Err(error) => self.status_error = Some(error.to_string()),
        }
    }

    fn apply_live_mute_lighting(&mut self, is_live: bool) {
        if self.lighting_cancel.is_some() {
            log_event(
                "info",
                "lighting.live_mute.skip_active_stream",
                &[("live", is_live.to_string())],
            );
            return;
        }
        let color = live_mute_lighting_color(is_live);
        let brightness = self.lighting.brightness;
        log_event(
            "info",
            "lighting.live_mute.apply",
            &[("live", is_live.to_string())],
        );
        thread::spawn(move || {
            if let Err(error) = write_solid_lighting_once(color, brightness, false) {
                log_event("error", "lighting.live_mute.error", &[("message", error)]);
            }
        });
    }

    fn apply_lighting_to_microphone(&mut self) {
        if self.lighting_device.is_none() {
            log_event("warn", "lighting.apply.no_device", &[]);
            return;
        }

        let colors = if self.lighting.effect == Effect::Solid {
            self.lighting
                .colors
                .get(self.lighting.selected_color)
                .copied()
                .map(|color| vec![[color.r(), color.g(), color.b()]])
                .unwrap_or_else(|| vec![[0, 255, 0]])
        } else {
            self.lighting
                .colors
                .iter()
                .map(|color| [color.r(), color.g(), color.b()])
                .collect()
        };

        let program = LightingProgram {
            effect: self.lighting.effect,
            target: self.lighting.target,
            colors,
            speed: self.lighting.speed,
            brightness: self.lighting.brightness,
            shared_peak_bits: if self.lighting.effect == Effect::VuMeter {
                self.input_monitor
                    .as_ref()
                    .map(|monitor| monitor.peak_bits())
            } else {
                None
            },
        };
        log_event(
            "info",
            "lighting.apply.start",
            &[
                ("effect", program.effect.as_config().to_string()),
                ("target", program.target.as_config().to_string()),
            ],
        );

        if let Some(cancel) = &self.lighting_cancel {
            cancel.store(true, Ordering::Relaxed);
            thread::sleep(Duration::from_millis(90));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.lighting_cancel = Some(cancel.clone());

        thread::spawn(move || {
            match stream_lighting_program_cancelable(
                &program,
                StreamDuration::Forever,
                Some(cancel),
                false,
            ) {
                Ok(()) => log_event(
                    "info",
                    "lighting.apply.done",
                    &[("effect", program.effect.as_config().to_string())],
                ),
                Err(error) => log_event("error", "lighting.apply.error", &[("message", error)]),
            }
        });
    }

    fn save_lighting_to_microphone(&mut self) {
        if self.lighting_device.is_none() {
            log_event("warn", "lighting.save.no_device", &[]);
            return;
        }
        thread::spawn(move || match save_lighting_to_microphone(false) {
            Ok(()) => log_event("info", "lighting.save.done", &[]),
            Err(error) => log_event("error", "lighting.save.error", &[("message", error)]),
        });
    }

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
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
                    self.refresh_status();
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

    fn ui_layout_editor(&mut self, ui: &mut egui::Ui) {
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

    fn ui_dashboard(&mut self, ui: &mut egui::Ui) {
        self.drain_hid_events();
        self.refresh_status_periodic();
        self.refresh_input_peak();
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

    fn ui_audio_panel(&mut self, ui: &mut egui::Ui) {
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
                }
            });
        });
    }

    fn ui_lighting_panel(&mut self, ui: &mut egui::Ui, gap: f32) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui(egui::vec2(174.0, 340.0), |ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        section_label(ui, "LIGHTING EFFECTS");
                        ui.add_space(2.0);
                        for effect in [
                            Effect::Wave,
                            Effect::Solid,
                            Effect::Cycle,
                            Effect::Pulse,
                            Effect::Blink,
                            Effect::Lightning,
                            Effect::VuMeter,
                        ] {
                            if ui
                                .selectable_label(self.lighting.effect == effect, effect.label())
                                .clicked()
                            {
                                self.lighting.effect = effect;
                                self.save_config_snapshot();
                            }
                        }

                        ui.add_space(12.0);
                        section_label(ui, "TARGET");
                        let mut target_changed = false;
                        ui.horizontal(|ui| {
                            target_changed |=
                                target_button(ui, &mut self.lighting.target, LightTarget::All);
                            target_changed |=
                                target_button(ui, &mut self.lighting.target, LightTarget::Top);
                            target_changed |=
                                target_button(ui, &mut self.lighting.target, LightTarget::Bottom);
                        });
                        if target_changed {
                            self.save_config_snapshot();
                        }
                    });
                });

                ui.add_space(gap);
                ui.allocate_ui(egui::vec2(196.0, 340.0), |ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        section_label(ui, "COLOR");
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for index in 0..self.lighting.colors.len() {
                                let color = self.lighting.colors[index];
                                let selected = self.lighting.selected_color == index;
                                let response = color_swatch(ui, color, selected);
                                if response.clicked() {
                                    self.lighting.selected_color = index;
                                    self.save_config_snapshot();
                                }
                            }
                        });
                        ui.add_space(6.0);
                        let mut color_changed = false;
                        if let Some(color) =
                            self.lighting.colors.get_mut(self.lighting.selected_color)
                        {
                            color_changed = ui.color_edit_button_srgba(color).changed();
                        }
                        if color_changed {
                            self.save_config_snapshot();
                        }

                        ui.add_space(8.0);
                        section_label(ui, "BRIGHTNESS");
                        if percent_slider(ui, &mut self.lighting.brightness, 160.0).changed() {
                            self.save_config_snapshot();
                        }
                        section_label(ui, "SPEED");
                        if percent_slider(ui, &mut self.lighting.speed, 160.0).changed() {
                            self.save_config_snapshot();
                        }
                        section_label(ui, "OPACITY");
                        if percent_slider(ui, &mut self.lighting.opacity, 160.0).changed() {
                            self.save_config_snapshot();
                        }
                        if ui
                            .checkbox(&mut self.lighting.live_when_muted, "Lights show live state")
                            .changed()
                        {
                            self.save_config_snapshot();
                            if self.lighting.live_when_muted {
                                if let Some(is_live) =
                                    self.status.as_ref().map(|status| !status.muted)
                                {
                                    self.apply_live_mute_lighting(is_live);
                                }
                            }
                        }

                        ui.add_space(8.0);
                        if ui
                            .add_sized([160.0, 26.0], egui::Button::new("Apply"))
                            .clicked()
                        {
                            self.apply_lighting_to_microphone();
                        }
                        if ui
                            .add_sized([160.0, 26.0], egui::Button::new("Stop Stream"))
                            .clicked()
                        {
                            if let Some(cancel) = &self.lighting_cancel {
                                cancel.store(true, Ordering::Relaxed);
                            }
                            self.lighting_cancel = None;
                            log_event("info", "lighting.apply.stop", &[]);
                        }
                        if ui
                            .add_sized([160.0, 26.0], egui::Button::new("Save to Mic"))
                            .on_hover_text("Experimental persistent device write")
                            .clicked()
                        {
                            self.save_lighting_to_microphone();
                        }
                    });
                });
            });
        });
    }

    fn ui_pattern_panel(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(230.0);
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
            section_label(ui, "POLAR PATTERN");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Last used:").size(12.0));
                ui.label(
                    egui::RichText::new(self.polar_pattern.label())
                        .size(12.0)
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

    fn ui_mic_stage(&mut self, ui: &mut egui::Ui, height: f32) {
        let available = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(available, height), egui::Sense::hover());
        let painter = ui.painter_at(rect);

        painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(22, 23, 23));
        let pattern_width = 160.0_f32.min(rect.width() * 0.42);
        let pattern_left = rect.right() - pattern_width - 16.0;
        let mic_area_right = pattern_left - self.stage_mic_gap.clamp(0.0, 80.0);
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

        let pattern_rect = egui::Rect::from_min_max(
            egui::pos2(pattern_left, rect.top() + 16.0),
            egui::pos2(pattern_left + pattern_width, rect.bottom() - 16.0),
        );
        if self.layout_edit {
            let drag_response = ui.interact(
                pattern_rect,
                ui.id().with("stage_pattern_panel_drag"),
                egui::Sense::drag(),
            );
            if drag_response.dragged() {
                self.stage_pattern_left_factor = (self.stage_pattern_left_factor
                    + drag_response.drag_delta().x / rect.width())
                .clamp(0.20, 0.82);
                self.save_config_snapshot();
            }
            painter.rect_stroke(
                pattern_rect.expand(3.0),
                0.0,
                egui::Stroke::new(1.0, egui::Color32::YELLOW),
                egui::StrokeKind::Outside,
            );
        }
        // Render the pattern panel positioned absolutely inside the already-allocated
        // stage rect. Using `new_child` (instead of `scope_builder`) deliberately avoids
        // advancing the parent cursor, so it does not add phantom vertical space below the
        // stage that would push the audio/lighting panels down.
        let mut pattern_ui = ui.new_child(egui::UiBuilder::new().max_rect(pattern_rect));
        self.ui_pattern_panel(&mut pattern_ui);
    }
}

impl eframe::App for MicLiteApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if TrayHandle::show_requested() {
            self.restore_from_tray(&ctx);
        }
        if TrayHandle::exit_requested() {
            self.force_exit = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            log_event("info", "tray.exit.request", &[]);
        }
        if ctx.input(|input| input.viewport().close_requested())
            && self.minimize_to_tray
            && !self.force_exit
        {
            self.ensure_tray_started();
            self.hidden_to_tray = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            log_event("info", "gui.close_to_tray", &[]);
        }
        if self.minimize_to_tray
            && !self.hidden_to_tray
            && ctx.input(|input| input.viewport().minimized == Some(true))
        {
            self.ensure_tray_started();
            self.hidden_to_tray = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            log_event("info", "gui.minimize_to_tray", &[]);
        }
        if !self.lighting_autostart_applied {
            self.lighting_autostart_applied = true;
            if self.lighting_device.is_some() {
                self.apply_lighting_to_microphone();
                log_event("info", "lighting.apply.autostart", &[]);
            }
        }
        if self.start_minimized && !self.start_minimized_applied {
            if self.minimize_to_tray {
                self.ensure_tray_started();
                self.hidden_to_tray = true;
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            self.start_minimized_applied = true;
            log_event("info", "gui.start_minimized", &[]);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(50));
        let inner_width = (ui.available_width() - 20.0).max(0.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.set_width(inner_width);
                ui.set_max_width(inner_width);
                ui.add_space(8.0);
                self.ui_top_bar(ui);
                self.ui_layout_editor(ui);
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);
                self.ui_dashboard(ui);
            });
            ui.add_space(10.0);
        });
    }
}
