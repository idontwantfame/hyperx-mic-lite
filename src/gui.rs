use eframe::egui;
use std::{
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
        AppConfig, AudioConfig, LightingConfig, UiConfig, load_or_create_config, save_config,
    },
    constants::CONFIG_SCHEMA_VERSION,
    gui_widgets::{
        color_swatch, draw_microphone, pattern_tile, percent_slider, section_label, target_button,
    },
    lighting::{
        LightingProgram, LightingState, StreamDuration, color_to_hex, detect_lighting_device,
        live_mute_lighting_color, parse_rgb_hex, pattern_description, save_lighting_to_microphone,
        spawn_hid_event_listener, stream_lighting_program_cancelable, write_solid_lighting_once,
    },
    logging::log_event,
    model::{Effect, HidEvent, LightTarget, LightingDevice, MicStatus, PolarPattern, Tab},
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
    input_peak: f32,
    input_monitor: Option<AudioPeakMonitor>,
    last_peak_update: Instant,
    last_status_update: Instant,
    polar_pattern: PolarPattern,
    hid_events: Receiver<HidEvent>,
    lighting: LightingState,
    lighting_device: Option<LightingDevice>,
    lighting_message: String,
    lighting_cancel: Option<Arc<AtomicBool>>,
    lighting_autostart_applied: bool,
    minimize_to_tray: bool,
    hidden_to_tray: bool,
    force_exit: bool,
    tray_handle: Option<TrayHandle>,
    start_minimized: bool,
    start_minimized_applied: bool,
}

