use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    constants::CONFIG_SCHEMA_VERSION,
    logging::log_event,
    paths::{config_dir, config_path},
    time::unix_timestamp_seconds,
};

fn default_lighting_target() -> String {
    "all".to_string()
}

fn default_minimize_to_tray() -> bool {
    true
}

fn default_last_polar_pattern() -> String {
    "unknown".to_string()
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct AppConfig {
    pub(crate) schema_version: u32,
    pub(crate) audio: AudioConfig,
    pub(crate) lighting: LightingConfig,
    pub(crate) ui: UiConfig,
    pub(crate) service: ServiceConfig,
    pub(crate) device: DeviceConfig,
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
        }
    }
}

impl AppConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
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

fn validate_rgb_hex(value: &str) -> Result<(), String> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 || !hex.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Err(format!("Invalid RGB color '{value}'. Use rrggbb."));
    }
    Ok(())
}

pub(crate) fn load_or_create_config() -> Result<AppConfig, String> {
    let path = config_path();
    if !path.exists() {
        let config = AppConfig::default();
        save_config(&config)?;
        return Ok(config);
    }
    load_config_from_path(&path)
}

pub(crate) fn load_config_from_path(path: &Path) -> Result<AppConfig, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let value = serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|error| format!("{}: invalid JSON: {error}", path.display()))?;
    let migrated = migrate_config_value(value);
    let config = serde_json::from_value::<AppConfig>(migrated)
        .map_err(|error| format!("{}: invalid config: {error}", path.display()))?
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

pub(crate) fn save_config(config: &AppConfig) -> Result<(), String> {
    config.validate()?;
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(config).map_err(|error| error.to_string())?;
    fs::write(&path, format!("{text}\n"))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    log_event(
        "info",
        "config.save",
        &[("path", path.display().to_string())],
    );
    Ok(())
}

pub(crate) fn export_config(destination: &Path) -> Result<(), String> {
    let config = load_or_create_config()?;
    let text = serde_json::to_string_pretty(&config).map_err(|error| error.to_string())?;
    fs::write(destination, format!("{text}\n"))
        .map_err(|error| format!("{}: {error}", destination.display()))?;
    log_event(
        "info",
        "config.export",
        &[("path", destination.display().to_string())],
    );
    println!("Exported config to {}", destination.display());
    Ok(())
}

pub(crate) fn import_config(source: &Path) -> Result<(), String> {
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

pub(crate) fn validate_config_file(path: &Path) -> Result<(), String> {
    let config = load_config_from_path(path)?;
    config.validate()?;
    println!("Config is valid: {}", path.display());
    Ok(())
}

pub(crate) fn reset_config() -> Result<(), String> {
    backup_config_if_present()?;
    save_config(&AppConfig::default())?;
    log_event("info", "config.reset", &[]);
    println!("Reset config to defaults at {}", config_path().display());
    Ok(())
}

fn backup_config_if_present() -> Result<(), String> {
    let path = config_path();
    if !path.exists() {
        return Ok(());
    }
    let backup = config_dir().join(format!("config.backup.{}.json", unix_timestamp_seconds()));
    fs::copy(&path, &backup)
        .map(|_| ())
        .map_err(|error| format!("{}: {error}", backup.display()))?;
    log_event(
        "info",
        "config.backup",
        &[("path", backup.display().to_string())],
    );
    Ok(())
}
