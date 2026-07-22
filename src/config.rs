use std::{
    error, fmt, fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    constants::CONFIG_SCHEMA_VERSION,
    logging::log_event,
    paths::{config_dir, config_path},
    time::unix_timestamp_seconds,
};

#[derive(Debug)]
pub(crate) enum ConfigError {
    Io { path: PathBuf, source: io::Error },
    InvalidJson { path: PathBuf, message: String },
    InvalidConfig { path: PathBuf, message: String },
    Validation(String),
    Serialize(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::InvalidJson { path, message } => {
                write!(formatter, "{}: invalid JSON: {message}", path.display())
            }
            Self::InvalidConfig { path, message } => {
                write!(formatter, "{}: invalid config: {message}", path.display())
            }
            Self::Validation(message) | Self::Serialize(message) => {
                write!(formatter, "{message}")
            }
        }
    }
}

impl error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn default_lighting_target() -> String {
    "all".to_string()
}

fn default_lighting_top_effect() -> String {
    "solid".to_string()
}

fn default_lighting_bottom_effect() -> String {
    "blink".to_string()
}

fn default_minimize_to_tray() -> bool {
    true
}

fn default_last_polar_pattern() -> String {
    "unknown".to_string()
}

fn default_mqtt_url() -> String {
    "mqtt://localhost:1883".to_string()
}

fn default_mqtt_client_id() -> String {
    "hyperx-mic-lite".to_string()
}

fn default_mqtt_base_topic() -> String {
    "hyperx_mic_lite/quadcast_s".to_string()
}

fn default_mqtt_discovery_prefix() -> String {
    "homeassistant".to_string()
}

fn default_true() -> bool {
    true
}

fn default_mqtt_qos() -> u8 {
    1
}

