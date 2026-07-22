mod audio_panel;
mod lighting_panel;
mod mic_stage;
mod mqtt_panel;
mod shell;

use eframe::egui;
use std::{
    sync::{
        Arc,
        atomic::AtomicBool,
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

use crate::{
    audio::{AudioPeakMonitor, start_audio_peak_monitor},
    config::{
        AppConfig, AudioConfig, LightingConfig, MqttConfig, UiConfig, load_or_create_config,
        save_config,
    },
    constants::CONFIG_SCHEMA_VERSION,
    lighting::{
        LightingState, color_to_hex, detect_lighting_device, parse_rgb_hex,
        spawn_hid_event_listener,
    },
    logging::log_event,
    model::{Effect, HidEvent, LightTarget, LightingDevice, MicStatus, PolarPattern, Tab},
    mqtt::{MqttBridge, MqttCommand, MqttStateSnapshot, start_mqtt_runtime},
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
    last_mqtt_input_level_muted: Option<bool>,
    last_mqtt_state: Option<MqttStateSnapshot>,
    lighting_autostart_applied: bool,
    minimize_to_tray: bool,
    hidden_to_tray: bool,
    force_exit: bool,
    tray_handle: Option<TrayHandle>,
    start_minimized: bool,
    start_minimized_applied: bool,
    window_x: Option<f32>,
    window_y: Option<f32>,
    last_window_position_save: Instant,
    device_notices: Vec<UiNotice>,
    device_notices_key: u64,
}

impl MicLiteApp {
    pub(crate) fn new(start_minimized: bool) -> Self {
        let config = load_or_create_config().unwrap_or_else(|error| {
            log_event(
                "error",
                "config.load.error",
                &[("message", error.to_string())],
            );
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
            last_mqtt_input_level_muted: None,
            last_mqtt_state: None,
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
            window_x: config.ui.window_x,
            window_y: config.ui.window_y,
            last_window_position_save: Instant::now(),
            device_notices: Vec::new(),
            device_notices_key: 0,
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
            log_event(
                "error",
                "config.save.error",
                &[("message", error.to_string())],
            );
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
            },
            service: stored.service,
            device: stored.device,
            mqtt: self.mqtt_config.clone(),
        }
    }

    fn save_config_result(&self) -> Result<(), String> {
        let config = self.current_config_snapshot();
        save_config(&config).map_err(|error| error.to_string())
    }

    fn is_muted(&self) -> bool {
        self.status.as_ref().is_some_and(|status| status.muted) || self.hid_muted
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
        // Tick slower while hidden in the tray: only tray/MQTT/HID plumbing needs
        // servicing there, and 4 Hz keeps those responsive at a fraction of the CPU.
        let repaint_delay = if self.hidden_to_tray {
            Duration::from_millis(250)
        } else {
            Duration::from_millis(50)
        };
        ui.ctx().request_repaint_after(repaint_delay);
        let inner_width = (ui.available_width() - 20.0).max(0.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.set_width(inner_width);
                ui.set_max_width(inner_width);
                ui.add_space(8.0);
                self.ui_top_bar(ui);
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
