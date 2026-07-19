use eframe::egui;
use std::{
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
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
        AppConfig, AudioConfig, LightingConfig, MqttConfig, UiConfig, export_config, import_config,
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
    mqtt::{MqttBridge, MqttCommand, MqttStateSnapshot, start_mqtt_runtime},
    paths::log_file_path,
    tray::TrayHandle,
};

enum LightingUiEvent {
    Applied,
    ApplyFailed(String),
    Saved,
    SaveFailed(String),
}

#[derive(Clone, Copy)]
enum NoticeSeverity {
    Good,
    Info,
    Warning,
    Error,
}

struct UiNotice {
    severity: NoticeSeverity,
    message: String,
}

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
    lighting_events: Receiver<LightingUiEvent>,
    lighting_event_sender: Sender<LightingUiEvent>,
    lighting_notice: Option<UiNotice>,
    mqtt_bridge: Option<MqttBridge>,
    mqtt_commands: Option<Receiver<MqttCommand>>,
    mqtt_config: MqttConfig,
    mqtt_settings_open: bool,
    mqtt_settings_message: Option<UiNotice>,
    mqtt_password_visible: bool,
    last_mqtt_publish: Instant,
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
    window_x: Option<f32>,
    window_y: Option<f32>,
    last_window_position_save: Instant,
}