fn default_mqtt_keep_alive_secs() -> u64 {
    30
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct AppConfig {
    pub(crate) schema_version: u32,
    pub(crate) audio: AudioConfig,
    pub(crate) lighting: LightingConfig,
    pub(crate) ui: UiConfig,
    pub(crate) service: ServiceConfig,
    pub(crate) device: DeviceConfig,
    #[serde(default)]
    pub(crate) mqtt: MqttConfig,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct AudioConfig {
    pub(crate) mic_volume: u8,
    pub(crate) mic_monitoring: u8,
    pub(crate) headphone_volume: u8,
    pub(crate) mute_on_app_start: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct LightingConfig {
    pub(crate) effect: String,
    #[serde(default = "default_lighting_target")]
    pub(crate) target: String,
    #[serde(default)]
    pub(crate) split_layers: bool,
    #[serde(default = "default_lighting_top_effect")]
    pub(crate) top_effect: String,
    #[serde(default = "default_lighting_bottom_effect")]
    pub(crate) bottom_effect: String,
    pub(crate) colors: Vec<String>,
    pub(crate) selected_color: usize,
    pub(crate) opacity: u8,
    pub(crate) speed: u8,
    pub(crate) brightness: u8,
    pub(crate) live_when_muted: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct UiConfig {
    pub(crate) selected_tab: String,
    pub(crate) window_width: f32,
    pub(crate) window_height: f32,
    #[serde(default)]
    pub(crate) window_x: Option<f32>,
    #[serde(default)]
    pub(crate) window_y: Option<f32>,
    #[serde(default = "default_minimize_to_tray")]
    pub(crate) minimize_to_tray: bool,
    #[serde(default = "default_last_polar_pattern")]
    pub(crate) last_polar_pattern: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ServiceConfig {
    pub(crate) enabled: bool,
    pub(crate) restore_on_startup: bool,
    pub(crate) owns_startup_restore: bool,
    pub(crate) owns_lighting_loop: bool,
    pub(crate) owns_hid_monitoring: bool,
    pub(crate) owns_tray_handoff: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct DeviceConfig {
    pub(crate) preferred_capture_endpoint_id: Option<String>,
    pub(crate) lighting_vendor_id: u16,
    pub(crate) lighting_product_id: u16,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct MqttConfig {
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default = "default_mqtt_url")]
    pub(crate) url: String,
    #[serde(default = "default_mqtt_client_id")]
    pub(crate) client_id: String,
    #[serde(default)]
    pub(crate) username: Option<String>,
    #[serde(default)]
    pub(crate) password: Option<String>,
    #[serde(default = "default_mqtt_base_topic")]
    pub(crate) base_topic: String,
    #[serde(default = "default_mqtt_discovery_prefix")]
    pub(crate) discovery_prefix: String,
    #[serde(default = "default_true")]
    pub(crate) home_assistant_discovery: bool,
    #[serde(default = "default_true")]
    pub(crate) retain_state: bool,
    #[serde(default = "default_mqtt_qos")]
    pub(crate) qos: u8,
    #[serde(default = "default_mqtt_keep_alive_secs")]
    pub(crate) keep_alive_secs: u64,
    #[serde(default = "default_true")]
    pub(crate) clean_session: bool,
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: default_mqtt_url(),
            client_id: default_mqtt_client_id(),
            username: None,
            password: None,
            base_topic: default_mqtt_base_topic(),
            discovery_prefix: default_mqtt_discovery_prefix(),
            home_assistant_discovery: true,
            retain_state: true,
            qos: 1,
            keep_alive_secs: 30,
            clean_session: true,
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            audio: AudioConfig {
                mic_volume: 50,
                mic_monitoring: 71,
                headphone_volume: 5,
                mute_on_app_start: false,
            },
            lighting: LightingConfig {
                effect: "wave".to_string(),
                target: "all".to_string(),
                split_layers: false,
                top_effect: default_lighting_top_effect(),
                bottom_effect: default_lighting_bottom_effect(),
                colors: vec![
                    "#ff2010".to_string(),
                    "#ff009a".to_string(),
                    "#5d18ff".to_string(),
                    "#00a2ff".to_string(),
                    "#00edbf".to_string(),
                    "#38ee3d".to_string(),
                    "#ffea20".to_string(),
                ],
                selected_color: 0,
                opacity: 25,
                speed: 75,
                brightness: 100,
                live_when_muted: true,
            },
            ui: UiConfig {
                selected_tab: "audio".to_string(),
                window_width: 1120.0,
                window_height: 760.0,
                window_x: None,
                window_y: None,
                minimize_to_tray: true,
                last_polar_pattern: "unknown".to_string(),
            },
            service: ServiceConfig {
                enabled: false,
                restore_on_startup: false,
                owns_startup_restore: true,
                owns_lighting_loop: false,
                owns_hid_monitoring: false,
                owns_tray_handoff: false,
            },
            device: DeviceConfig {
                preferred_capture_endpoint_id: None,
                lighting_vendor_id: 0x0951,
                lighting_product_id: 0x171f,
            },
            mqtt: MqttConfig::default(),
        }
    }
}

impl AppConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        self.validate_fields().map_err(ConfigError::Validation)
    }

    fn validate_fields(&self) -> Result<(), String> {
        if self.schema_version == 0 || self.schema_version > CONFIG_SCHEMA_VERSION {
            return Err(format!(
                "Unsupported config schema version {}.",
                self.schema_version
            ));
        }
        validate_percent("audio.mic_volume", self.audio.mic_volume)?;
        validate_percent("audio.mic_monitoring", self.audio.mic_monitoring)?;
        validate_percent("audio.headphone_volume", self.audio.headphone_volume)?;
        validate_percent("lighting.opacity", self.lighting.opacity)?;
        validate_percent("lighting.speed", self.lighting.speed)?;
        validate_percent("lighting.brightness", self.lighting.brightness)?;
        if self.lighting.colors.is_empty() {
            return Err("lighting.colors must contain at least one color.".to_string());
        }
        for color in &self.lighting.colors {
            validate_rgb_hex(color)?;
        }
        if self.lighting.selected_color >= self.lighting.colors.len() {
            return Err("lighting.selected_color is outside lighting.colors.".to_string());
        }
        if !matches!(self.lighting.target.as_str(), "all" | "top" | "bottom") {
            return Err("lighting.target must be 'all', 'top', or 'bottom'.".to_string());
        }
        validate_effect_name("lighting.effect", &self.lighting.effect)?;
        validate_effect_name("lighting.top_effect", &self.lighting.top_effect)?;
        validate_effect_name("lighting.bottom_effect", &self.lighting.bottom_effect)?;
        if !matches!(self.ui.selected_tab.as_str(), "audio" | "lights") {
            return Err("ui.selected_tab must be 'audio' or 'lights'.".to_string());
        }
        if !matches!(
            self.ui.last_polar_pattern.as_str(),
            "stereo" | "omni" | "cardioid" | "bidirectional" | "unknown"
        ) {
            return Err("ui.last_polar_pattern is invalid.".to_string());
        }
        if self.ui.window_width < 640.0 || self.ui.window_height < 480.0 {
            return Err("ui.window_width/window_height are too small.".to_string());
        }
        validate_optional_window_position("ui.window_x", self.ui.window_x)?;
        validate_optional_window_position("ui.window_y", self.ui.window_y)?;
        validate_topic_prefix("mqtt.base_topic", &self.mqtt.base_topic)?;
        validate_topic_prefix("mqtt.discovery_prefix", &self.mqtt.discovery_prefix)?;
        if self.mqtt.client_id.trim().is_empty() {
            return Err("mqtt.client_id must not be empty.".to_string());
        }
        if self.mqtt.qos > 2 {
            return Err("mqtt.qos must be 0, 1, or 2.".to_string());
        }
        if !(5..=3600).contains(&self.mqtt.keep_alive_secs) {
            return Err("mqtt.keep_alive_secs must be 5..3600.".to_string());
        }
        Ok(())
    }

    fn migrated(mut self) -> Self {
        if self.schema_version < CONFIG_SCHEMA_VERSION {
            self.schema_version = CONFIG_SCHEMA_VERSION;
        }
        self
    }
}

fn validate_percent(name: &str, value: u8) -> Result<(), String> {
    if value > 100 {
        Err(format!("{name} must be 0..100."))
    } else {
        Ok(())
    }
}

fn validate_optional_window_position(name: &str, value: Option<f32>) -> Result<(), String> {
    if let Some(value) = value {
        if !value.is_finite() || !(-32000.0..=32000.0).contains(&value) {
            return Err(format!("{name} must be a finite screen coordinate."));
        }
    }
    Ok(())
}

fn validate_effect_name(name: &str, value: &str) -> Result<(), String> {
    if matches!(
        value,
        "wave" | "solid" | "cycle" | "pulse" | "blink" | "lightning" | "vu_meter"
    ) {
        Ok(())
    } else {
        Err(format!("{name} is invalid."))
    }
}

fn validate_rgb_hex(value: &str) -> Result<(), String> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 || !hex.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Err(format!("Invalid RGB color '{value}'. Use rrggbb."));
    }
    Ok(())
}

