use std::{
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use rumqttc::{Client, Event, Incoming, LastWill, MqttOptions, QoS};
use serde::Serialize;
use serde_json::json;

use crate::{
    config::MqttConfig,
    logging::log_event,
    model::{Effect, LightTarget},
};

const MQTT_COMMAND_QUEUE_LIMIT: usize = 32;

#[derive(Debug)]
pub(crate) enum MqttCommand {
    SetMute(bool),
    ToggleMute,
    SetMicVolume(u8),
    SetMonitoringVolume(u8),
    SetHeadphoneVolume(u8),
    SetEffect(Effect),
    SetTarget(LightTarget),
    SetBrightness(u8),
    SetSpeed(u8),
    SetOpacity(u8),
    SetLiveWhenMuted(bool),
    ApplyLighting,
    StopLighting,
    SaveLighting,
}

#[derive(Clone)]
pub(crate) struct MqttBridge {
    client: Client,
    topics: MqttTopics,
    qos: QoS,
    retain_state: bool,
}

pub(crate) struct MqttRuntime {
    pub(crate) bridge: MqttBridge,
    pub(crate) commands: Receiver<MqttCommand>,
}

#[derive(Clone)]
struct MqttTopics {
    base: String,
    discovery_prefix: String,
}

#[derive(Serialize)]
pub(crate) struct MqttStateSnapshot {
    pub(crate) available: bool,
    pub(crate) device_name: String,
    pub(crate) device_state: String,
    pub(crate) muted: bool,
    pub(crate) mic_volume: u8,
    pub(crate) mic_monitoring: u8,
    pub(crate) headphone_volume: u8,
    pub(crate) input_level_percent: f32,
    pub(crate) polar_pattern: String,
    pub(crate) lighting_available: bool,
    pub(crate) effect: String,
    pub(crate) target: String,
    pub(crate) brightness: u8,
    pub(crate) speed: u8,
    pub(crate) opacity: u8,
    pub(crate) live_when_muted: bool,
}

pub(crate) fn start_mqtt_runtime(config: &MqttConfig) -> Option<MqttRuntime> {
    if !config.enabled {
        return None;
    }

    let topics = MqttTopics {
        base: trim_topic(&config.base_topic),
        discovery_prefix: trim_topic(&config.discovery_prefix),
    };
    let qos = qos_from_u8(config.qos);
    let url = mqtt_url_with_client_id(&config.url, &config.client_id);
    let mut options = match MqttOptions::parse_url(&url) {
        Ok(options) => options,
        Err(error) => {
            log_event(
                "error",
                "mqtt.config.url.error",
                &[("message", error.to_string())],
            );
            return None;
        }
    };
    options.set_client_id(config.client_id.clone());
    options.set_keep_alive(Duration::from_secs(config.keep_alive_secs));
    options.set_clean_session(config.clean_session);
    if let Some(username) = config.username.as_ref().filter(|value| !value.is_empty()) {
        options.set_credentials(username, config.password.clone().unwrap_or_default());
    }
    options.set_last_will(LastWill::new(topics.availability(), "offline", qos, true));

    let (client, mut connection) = Client::new(options, 64);
    let bridge = MqttBridge {
        client: client.clone(),
        topics: topics.clone(),
        qos,
        retain_state: config.retain_state,
    };
    // Bounded so a flooding publisher cannot grow the queue without limit; the GUI
    // drains it every frame, so 32 pending commands is already an abnormal backlog.
    let (sender, receiver) = mpsc::sync_channel(MQTT_COMMAND_QUEUE_LIMIT);
    let discovery_enabled = config.home_assistant_discovery;

    thread::spawn(move || {
        if let Err(error) = client.subscribe(topics.command_wildcard(), qos) {
            log_event(
                "error",
                "mqtt.subscribe.error",
                &[("message", error.to_string())],
            );
            return;
        }
        if discovery_enabled {
            publish_discovery(&client, &topics, qos);
        }
        if let Err(error) = client.publish(topics.availability(), qos, true, "online") {
            log_event(
                "warn",
                "mqtt.publish.error",
                &[("message", error.to_string())],
            );
        }
        log_event(
            "info",
            "mqtt.connected",
            &[("base_topic", topics.base.clone())],
        );

        for event in connection.iter() {
            match event {
                Ok(Event::Incoming(Incoming::Publish(publish))) => {
                    if let Some(command) = parse_command(
                        &topics,
                        &publish.topic,
                        &String::from_utf8_lossy(&publish.payload),
                    ) {
                        if let Err(error) = sender.try_send(command) {
                            log_event(
                                "warn",
                                "mqtt.command.dropped",
                                &[("message", error.to_string())],
                            );
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => log_event(
                    "error",
                    "mqtt.connection.error",
                    &[("message", error.to_string())],
                ),
            }
        }
        log_event("warn", "mqtt.connection.closed", &[]);
    });

    Some(MqttRuntime {
        bridge,
        commands: receiver,
    })
}

impl MqttBridge {
    pub(crate) fn publish_state(&self, state: &MqttStateSnapshot) {
        publish_json(
            &self.client,
            self.topics.state("json"),
            self.qos,
            self.retain_state,
            state,
        );
        self.publish_value(
            "availability",
            if state.available { "online" } else { "offline" },
            true,
        );
        self.publish_value("device_name", state.device_name.as_str(), true);
        self.publish_value("device_state", state.device_state.as_str(), true);
        self.publish_value(
            "mute",
            if state.muted { "ON" } else { "OFF" },
            self.retain_state,
        );
        self.publish_value(
            "mic_volume",
            state.mic_volume.to_string(),
            self.retain_state,
        );
        self.publish_value(
            "mic_monitoring",
            state.mic_monitoring.to_string(),
            self.retain_state,
        );
        self.publish_value(
            "headphone_volume",
            state.headphone_volume.to_string(),
            self.retain_state,
        );
        self.publish_value(
            "input_level",
            format!("{:.1}", state.input_level_percent),
            false,
        );
        self.publish_value(
            "polar_pattern",
            state.polar_pattern.as_str(),
            self.retain_state,
        );
        self.publish_value(
            "lighting_available",
            if state.lighting_available {
                "ON"
            } else {
                "OFF"
            },
            self.retain_state,
        );
        self.publish_value("effect", state.effect.as_str(), self.retain_state);
        self.publish_value("target", state.target.as_str(), self.retain_state);
        self.publish_value(
            "brightness",
            state.brightness.to_string(),
            self.retain_state,
        );
        self.publish_value("speed", state.speed.to_string(), self.retain_state);
        self.publish_value("opacity", state.opacity.to_string(), self.retain_state);
        self.publish_value(
            "live_when_muted",
            if state.live_when_muted { "ON" } else { "OFF" },
            self.retain_state,
        );
    }

    fn publish_value<V: Into<Vec<u8>>>(&self, key: &str, value: V, retain: bool) {
        if let Err(error) = self
            .client
            .publish(self.topics.state(key), self.qos, retain, value)
        {
            log_event(
                "warn",
                "mqtt.publish.error",
                &[("message", error.to_string())],
            );
        }
    }
}

fn publish_json<T: Serialize>(client: &Client, topic: String, qos: QoS, retain: bool, value: &T) {
    match serde_json::to_vec(value) {
        Ok(payload) => {
            if let Err(error) = client.publish(topic, qos, retain, payload) {
                log_event(
                    "warn",
                    "mqtt.publish.error",
                    &[("message", error.to_string())],
                );
            }
        }
        Err(error) => log_event("warn", "mqtt.json.error", &[("message", error.to_string())]),
    }
}

fn publish_discovery(client: &Client, topics: &MqttTopics, qos: QoS) {
    let definitions = [
        discovery_switch(
            topics,
            "mute",
            "Microphone Muted",
            "microphone",
            topics.state("mute"),
            topics.command("mute"),
        ),
        discovery_switch(
            topics,
            "live_when_muted",
            "Lights Show Live State",
            "light",
            topics.state("live_when_muted"),
            topics.command("live_when_muted"),
        ),
        discovery_switch(
            topics,
            "lighting_available",
            "Lighting Available",
            "light",
            topics.state("lighting_available"),
            String::new(),
        ),
        discovery_number(
            topics,
            "mic_volume",
            "Mic Volume",
            "volume",
            topics.state("mic_volume"),
            topics.command("mic_volume"),
        ),
        discovery_number(
            topics,
            "mic_monitoring",
            "Mic Monitoring",
            "volume",
            topics.state("mic_monitoring"),
            topics.command("mic_monitoring"),
        ),
        discovery_number(
            topics,
            "headphone_volume",
            "Headphone Volume",
            "volume",
            topics.state("headphone_volume"),
            topics.command("headphone_volume"),
        ),
        discovery_number(
            topics,
            "brightness",
            "Lighting Brightness",
            "light",
            topics.state("brightness"),
            topics.command("brightness"),
        ),
        discovery_number(
            topics,
            "speed",
            "Lighting Speed",
            "speedometer",
            topics.state("speed"),
            topics.command("speed"),
        ),
        discovery_number(
            topics,
            "opacity",
            "Lighting Opacity",
            "brightness-percent",
            topics.state("opacity"),
            topics.command("opacity"),
        ),
        discovery_sensor(
            topics,
            "input_level",
            "Input Level",
            "sound-pressure",
            "%",
            topics.state("input_level"),
        ),
        discovery_sensor(
            topics,
            "device_state",
            "Device State",
            "information",
            "",
            topics.state("device_state"),
        ),
        discovery_sensor(
            topics,
            "polar_pattern",
            "Polar Pattern",
            "microphone",
            "",
            topics.state("polar_pattern"),
        ),
        discovery_select(
            topics,
            "effect",
            "Lighting Effect",
            vec![
                "wave",
                "solid",
                "cycle",
                "pulse",
                "blink",
                "lightning",
                "vu_meter",
            ],
            topics.state("effect"),
            topics.command("effect"),
        ),
        discovery_select(
            topics,
            "target",
            "Lighting Target",
            vec!["all", "top", "bottom"],
            topics.state("target"),
            topics.command("target"),
        ),
        discovery_button(topics, "apply", "Apply Lighting", topics.command("apply")),
        discovery_button(
            topics,
            "stop",
            "Stop Lighting Stream",
            topics.command("stop"),
        ),
        discovery_button(
            topics,
            "save",
            "Save Lighting To Mic",
            topics.command("save"),
        ),
    ];

    for (topic, payload) in definitions {
        publish_json(client, topic, qos, true, &payload);
    }
}

fn discovery_base(topics: &MqttTopics, key: &str, name: &str, icon: &str) -> serde_json::Value {
    json!({
        "name": name,
        "unique_id": format!("hyperx_mic_lite_{key}"),
        "object_id": format!("hyperx_mic_lite_{key}"),
        "availability_topic": topics.availability(),
        "payload_available": "online",
        "payload_not_available": "offline",
        "icon": format!("mdi:{icon}"),
        "device": {
            "identifiers": ["hyperx_mic_lite_quadcast_s"],
            "name": "HyperX QuadCast S",
            "manufacturer": "HyperX",
            "model": "QuadCast S",
        },
    })
}

fn discovery_switch(
    topics: &MqttTopics,
    key: &str,
    name: &str,
    icon: &str,
    state_topic: String,
    command_topic: String,
) -> (String, serde_json::Value) {
    let mut value = discovery_base(topics, key, name, icon);
    value["state_topic"] = json!(state_topic);
    if !command_topic.is_empty() {
        value["command_topic"] = json!(command_topic);
    }
    value["payload_on"] = json!("ON");
    value["payload_off"] = json!("OFF");
    (topics.discovery("switch", key), value)
}

fn discovery_number(
    topics: &MqttTopics,
    key: &str,
    name: &str,
    icon: &str,
    state_topic: String,
    command_topic: String,
) -> (String, serde_json::Value) {
    let mut value = discovery_base(topics, key, name, icon);
    value["state_topic"] = json!(state_topic);
    value["command_topic"] = json!(command_topic);
    value["min"] = json!(0);
    value["max"] = json!(100);
    value["step"] = json!(1);
    value["mode"] = json!("slider");
    (topics.discovery("number", key), value)
}

fn discovery_sensor(
    topics: &MqttTopics,
    key: &str,
    name: &str,
    icon: &str,
    unit: &str,
    state_topic: String,
) -> (String, serde_json::Value) {
    let mut value = discovery_base(topics, key, name, icon);
    value["state_topic"] = json!(state_topic);
    if !unit.is_empty() {
        value["unit_of_measurement"] = json!(unit);
    }
    (topics.discovery("sensor", key), value)
}

fn discovery_select(
    topics: &MqttTopics,
    key: &str,
    name: &str,
    options: Vec<&str>,
    state_topic: String,
    command_topic: String,
) -> (String, serde_json::Value) {
    let mut value = discovery_base(topics, key, name, "form-select");
    value["state_topic"] = json!(state_topic);
    value["command_topic"] = json!(command_topic);
    value["options"] = json!(options);
    (topics.discovery("select", key), value)
}

fn discovery_button(
    topics: &MqttTopics,
    key: &str,
    name: &str,
    command_topic: String,
) -> (String, serde_json::Value) {
    let mut value = discovery_base(topics, key, name, "gesture-tap-button");
    value["command_topic"] = json!(command_topic);
    (topics.discovery("button", key), value)
}

fn parse_command(topics: &MqttTopics, topic: &str, payload: &str) -> Option<MqttCommand> {
    let key = topic.strip_prefix(&format!("{}/command/", topics.base))?;
    let payload = payload.trim();
    match key {
        "mute" => parse_bool_command(payload)
            .map(MqttCommand::SetMute)
            .or_else(|| {
                if payload.eq_ignore_ascii_case("toggle") {
                    Some(MqttCommand::ToggleMute)
                } else {
                    None
                }
            }),
        "mic_volume" => parse_percent(payload).map(MqttCommand::SetMicVolume),
        "mic_monitoring" => parse_percent(payload).map(MqttCommand::SetMonitoringVolume),
        "headphone_volume" => parse_percent(payload).map(MqttCommand::SetHeadphoneVolume),
        "brightness" => parse_percent(payload).map(MqttCommand::SetBrightness),
        "speed" => parse_percent(payload).map(MqttCommand::SetSpeed),
        "opacity" => parse_percent(payload).map(MqttCommand::SetOpacity),
        "effect" => parse_effect(payload).map(MqttCommand::SetEffect),
        "target" => parse_target(payload).map(MqttCommand::SetTarget),
        "live_when_muted" => parse_bool_command(payload).map(MqttCommand::SetLiveWhenMuted),
        "apply" => Some(MqttCommand::ApplyLighting),
        "stop" => Some(MqttCommand::StopLighting),
        "save" => Some(MqttCommand::SaveLighting),
        _ => None,
    }
}

fn parse_bool_command(payload: &str) -> Option<bool> {
    match payload.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "muted" | "yes" => Some(true),
        "off" | "false" | "0" | "live" | "no" => Some(false),
        _ => None,
    }
}

fn parse_percent(payload: &str) -> Option<u8> {
    payload.parse::<u8>().ok().filter(|value| *value <= 100)
}

fn parse_effect(payload: &str) -> Option<Effect> {
    match payload {
        "wave" => Some(Effect::Wave),
        "solid" => Some(Effect::Solid),
        "cycle" => Some(Effect::Cycle),
        "pulse" => Some(Effect::Pulse),
        "blink" => Some(Effect::Blink),
        "lightning" => Some(Effect::Lightning),
        "vu_meter" => Some(Effect::VuMeter),
        _ => None,
    }
}

fn parse_target(payload: &str) -> Option<LightTarget> {
    match payload {
        "all" => Some(LightTarget::All),
        "top" => Some(LightTarget::Top),
        "bottom" => Some(LightTarget::Bottom),
        _ => None,
    }
}

fn qos_from_u8(value: u8) -> QoS {
    match value {
        0 => QoS::AtMostOnce,
        2 => QoS::ExactlyOnce,
        _ => QoS::AtLeastOnce,
    }
}

fn trim_topic(value: &str) -> String {
    value.trim().trim_matches('/').to_string()
}

fn mqtt_url_with_client_id(url: &str, client_id: &str) -> String {
    if url.to_ascii_lowercase().contains("client_id=") {
        return url.to_string();
    }
    let separator = if url.contains('?') { '&' } else { '?' };
    format!(
        "{url}{separator}client_id={}",
        mqtt_url_component(client_id)
    )
}

fn mqtt_url_component(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            other => format!("%{other:02X}").chars().collect(),
        })
        .collect()
}

impl MqttTopics {
    fn availability(&self) -> String {
        format!("{}/status", self.base)
    }

    fn state(&self, key: &str) -> String {
        format!("{}/state/{key}", self.base)
    }

    fn command(&self, key: &str) -> String {
        format!("{}/command/{key}", self.base)
    }

    fn command_wildcard(&self) -> String {
        format!("{}/command/#", self.base)
    }

    fn discovery(&self, component: &str, key: &str) -> String {
        format!(
            "{}/{}/hyperx_mic_lite_{}/config",
            self.discovery_prefix, component, key
        )
    }
}