impl MicLiteApp {
    pub(crate) fn new(start_minimized: bool) -> Self {
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
            lighting_message: String::new(),
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
        };
        app.refresh_status();
        if app.mute_on_app_start {
            app.set_mute(true);
        }
        if app.lighting.selected_color >= app.lighting.colors.len() {
            app.lighting.selected_color = 0;
        }
        app.lighting_message = match &app.lighting_device {
            Some(device) => format!(
                "Detected {:04x}:{:04x} interface {} usage {:04x}:{:04x}. Packet writer is next.",
                device.vendor_id,
                device.product_id,
                device.interface_number,
                device.usage_page,
                device.usage
            ),
            None => "No supported QuadCast S lighting HID interface detected.".to_string(),
        };
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
                    if let Some(status) = &mut self.status {
                        status.muted = !is_live;
                    }
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
        self.lighting_message = if is_live {
            "Showing live microphone lighting.".to_string()
        } else {
            "Showing muted microphone lighting.".to_string()
        };
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
            self.lighting_message = "No supported lighting interface is available.".to_string();
            log_event("warn", "lighting.apply.no_device", &[]);
            return;
        }

        let program = LightingProgram {
            effect: self.lighting.effect,
            target: self.lighting.target,
            colors: self
                .lighting
                .colors
                .iter()
                .map(|color| [color.r(), color.g(), color.b()])
                .collect(),
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
        self.lighting_message = format!(
            "Applying {} to microphone. It will keep running while this app is open.",
            program.effect.label(),
        );
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
            self.lighting_message = "No supported lighting interface is available.".to_string();
            log_event("warn", "lighting.save.no_device", &[]);
            return;
        }
        self.lighting_message =
            "Saving current microphone lighting to device memory...".to_string();
        thread::spawn(move || match save_lighting_to_microphone(false) {
            Ok(()) => log_event("info", "lighting.save.done", &[]),
            Err(error) => log_event("error", "lighting.save.error", &[("message", error)]),
        });
    }

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("HyperX QuadCast S");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Refresh").clicked() {
                    self.refresh_status();
                }
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
            });
        });
    }

    fn ui_dashboard(&mut self, ui: &mut egui::Ui) {
        self.drain_hid_events();
        self.refresh_status_periodic();
        self.refresh_input_peak();
        ui.allocate_ui(egui::vec2(ui.available_width(), 250.0), |ui| {
            self.ui_mic_stage(ui);
        });
        ui.separator();
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
            ui.allocate_ui(egui::vec2(285.0, 320.0), |ui| {
                self.ui_audio_panel(ui);
            });
            ui.add_space(18.0);
            ui.allocate_ui(egui::vec2(590.0, 360.0), |ui| {
                self.ui_lighting_panel(ui);
            });
        });
    }

    fn ui_audio_panel(&mut self, ui: &mut egui::Ui) {
        let muted = self.status.as_ref().is_some_and(|status| status.muted);
        ui.set_min_width(260.0);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.vertical(|ui| {
                section_label(ui, "AUDIO");
                ui.add_space(4.0);
                section_label(ui, "MIC VOLUME");
                if percent_slider(ui, &mut self.mic_volume, 210.0).changed() {
                    self.set_volume();
                    self.save_config_snapshot();
                }

                ui.add_space(10.0);
                section_label(ui, "INPUT LEVEL");
                let display_peak = self.input_peak.sqrt().clamp(0.0, 1.0);
                ui.add(
                    egui::ProgressBar::new(display_peak)
                        .desired_width(245.0)
                        .text(format!("{:.1}%", display_peak * 100.0)),
                );
                ui.small("Bottom dial controls hardware gain.");

                ui.add_space(10.0);
                section_label(ui, "MIC MONITORING");
                if percent_slider(ui, &mut self.mic_monitoring, 210.0).changed() {
                    self.set_mic_monitoring();
                    self.save_config_snapshot();
                }

                ui.add_space(10.0);
                section_label(ui, "HEADPHONE VOLUME");
                if percent_slider(ui, &mut self.headphone_volume, 210.0).changed() {
                    self.set_headphone_volume();
                    self.save_config_snapshot();
                }

                ui.add_space(10.0);
                let button_text = if muted {
                    "Unmute microphone"
                } else {
                    "Mute microphone"
                };
                if ui
                    .add_sized([180.0, 28.0], egui::Button::new(button_text))
                    .clicked()
                {
                    self.set_mute(!muted);
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

                if let Some(error) = &self.status_error {
                    ui.add_space(8.0);
                    ui.colored_label(egui::Color32::from_rgb(255, 120, 120), error);
                }
            });
        });
    }

    fn ui_lighting_panel(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(560.0);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                ui.allocate_ui(egui::vec2(200.0, 340.0), |ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        section_label(ui, "LIGHTING");
                        ui.add_space(4.0);
                        section_label(ui, "EFFECTS");
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

                ui.add_space(18.0);
                ui.allocate_ui(egui::vec2(330.0, 340.0), |ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        section_label(ui, "COLOR");
                        ui.horizontal_wrapped(|ui| {
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
                        if percent_slider(ui, &mut self.lighting.brightness, 210.0).changed() {
                            self.save_config_snapshot();
                        }
                        section_label(ui, "SPEED");
                        if percent_slider(ui, &mut self.lighting.speed, 210.0).changed() {
                            self.save_config_snapshot();
                        }
                        section_label(ui, "OPACITY");
                        if percent_slider(ui, &mut self.lighting.opacity, 210.0).changed() {
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
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add_sized([150.0, 28.0], egui::Button::new("Apply"))
                                .clicked()
                            {
                                self.apply_lighting_to_microphone();
                            }
                            if ui
                                .add_sized([150.0, 28.0], egui::Button::new("Save to Mic"))
                                .on_hover_text("Experimental persistent device write")
                                .clicked()
                            {
                                self.save_lighting_to_microphone();
                            }
                        });
                        if ui
                            .add_sized([150.0, 28.0], egui::Button::new("Stop Stream"))
                            .clicked()
                        {
                            if let Some(cancel) = &self.lighting_cancel {
                                cancel.store(true, Ordering::Relaxed);
                            }
                            self.lighting_cancel = None;
                            self.lighting_message = "Lighting stream stopped.".to_string();
                            log_event("info", "lighting.apply.stop", &[]);
                        }
                        if let Some(device) = &self.lighting_device {
                            ui.small(format!("{} {}", device.manufacturer, device.product));
                        }
                        ui.label(&self.lighting_message);
                    });
                });
            });
        });
    }

    fn ui_pattern_panel(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(230.0);
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    section_label(ui, "POLAR PATTERN");
                    ui.add_space(150.0);
                    ui.small("Last used");
                    ui.strong(self.polar_pattern.label());
                    ui.small(pattern_description(self.polar_pattern));
                });
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    pattern_tile(ui, PolarPattern::Stereo, self.polar_pattern);
                    pattern_tile(ui, PolarPattern::Omni, self.polar_pattern);
                    pattern_tile(ui, PolarPattern::Cardioid, self.polar_pattern);
                    pattern_tile(ui, PolarPattern::Bidirectional, self.polar_pattern);
                });
            });
        });
    }

    fn ui_mic_stage(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_width();
        let height = ui.available_height().clamp(190.0, 290.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(available, height), egui::Sense::hover());
        let painter = ui.painter_at(rect);

        painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(22, 23, 23));
        let pattern_width = 235.0_f32.min(rect.width() * 0.29).max(205.0);
        let desired_pattern_left = rect.left() + rect.width() * 0.69;
        let min_pattern_left = rect.left() + rect.width() * 0.52;
        let max_pattern_left = rect.right() - pattern_width - 16.0;
        let pattern_left = desired_pattern_left.clamp(min_pattern_left, max_pattern_left);
        let mic_area_right = pattern_left - 18.0;
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

        if let Some(status) = &self.status {
            let text = format!(
                "{} | {}% | {} | {}",
                status.device.name,
                status.volume,
                if status.muted { "Muted" } else { "Live" },
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
            egui::pos2(pattern_left, rect.top() + 10.0),
            egui::pos2(pattern_left + pattern_width, rect.bottom() - 10.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(pattern_rect), |ui| {
            self.ui_pattern_panel(ui);
        });
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
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.add_space(8.0);
                self.ui_top_bar(ui);
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);
                self.ui_dashboard(ui);
            });
            ui.add_space(10.0);
        });
    }
}