fn validate_topic_prefix(name: &str, value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('/') || value.ends_with('/') {
        return Err(format!(
            "{name} must be a non-empty topic prefix without leading/trailing slash."
        ));
    }
    if value.contains('#') || value.contains('+') {
        return Err(format!("{name} must not contain MQTT wildcards."));
    }
    Ok(())
}

pub(crate) fn load_or_create_config() -> Result<AppConfig, ConfigError> {
    let path = config_path();
    if !path.exists() {
        let config = AppConfig::default();
        save_config(&config)?;
        return Ok(config);
    }
    load_config_from_path(&path)
}

pub(crate) fn load_config_from_path(path: &Path) -> Result<AppConfig, ConfigError> {
    let text = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let value = serde_json::from_str::<serde_json::Value>(&text).map_err(|error| {
        ConfigError::InvalidJson {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    })?;
    let migrated = migrate_config_value(value);
    let config = serde_json::from_value::<AppConfig>(migrated)
        .map_err(|error| ConfigError::InvalidConfig {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?
        .migrated();
    config.validate()?;
    log_event(
        "info",
        "config.load",
        &[("path", path.display().to_string())],
    );
    Ok(config)
}

fn migrate_config_value(mut value: serde_json::Value) -> serde_json::Value {
    let defaults = serde_json::to_value(AppConfig::default()).unwrap_or_default();
    merge_missing_json(&mut value, &defaults);
    value
}

fn merge_missing_json(value: &mut serde_json::Value, defaults: &serde_json::Value) {
    if let (Some(value_object), Some(default_object)) =
        (value.as_object_mut(), defaults.as_object())
    {
        for (key, default_value) in default_object {
            match value_object.get_mut(key) {
                Some(existing) => merge_missing_json(existing, default_value),
                None => {
                    value_object.insert(key.clone(), default_value.clone());
                }
            }
        }
    }
}

pub(crate) fn save_config(config: &AppConfig) -> Result<(), ConfigError> {
    config.validate()?;
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let text = serde_json::to_string_pretty(config)
        .map_err(|error| ConfigError::Serialize(error.to_string()))?;
    // Write to a temp file and rename so a crash mid-write cannot truncate config.json.
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, format!("{text}\n")).map_err(|source| ConfigError::Io {
        path: temp_path.clone(),
        source,
    })?;
    fs::rename(&temp_path, &path).map_err(|source| ConfigError::Io {
        path: path.clone(),
        source,
    })?;
    log_event(
        "info",
        "config.save",
        &[("path", path.display().to_string())],
    );
    Ok(())
}

pub(crate) fn export_config(destination: &Path) -> Result<(), ConfigError> {
    let config = load_or_create_config()?;
    let text = serde_json::to_string_pretty(&config)
        .map_err(|error| ConfigError::Serialize(error.to_string()))?;
    fs::write(destination, format!("{text}\n")).map_err(|source| ConfigError::Io {
        path: destination.to_path_buf(),
        source,
    })?;
    log_event(
        "info",
        "config.export",
        &[("path", destination.display().to_string())],
    );
    println!("Exported config to {}", destination.display());
    Ok(())
}

pub(crate) fn import_config(source: &Path) -> Result<(), ConfigError> {
    let config = load_config_from_path(source)?;
    backup_config_if_present()?;
    save_config(&config)?;
    log_event(
        "info",
        "config.import",
        &[("source", source.display().to_string())],
    );
    println!("Imported config from {}", source.display());
    Ok(())
}

pub(crate) fn validate_config_file(path: &Path) -> Result<(), ConfigError> {
    let config = load_config_from_path(path)?;
    config.validate()?;
    println!("Config is valid: {}", path.display());
    Ok(())
}

pub(crate) fn reset_config() -> Result<(), ConfigError> {
    backup_config_if_present()?;
    save_config(&AppConfig::default())?;
    log_event("info", "config.reset", &[]);
    println!("Reset config to defaults at {}", config_path().display());
    Ok(())
}

fn backup_config_if_present() -> Result<(), ConfigError> {
    let path = config_path();
    if !path.exists() {
        return Ok(());
    }
    let backup = config_dir().join(format!("config.backup.{}.json", unix_timestamp_seconds()));
    fs::copy(&path, &backup)
        .map(|_| ())
        .map_err(|source| ConfigError::Io {
            path: backup.clone(),
            source,
        })?;
    log_event(
        "info",
        "config.backup",
        &[("path", backup.display().to_string())],
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config_file(name: &str, contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hyperx-mic-lite-config-tests-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join(name);
        fs::write(&path, contents).expect("write temp config");
        path
    }

    #[test]
    fn migrate_adds_missing_sections_with_defaults() {
        // Arrange: old-schema config with no mqtt/ui sections
        let value = serde_json::json!({
            "schema_version": 1,
            "audio": { "mic_volume": 33 }
        });

        // Act
        let migrated = migrate_config_value(value);

        // Assert: defaults filled in, nothing else disturbed
        assert_eq!(migrated["mqtt"]["url"], "mqtt://localhost:1883");
        assert_eq!(migrated["ui"]["minimize_to_tray"], true);
        assert_eq!(migrated["lighting"]["target"], "all");
    }

    #[test]
    fn migrate_preserves_existing_values() {
        // Arrange
        let value = serde_json::json!({
            "schema_version": 1,
            "audio": { "mic_volume": 33 },
            "mqtt": { "url": "mqtt://broker.local:1883" }
        });

        // Act
        let migrated = migrate_config_value(value);

        // Assert
        assert_eq!(migrated["audio"]["mic_volume"], 33);
        assert_eq!(migrated["mqtt"]["url"], "mqtt://broker.local:1883");
    }

    #[test]
    fn validate_accepts_default_config() {
        assert!(AppConfig::default().validate().is_ok());
    }

    #[test]
    fn validate_rejects_percent_above_100() {
        // Arrange
        let mut config = AppConfig::default();
        config.audio.mic_volume = 101;

        // Act
        let error = config.validate().expect_err("101% must be rejected");

        // Assert
        assert!(error.to_string().contains("audio.mic_volume"));
    }

    #[test]
    fn validate_rejects_invalid_rgb_color() {
        let mut config = AppConfig::default();
        config.lighting.colors = vec!["zzzzzz".to_string()];

        let error = config.validate().expect_err("non-hex color must fail");

        assert!(error.to_string().contains("Invalid RGB color"));
    }

    #[test]
    fn validate_rejects_selected_color_outside_palette() {
        let mut config = AppConfig::default();
        config.lighting.selected_color = config.lighting.colors.len();

        let error = config.validate().expect_err("out-of-range index");

        assert!(error.to_string().contains("selected_color"));
    }

    #[test]
    fn validate_rejects_topic_prefix_with_wildcards() {
        let mut config = AppConfig::default();
        config.mqtt.base_topic = "hyperx/#".to_string();

        let error = config.validate().expect_err("wildcard topic must fail");

        assert!(error.to_string().contains("wildcards"));
    }

    #[test]
    fn validate_rejects_slash_wrapped_topic_prefix() {
        let mut config = AppConfig::default();
        config.mqtt.base_topic = "/hyperx".to_string();

        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_rejects_non_finite_window_position() {
        let mut config = AppConfig::default();
        config.ui.window_x = Some(f32::NAN);

        let error = config.validate().expect_err("NaN position must fail");

        assert!(error.to_string().contains("ui.window_x"));
    }

    #[test]
    fn load_round_trips_default_config() {
        // Arrange
        let text = serde_json::to_string_pretty(&AppConfig::default()).expect("serialize");
        let path = temp_config_file("roundtrip.json", &text);

        // Act
        let loaded = load_config_from_path(&path).expect("load config");

        // Assert
        let defaults = AppConfig::default();
        assert_eq!(loaded.schema_version, defaults.schema_version);
        assert_eq!(loaded.audio.mic_volume, defaults.audio.mic_volume);
        assert_eq!(loaded.mqtt.base_topic, defaults.mqtt.base_topic);
        let _ = fs::remove_file(&path);
    }

    // expect_err would require AppConfig: Debug, which we deliberately avoid deriving
    // (MqttConfig carries a password that must not become printable crate-wide).
    fn expect_load_error(path: &Path, context: &str) -> ConfigError {
        match load_config_from_path(path) {
            Err(error) => error,
            Ok(_) => panic!("{context}: expected load_config_from_path to fail"),
        }
    }

    #[test]
    fn load_reports_unreadable_file_as_io_error() {
        let path = std::env::temp_dir().join("hyperx-mic-lite-config-tests-missing/none.json");

        let error = expect_load_error(&path, "missing file");

        assert!(matches!(error, ConfigError::Io { .. }));
    }

    #[test]
    fn load_reports_malformed_json() {
        let path = temp_config_file("malformed.json", "{ not json");

        let error = expect_load_error(&path, "malformed JSON");

        assert!(matches!(error, ConfigError::InvalidJson { .. }));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn load_rejects_out_of_range_values_as_validation_error() {
        // Arrange: schema gets migrated, but the bad value must still fail validation
        let text = serde_json::json!({
            "schema_version": 1,
            "audio": { "mic_volume": 200 }
        })
        .to_string();
        let path = temp_config_file("invalid-values.json", &text);

        // Act
        let error = expect_load_error(&path, "200% must be rejected");

        // Assert
        assert!(matches!(error, ConfigError::Validation(_)));
        let _ = fs::remove_file(&path);
    }
}
