use eframe::egui;
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use crate::{
    config::MqttConfig,
    gui_widgets::section_label,
    logging::log_event,
    mqtt::{MqttCommand, MqttStateSnapshot},
};

use super::{MicLiteApp, NoticeSeverity, UiNotice, notice_color};

impl MicLiteApp {
    pub(super) fn drain_mqtt_commands(&mut self) {
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

    pub(super) fn publish_mqtt_state_periodic(&mut self) {
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

    pub(super) fn ui_mqtt_settings_window(&mut self, ctx: &egui::Context) {
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