impl MicLiteApp {
    pub(crate) fn new(start_minimized: bool, layout_edit: bool) -> Self {
        let config = load_or_create_config().unwrap_or_else(|error| {
            log_event("error", "config.load.error", &[("message", error)]);
            AppConfig::default()
        });
        let (lighting_event_sender, lighting_events) = mpsc::channel();
        let colors = config
            .lighting
            .colors
            .iter()
            .filter_map(|color| parse_rgb_hex(color).ok())
            .map(|rgb| egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
            .collect::<Vec<_>>();
        let mqtt_runtime = start_mqtt_runtime(&config.mqtt);
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
                split_layers: config.lighting.split_layers,
                top_effect: Effect::from_config(&config.lighting.top_effect),
                bottom_effect: Effect::from_config(&config.lighting.bottom_effect),
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
            lighting_events,
            lighting_event_sender,
            lighting_notice: None,
            mqtt_bridge: mqtt_runtime.as_ref().map(|runtime| runtime.bridge.clone()),
            mqtt_commands: mqtt_runtime.map(|runtime| runtime.commands),
            mqtt_config: config.mqtt.clone(),
            mqtt_settings_open: false,
            mqtt_settings_message: None,
            mqtt_password_visible: false,
            last_mqtt_publish: Instant::now(),
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
            window_x: config.ui.window_x,
            window_y: config.ui.window_y,
            last_window_position_save: Instant::now(),
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
        let config = self.current_config_snapshot();
        if let Err(error) = save_config(&config) {
            log_event("error", "config.save.error", &[("message", error)]);
        }
    }

    fn current_config_snapshot(&self) -> AppConfig {
        // Service/device settings are edited outside the GUI (e.g. the `service`
        // CLI), so read the file once here to carry them over instead of caching
        // stale values from startup.
        let stored = load_or_create_config().unwrap_or_default();
        AppConfig {
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
                split_layers: self.lighting.split_layers,
                top_effect: self.lighting.top_effect.as_config().to_string(),
                bottom_effect: self.lighting.bottom_effect.as_config().to_string(),
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
                window_x: self.window_x,
                window_y: self.window_y,
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
            service: stored.service,
            device: stored.device,
            mqtt: self.mqtt_config.clone(),
        }
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

    fn remember_window_position(&mut self, ctx: &egui::Context) {
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

    fn drain_lighting_events(&mut self) {
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

    fn drain_mqtt_commands(&mut self) {
        let mut commands = Vec::new();
        if let Some(receiver) = &self.mqtt_commands {
            while let Ok(command) = receiver.try_recv() {
                commands.push(command);
            }
        }
        for command in commands {
            self.apply_mqtt_command(command);
        }
    }

    fn apply_mqtt_command(&mut self, command: MqttCommand) {
        log_event(
            "info",
            "mqtt.command",
            &[("command", format!("{command:?}"))],
        );
        match command {
            MqttCommand::SetMute(muted) => self.set_mute(muted),
            MqttCommand::ToggleMute => {
                let muted = self.is_muted();
                self.set_mute(!muted);
            }
            MqttCommand::SetMicVolume(value) => {
                self.mic_volume = value;
                self.set_volume();
                self.save_config_snapshot();
            }
            MqttCommand::SetMonitoringVolume(value) => {
                self.mic_monitoring = value;
                self.set_mic_monitoring();
                self.save_config_snapshot();
            }
            MqttCommand::SetHeadphoneVolume(value) => {
                self.headphone_volume = value;
                self.set_headphone_volume();
                self.save_config_snapshot();
            }
            MqttCommand::SetEffect(effect) => {
                self.lighting.effect = effect;
                self.save_config_snapshot();
                self.apply_lighting_to_microphone();
            }
            MqttCommand::SetTarget(target) => {
                self.lighting.target = target;
                self.save_config_snapshot();
                self.apply_lighting_to_microphone();
            }
            MqttCommand::SetBrightness(value) => {
                self.lighting.brightness = value;
                self.save_config_snapshot();
                self.apply_lighting_to_microphone();
            }
            MqttCommand::SetSpeed(value) => {
                self.lighting.speed = value;
                self.save_config_snapshot();
                self.apply_lighting_to_microphone();
            }
            MqttCommand::SetOpacity(value) => {
                self.lighting.opacity = value;
                self.save_config_snapshot();
                self.apply_lighting_to_microphone();
            }
            MqttCommand::SetLiveWhenMuted(enabled) => {
                self.lighting.live_when_muted = enabled;
                self.save_config_snapshot();
            }
            MqttCommand::ApplyLighting => self.apply_lighting_to_microphone(),
            MqttCommand::StopLighting => {
                if let Some(cancel) = &self.lighting_cancel {
                    cancel.store(true, Ordering::Relaxed);
                }
                self.lighting_cancel = None;
                self.lighting_notice = Some(UiNotice {
                    severity: NoticeSeverity::Info,
                    message: "Lighting stream stopped from MQTT.".to_string(),
                });
            }
            MqttCommand::SaveLighting => self.save_lighting_to_microphone(),
        }
        self.publish_mqtt_state();
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

    fn refresh_devices(&mut self) {
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

    fn refresh_status_periodic(&mut self) {
        if self.last_status_update.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_status_update = Instant::now();
        self.refresh_status();
    }

    fn publish_mqtt_state_periodic(&mut self) {
        if self.last_mqtt_publish.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.publish_mqtt_state();
    }

    fn publish_mqtt_state(&mut self) {
        self.last_mqtt_publish = Instant::now();
        let Some(bridge) = &self.mqtt_bridge else {
            return;
        };
        let (available, device_name, device_state, muted, mic_volume) =
            if let Some(status) = &self.status {
                (
                    status.device.state == "active",
                    status.device.name.clone(),
                    status.device.state.clone(),
                    self.is_muted(),
                    status.volume,
                )
            } else {
                (
                    false,
                    "Unavailable".to_string(),
                    self.status_error
                        .clone()
                        .unwrap_or_else(|| "unavailable".to_string()),
                    self.is_muted(),
                    self.mic_volume,
                )
            };
        bridge.publish_state(&MqttStateSnapshot {
            available,
            device_name,
            device_state,
            muted,
            mic_volume,
            mic_monitoring: self.mic_monitoring,
            headphone_volume: self.headphone_volume,
            input_level_percent: self.input_peak.sqrt().clamp(0.0, 1.0) * 100.0,
            polar_pattern: self.polar_pattern.as_config().to_string(),
            lighting_available: self.lighting_device.is_some(),
            effect: self.lighting.effect.as_config().to_string(),
            target: self.lighting.target.as_config().to_string(),
            brightness: self.lighting.brightness,
            speed: self.lighting.speed,
            opacity: self.lighting.opacity,
            live_when_muted: self.lighting.live_when_muted,
        });
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
            self.lighting_notice = Some(UiNotice {
                severity: NoticeSeverity::Warning,
                message:
                    "Lighting controller not detected. Connect the QuadCast S and close NGENUITY."
                        .to_string(),
            });
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
            split_layers: self.lighting.split_layers,
            top_effect: self.lighting.top_effect,
            bottom_effect: self.lighting.bottom_effect,
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
                ("split_layers", program.split_layers.to_string()),
                ("top_effect", program.top_effect.as_config().to_string()),
                (
                    "bottom_effect",
                    program.bottom_effect.as_config().to_string(),
                ),
            ],
        );

        if let Some(cancel) = &self.lighting_cancel {
            cancel.store(true, Ordering::Relaxed);
            thread::sleep(Duration::from_millis(90));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.lighting_cancel = Some(cancel.clone());
        self.lighting_notice = Some(UiNotice {
            severity: NoticeSeverity::Info,
            message: format!("Applying {} lighting.", program.effect.label()),
        });
        let sender = self.lighting_event_sender.clone();

        thread::spawn(move || {
            match stream_lighting_program_cancelable(
                &program,
                StreamDuration::Forever,
                Some(cancel),
                false,
            ) {
                Ok(()) => {
                    let _ = sender.send(LightingUiEvent::Applied);
                    log_event(
                        "info",
                        "lighting.apply.done",
                        &[("effect", program.effect.as_config().to_string())],
                    );
                }
                Err(error) => {
                    let _ = sender.send(LightingUiEvent::ApplyFailed(error.clone()));
                    log_event("error", "lighting.apply.error", &[("message", error)]);
                }
            }
        });
    }

    fn save_lighting_to_microphone(&mut self) {
        if self.lighting_device.is_none() {
            log_event("warn", "lighting.save.no_device", &[]);
            self.lighting_notice = Some(UiNotice {
                severity: NoticeSeverity::Warning,
                message: "Lighting controller not detected. Persistent save is unavailable."
                    .to_string(),
            });
            return;
        }
        self.lighting_notice = Some(UiNotice {
            severity: NoticeSeverity::Info,
            message: "Saving lighting to microphone memory.".to_string(),
        });
        let sender = self.lighting_event_sender.clone();
        thread::spawn(move || match save_lighting_to_microphone(false) {
            Ok(()) => {
                let _ = sender.send(LightingUiEvent::Saved);
                log_event("info", "lighting.save.done", &[]);
            }
            Err(error) => {
                let _ = sender.send(LightingUiEvent::SaveFailed(error.clone()));
                log_event("error", "lighting.save.error", &[("message", error)]);
            }
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

    fn ui_mqtt_settings_window(&mut self, ctx: &egui::Context) {
        if !self.mqtt_settings_open {
            return;
        }

        let mut open = self.mqtt_settings_open;
        egui::Window::new("MQTT Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        section_label(ui, "STATUS");
                        ui.add_space(8.0);
                        let (text, color) = if !self.mqtt_config.enabled {
                            ("Disabled", notice_color(NoticeSeverity::Info))
                        } else if self.mqtt_bridge.is_some() {
                            ("Runtime active", notice_color(NoticeSeverity::Good))
                        } else {
                            ("Enabled; restart needed or startup failed", notice_color(NoticeSeverity::Warning))
                        };
                        ui.colored_label(color, text);
                    });
                    ui.small("Connection changes are saved immediately, but the MQTT runtime starts on app launch. Restart the GUI after changing broker settings.");
                    ui.add_space(8.0);

                    ui.checkbox(&mut self.mqtt_config.enabled, "Enable MQTT integration");
                    ui.checkbox(
                        &mut self.mqtt_config.home_assistant_discovery,
                        "Publish Home Assistant discovery",
                    );
                    ui.checkbox(&mut self.mqtt_config.retain_state, "Retain state topics");
                    ui.checkbox(&mut self.mqtt_config.clean_session, "Clean session");

                    ui.add_space(8.0);
                    section_label(ui, "BROKER");
                    labeled_text_edit(ui, "URL", &mut self.mqtt_config.url, false);
                    labeled_text_edit(ui, "Client ID", &mut self.mqtt_config.client_id, false);
                    ui.horizontal(|ui| {
                        ui.set_width(500.0);
                        ui.add_sized([115.0, 20.0], egui::Label::new("Username"));
                        let username = self.mqtt_config.username.get_or_insert_with(String::new);
                        ui.add_sized([340.0, 20.0], egui::TextEdit::singleline(username));
                    });
                    ui.horizontal(|ui| {
                        ui.set_width(500.0);
                        ui.add_sized([115.0, 20.0], egui::Label::new("Password"));
                        let password = self.mqtt_config.password.get_or_insert_with(String::new);
                        ui.add_sized(
                            [255.0, 20.0],
                            egui::TextEdit::singleline(password)
                                .password(!self.mqtt_password_visible),
                        );
                        ui.checkbox(&mut self.mqtt_password_visible, "show");
                    });

                    ui.add_space(8.0);
                    section_label(ui, "TOPICS");
                    labeled_text_edit(ui, "Base topic", &mut self.mqtt_config.base_topic, false);
                    labeled_text_edit(
                        ui,
                        "Discovery prefix",
                        &mut self.mqtt_config.discovery_prefix,
                        false,
                    );

                    ui.add_space(8.0);
                    section_label(ui, "SESSION");
                    ui.horizontal(|ui| {
                        ui.add_sized([115.0, 20.0], egui::Label::new("QoS"));
                        ui.add(
                            egui::DragValue::new(&mut self.mqtt_config.qos)
                                .range(0..=2)
                                .speed(1),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.add_sized([115.0, 20.0], egui::Label::new("Keep alive"));
                        ui.add(
                            egui::DragValue::new(&mut self.mqtt_config.keep_alive_secs)
                                .range(5..=3600)
                                .speed(5),
                        );
                        ui.label("seconds");
                    });

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            self.normalize_mqtt_config();
                            match self.save_config_result() {
                                Ok(()) => {
                                    self.mqtt_settings_message = Some(UiNotice {
                                        severity: NoticeSeverity::Good,
                                        message: "MQTT settings saved. Restart the GUI to reconnect."
                                            .to_string(),
                                    });
                                    log_event("info", "mqtt.settings.save", &[]);
                                }
                                Err(error) => {
                                    self.mqtt_settings_message = Some(UiNotice {
                                        severity: NoticeSeverity::Error,
                                        message: error,
                                    });
                                }
                            }
                        }
                        if ui.button("Reset defaults").clicked() {
                            self.mqtt_config = MqttConfig::default();
                            self.mqtt_settings_message = Some(UiNotice {
                                severity: NoticeSeverity::Info,
                                message: "Defaults loaded. Click Save to persist them.".to_string(),
                            });
                        }
                        if ui.button("Close").clicked() {
                            self.mqtt_settings_open = false;
                        }
                    });
                    if let Some(message) = &self.mqtt_settings_message {
                        ui.add_space(6.0);
                        ui.colored_label(notice_color(message.severity), &message.message);
                    }
                });
            });
        self.mqtt_settings_open = open && self.mqtt_settings_open;
    }

    fn normalize_mqtt_config(&mut self) {
        self.mqtt_config.url = self.mqtt_config.url.trim().to_string();
        self.mqtt_config.client_id = self.mqtt_config.client_id.trim().to_string();
        self.mqtt_config.base_topic = self
            .mqtt_config
            .base_topic
            .trim()
            .trim_matches('/')
            .to_string();
        self.mqtt_config.discovery_prefix = self
            .mqtt_config
            .discovery_prefix
            .trim()
            .trim_matches('/')
            .to_string();
        self.mqtt_config.username = self
            .mqtt_config
            .username
            .take()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.mqtt_config.password = self
            .mqtt_config
            .password
            .take()
            .filter(|value| !value.is_empty());
        self.mqtt_config.qos = self.mqtt_config.qos.min(2);
        self.mqtt_config.keep_alive_secs = self.mqtt_config.keep_alive_secs.clamp(5, 3600);
    }

    fn save_config_result(&self) -> Result<(), String> {
        let config = self.current_config_snapshot();
        save_config(&config)
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

                        ui.add_space(10.0);
                        if ui
                            .checkbox(&mut self.lighting.split_layers, "Split top/bottom")
                            .changed()
                        {
                            self.save_config_snapshot();
                        }
                        if self.lighting.split_layers {
                            self.ui_layer_effect_picker(ui, "Top", true);
                            self.ui_layer_effect_picker(ui, "Bottom", false);
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
                                let popup_id = response.id.with("color_picker");
                                if response.double_clicked() {
                                    self.lighting.selected_color = index;
                                    egui::Popup::open_id(ui.ctx(), popup_id);
                                    self.save_config_snapshot();
                                } else if response.clicked() {
                                    self.lighting.selected_color = index;
                                    self.save_config_snapshot();
                                }

                                let mut color_changed = false;
                                egui::Popup::from_response(&response)
                                    .id(popup_id)
                                    .open_memory(None)
                                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                                    .show(|ui| {
                                        ui.spacing_mut().slider_width = 275.0;
                                        color_changed = egui::color_picker::color_picker_color32(
                                            ui,
                                            &mut self.lighting.colors[index],
                                            egui::color_picker::Alpha::Opaque,
                                        );
                                    });
                                if color_changed {
                                    self.lighting.selected_color = index;
                                    self.save_config_snapshot();
                                }
                            }
                        });

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
                        if let Some(notice) = &self.lighting_notice {
                            ui.add_space(6.0);
                            ui.colored_label(notice_color(notice.severity), &notice.message);
                        } else if self.lighting_device.is_none() {
                            ui.add_space(6.0);
                            ui.colored_label(
                                notice_color(NoticeSeverity::Warning),
                                "Lighting controls need the QuadCast S HID interface.",
                            );
                        }
                    });
                });
            });
        });
    }

    fn ui_layer_effect_picker(&mut self, ui: &mut egui::Ui, label: &str, top: bool) {
        let mut value = if top {
            self.lighting.top_effect
        } else {
            self.lighting.bottom_effect
        };
        ui.horizontal(|ui| {
            ui.add_sized([46.0, 20.0], egui::Label::new(label));
            egui::ComboBox::from_id_salt(("layer_effect", label))
                .selected_text(value.label())
                .width(92.0)
                .show_ui(ui, |ui| {
                    for effect in [
                        Effect::Solid,
                        Effect::Wave,
                        Effect::Cycle,
                        Effect::Pulse,
                        Effect::Blink,
                        Effect::Lightning,
                    ] {
                        ui.selectable_value(&mut value, effect, effect.label());
                    }
                });
        });
        if top && value != self.lighting.top_effect {
            self.lighting.top_effect = value;
            self.save_config_snapshot();
        } else if !top && value != self.lighting.bottom_effect {
            self.lighting.bottom_effect = value;
            self.save_config_snapshot();
        }
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

        let mut notice_y = rect.top() + 42.0;
        for notice in self.device_status_notices().into_iter().take(4) {
            painter.text(
                rect.left_top() + egui::vec2(16.0, notice_y - rect.top()),
                egui::Align2::LEFT_TOP,
                notice.message,
                egui::FontId::proportional(12.0),
                notice_color(notice.severity),
            );
            notice_y += 16.0;
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

fn is_expected_hyperx_device_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("hyperx") || name.contains("quadcast") || name.contains("kingston")
}

fn notice_color(severity: NoticeSeverity) -> egui::Color32 {
    match severity {
        NoticeSeverity::Good => egui::Color32::from_rgb(118, 220, 150),
        NoticeSeverity::Info => egui::Color32::from_rgb(150, 190, 235),
        NoticeSeverity::Warning => egui::Color32::from_rgb(255, 195, 95),
        NoticeSeverity::Error => egui::Color32::from_rgb(255, 120, 120),
    }
}

fn labeled_text_edit(ui: &mut egui::Ui, label: &str, value: &mut String, password: bool) {
    ui.horizontal(|ui| {
        ui.set_width(500.0);
        ui.add_sized([115.0, 20.0], egui::Label::new(label));
        ui.add_sized(
            [340.0, 20.0],
            egui::TextEdit::singleline(value).password(password),
        );
    });
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
        self.ui_mqtt_settings_window(&ctx);
        self.remember_window_position(&ctx);
    }
}
