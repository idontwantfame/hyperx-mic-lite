#![cfg(windows)]

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::{
    env,
    ffi::{CStr, OsString},
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};
use windows_service::{
    define_windows_service,
    service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
    service_manager::{ServiceManager, ServiceManagerAccess},
};
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ},
};

const CONFIG_SCHEMA_VERSION: u32 = 1;
const APP_NAME: &str = "HyperXMicLite";
const SERVICE_NAME: &str = "HyperXMicLite";
const SERVICE_DISPLAY_NAME: &str = "HyperX Mic Lite";
const SERVICE_DESCRIPTION: &str =
    "Restores HyperX Mic Lite microphone settings and hosts background device tasks.";
const STARTUP_VALUE_NAME: &str = "HyperXMicLite";
const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const EVENTLOG_SOURCE_PATH: &str =
    r"SYSTEM\CurrentControlSet\Services\EventLog\Application\HyperXMicLite";
const EVENTLOG_TYPES_SUPPORTED: u32 = 0x0007;
const EVENTLOG_MESSAGE_ID: u32 = 0x40000001;
use windows::{
    Win32::{
        Devices::{
            FunctionDiscovery::PKEY_Device_FriendlyName,
            HumanInterfaceDevice::{
                HIDP_CAPS, HIDP_STATUS_SUCCESS, HidD_FreePreparsedData, HidD_GetPreparsedData,
                HidP_GetCaps, PHIDP_PREPARSED_DATA,
            },
        },
        Foundation::CloseHandle,
        Media::Audio::{
            DEVICE_STATE, DEVICE_STATE_ACTIVE, DEVICE_STATE_DISABLED, DEVICE_STATE_NOTPRESENT,
            DEVICE_STATE_UNPLUGGED, DEVICE_STATEMASK_ALL,
            Endpoints::{IAudioEndpointVolume, IAudioMeterInformation},
            IAudioMute, IAudioVolumeLevel, IDeviceTopology, IMMDevice, IMMDeviceEnumerator,
            MMDeviceEnumerator, eCapture, eCommunications, eRender,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        },
        System::{
            Com::StructuredStorage::PropVariantClear,
            Com::{
                CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize, STGM_READ,
            },
            EventLog::{
                DeregisterEventSource, EVENTLOG_ERROR_TYPE, EVENTLOG_INFORMATION_TYPE,
                EVENTLOG_WARNING_TYPE, RegisterEventSourceW, ReportEventW,
            },
        },
    },
    core::Result as WinResult,
    core::{Error, HRESULT, Interface, PCWSTR, Type, w},
};

#[derive(Serialize)]
struct DeviceInfo {
    id: String,
    name: String,
    state: String,
    is_default: bool,
}

#[derive(Serialize)]
struct MicStatus {
    device: DeviceInfo,
    volume: u8,
    muted: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PolarPattern {
    Stereo,
    Omni,
    Cardioid,
    Bidirectional,
    Unknown(u8),
}

impl PolarPattern {
    fn from_report(value: u8) -> Self {
        match value {
            0 => Self::Stereo,
            1 => Self::Omni,
            2 => Self::Cardioid,
            3 => Self::Bidirectional,
            other => Self::Unknown(other),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Stereo => "Stereo",
            Self::Omni => "Omni",
            Self::Cardioid => "Cardioid",
            Self::Bidirectional => "Bidirectional",
            Self::Unknown(_) => "Unknown",
        }
    }
}

enum HidEvent {
    Mute(bool),
    Pattern(PolarPattern),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Audio,
    Lights,
}

impl Tab {
    fn from_config(value: &str) -> Self {
        match value {
            "lights" => Self::Lights,
            _ => Self::Audio,
        }
    }

    fn as_config(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Lights => "lights",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect {
    Wave,
    Solid,
    Cycle,
    Pulse,
    Blink,
    Lightning,
    VuMeter,
}

impl Effect {
    fn label(self) -> &'static str {
        match self {
            Self::Wave => "Wave",
            Self::Solid => "Solid",
            Self::Cycle => "Cycle",
            Self::Pulse => "Pulse",
            Self::Blink => "Blink",
            Self::Lightning => "Lightning",
            Self::VuMeter => "VU Meter",
        }
    }

    fn from_config(value: &str) -> Self {
        match value {
            "solid" => Self::Solid,
            "cycle" => Self::Cycle,
            "pulse" => Self::Pulse,
            "blink" => Self::Blink,
            "lightning" => Self::Lightning,
            "vu_meter" => Self::VuMeter,
            _ => Self::Wave,
        }
    }

    fn as_config(self) -> &'static str {
        match self {
            Self::Wave => "wave",
            Self::Solid => "solid",
            Self::Cycle => "cycle",
            Self::Pulse => "pulse",
            Self::Blink => "blink",
            Self::Lightning => "lightning",
            Self::VuMeter => "vu_meter",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LightTarget {
    All,
    Top,
    Bottom,
}

impl LightTarget {
    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Top => "Top",
            Self::Bottom => "Bottom",
        }
    }

    fn from_config(value: &str) -> Self {
        match value {
            "top" => Self::Top,
            "bottom" => Self::Bottom,
            _ => Self::All,
        }
    }

    fn as_config(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
}

fn default_lighting_target() -> String {
    "all".to_string()
}

#[derive(Clone, Serialize, Deserialize)]
struct AppConfig {
    schema_version: u32,
    audio: AudioConfig,
    lighting: LightingConfig,
    ui: UiConfig,
    service: ServiceConfig,
    device: DeviceConfig,
}

#[derive(Clone, Serialize, Deserialize)]
struct AudioConfig {
    mic_volume: u8,
    mic_monitoring: u8,
    headphone_volume: u8,
    mute_on_app_start: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct LightingConfig {
    effect: String,
    #[serde(default = "default_lighting_target")]
    target: String,
    colors: Vec<String>,
    selected_color: usize,
    opacity: u8,
    speed: u8,
    brightness: u8,
    live_when_muted: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct UiConfig {
    selected_tab: String,
    window_width: f32,
    window_height: f32,
}

#[derive(Clone, Serialize, Deserialize)]
struct ServiceConfig {
    enabled: bool,
    restore_on_startup: bool,
    owns_startup_restore: bool,
    owns_lighting_loop: bool,
    owns_hid_monitoring: bool,
    owns_tray_handoff: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct DeviceConfig {
    preferred_capture_endpoint_id: Option<String>,
    lighting_vendor_id: u16,
    lighting_product_id: u16,
}

struct LightingState {
    effect: Effect,
    target: LightTarget,
    colors: Vec<egui::Color32>,
    selected_color: usize,
    opacity: u8,
    speed: u8,
    brightness: u8,
    live_when_muted: bool,
}

#[derive(Clone)]
struct LightingProgram {
    effect: Effect,
    target: LightTarget,
    colors: Vec<[u8; 3]>,
    speed: u8,
    brightness: u8,
}

const LIGHTING_CELL_COUNT: usize = 16;
type LightingFrame = [[u8; 3]; LIGHTING_CELL_COUNT];

#[derive(Clone, Copy)]
enum StreamDuration {
    Timed(Duration),
    Forever,
}

#[derive(Serialize)]
struct LightingDevice {
    vendor_id: u16,
    product_id: u16,
    interface_number: i32,
    usage_page: u16,
    usage: u16,
    manufacturer: String,
    product: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct ServiceHealth {
    schema_version: u32,
    service_name: String,
    state: String,
    pid: u32,
    updated_at: String,
    heartbeat_count: u64,
    restore_on_startup: bool,
    last_restore: Option<String>,
    last_error: Option<String>,
}

struct AudioPeakMonitor {
    peak_bits: Arc<AtomicU32>,
    _stream: cpal::Stream,
}

impl AudioPeakMonitor {
    fn peak(&self) -> f32 {
        f32::from_bits(self.peak_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0)
    }
}

struct MicLiteApp {
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
    start_minimized: bool,
    start_minimized_applied: bool,
}

impl ServiceHealth {
    fn new(state: &str) -> Self {
        Self {
            schema_version: 1,
            service_name: SERVICE_NAME.to_string(),
            state: state.to_string(),
            pid: process::id(),
            updated_at: log_timestamp(),
            heartbeat_count: 0,
            restore_on_startup: false,
            last_restore: None,
            last_error: None,
        }
    }
}

struct ComApartment;

impl ComApartment {
    fn init() -> WinResult<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
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
    fn validate(&self) -> Result<(), String> {
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
            parse_rgb_hex(color)?;
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

fn main() {
    install_panic_hook();
    log_event(
        "info",
        "app.start",
        &[("args", env::args().skip(1).collect::<Vec<_>>().join(" "))],
    );
    if let Err(error) = run() {
        log_event("error", "app.error", &[("message", error.to_string())]);
        eprintln!("{error}");
        process::exit(1);
    }
    log_event("info", "app.exit", &[]);
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|panic_info| {
        let message = panic_info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| {
                panic_info
                    .payload()
                    .downcast_ref::<String>()
                    .map(String::as_str)
            })
            .unwrap_or("panic");
        let location = panic_info
            .location()
            .map(|location| format!("{}:{}", location.file(), location.line()))
            .unwrap_or_else(|| "unknown".to_string());
        let report_path = write_crash_report(message, &location).unwrap_or_else(|_| PathBuf::new());
        log_event(
            "error",
            "app.panic",
            &[
                ("message", message.to_string()),
                ("location", location),
                ("report_path", report_path.display().to_string()),
            ],
        );
    }));
}

fn log_event(level: &str, event: &str, fields: &[(&str, String)]) {
    let path = log_file_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let mut object = serde_json::Map::new();
    object.insert("ts".to_string(), serde_json::Value::String(log_timestamp()));
    object.insert(
        "level".to_string(),
        serde_json::Value::String(level.to_string()),
    );
    object.insert(
        "event".to_string(),
        serde_json::Value::String(event.to_string()),
    );
    object.insert(
        "pid".to_string(),
        serde_json::Value::Number(serde_json::Number::from(process::id())),
    );
    for (key, value) in fields {
        object.insert((*key).to_string(), serde_json::Value::String(value.clone()));
    }

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{}", serde_json::Value::Object(object));
    }

    if should_write_event_log(level, event) {
        write_windows_event(level, event, fields);
    }
}

fn should_write_event_log(level: &str, event: &str) -> bool {
    matches!(level, "warn" | "error")
        || matches!(event, "app.start" | "app.exit" | "gui.start" | "gui.exit")
}

fn write_windows_event(level: &str, event: &str, fields: &[(&str, String)]) {
    let event_type = match level {
        "error" => EVENTLOG_ERROR_TYPE,
        "warn" => EVENTLOG_WARNING_TYPE,
        _ => EVENTLOG_INFORMATION_TYPE,
    };

    let mut lines = vec![
        format!("event={event}"),
        format!("level={level}"),
        format!("pid={}", process::id()),
        format!("log_path={}", log_file_path().display()),
    ];
    for (key, value) in fields {
        lines.push(format!("{key}={value}"));
    }
    let message = lines.join("\r\n");
    let wide_message = message.encode_utf16().chain([0]).collect::<Vec<_>>();
    let strings = [PCWSTR(wide_message.as_ptr())];

    unsafe {
        if let Ok(handle) = RegisterEventSourceW(None, w!("HyperXMicLite")) {
            let _ = ReportEventW(
                handle,
                event_type,
                0,
                event_id_for(event),
                None,
                0,
                Some(&strings),
                None,
            );
            let _ = DeregisterEventSource(handle);
        }
    }
}

fn event_id_for(event: &str) -> u32 {
    let _ = event;
    EVENTLOG_MESSAGE_ID
}

fn write_crash_report(message: &str, location: &str) -> Result<PathBuf, String> {
    let dir = app_data_dir().join("crashes");
    fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let path = dir.join(format!("panic-{}.json", unix_timestamp_seconds()));
    let report = serde_json::json!({
        "ts": log_timestamp(),
        "message": message,
        "location": location,
        "args": env::args().collect::<Vec<_>>(),
        "version": env!("CARGO_PKG_VERSION"),
        "log_path": log_file_path(),
        "config_path": config_path(),
    });
    fs::write(
        &path,
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(path)
}

fn log_timestamp() -> String {
    let seconds = unix_timestamp_seconds();
    format!("{seconds}")
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn log_file_path() -> PathBuf {
    app_data_dir().join("logs").join("app.log")
}

fn service_health_path() -> PathBuf {
    app_data_dir().join("service-health.json")
}

fn run() -> WinResult<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        usage();
        process::exit(2);
    }

    let _com = ComApartment::init()?;

    match args[0].as_str() {
        "list" => print_devices_json(&list_capture_devices()?),
        "status" => print_status_json(&mic_status()?),
        "lighting-detect" => print_lighting_detection(),
        "lighting-hid-dump" => print_lighting_hid_dump(),
        "hid-monitor" => run_hid_monitor(&args[1..]),
        "level-monitor" => run_level_monitor(&args[1..]),
        "lighting-solid" => run_lighting_solid(&args[1..]),
        "lighting-effect" => run_lighting_effect(&args[1..]),
        "lighting-vu-test" => run_lighting_vu_test(&args[1..]),
        "lighting-save" => run_lighting_save(&args[1..]),
        "audio" => run_audio_command(&args[1..])?,
        "config" => run_config_command(&args[1..]),
        "logs" => run_logs_command(&args[1..]),
        "diagnostics" => run_diagnostics_command(&args[1..]),
        "eventlog" => run_eventlog_command(&args[1..]),
        "service" => run_service_command(&args[1..]),
        "startup" => run_startup_command(&args[1..]),
        "service-run" => {
            if let Err(error) = run_windows_service() {
                let message = windows_service_error(error);
                log_event("error", "service.dispatcher.error", &[("message", message)]);
                process::exit(1);
            }
        }
        "gui" => run_gui(&args[1..]),
        "mute" => {
            set_mic_mute(true)?;
            print_status_json(&mic_status()?);
        }
        "unmute" => {
            set_mic_mute(false)?;
            print_status_json(&mic_status()?);
        }
        "toggle" => {
            let volume = endpoint_volume(&default_capture_device()?)?;
            let muted = unsafe { volume.GetMute()?.as_bool() };
            unsafe { volume.SetMute(!muted, std::ptr::null())? };
            print_status_json(&mic_status()?);
        }
        "volume" => set_volume(&args[1..])?,
        _ => {
            usage();
            process::exit(2);
        }
    }

    Ok(())
}

fn usage() {
    eprintln!(
        "hyperx-mic-lite controls the default Windows microphone.\n\n\
Usage:\n\
  hyperx-mic-lite list\n\
  hyperx-mic-lite status\n\
  hyperx-mic-lite mute\n\
  hyperx-mic-lite unmute\n\
  hyperx-mic-lite toggle\n\
  hyperx-mic-lite volume 75\n\
  hyperx-mic-lite audio volume <mic|monitoring|headphone> <0-100>\n\
  hyperx-mic-lite audio mute <mic|monitoring|headphone> <on|off>\n\
  hyperx-mic-lite lighting-detect\n\
  hyperx-mic-lite lighting-hid-dump\n\
  hyperx-mic-lite hid-monitor [seconds]\n\
  hyperx-mic-lite level-monitor [seconds]\n\
  hyperx-mic-lite lighting-solid ff0066 [seconds]\n\
  hyperx-mic-lite lighting-effect <solid|wave|cycle|pulse|blink|lightning|vu_meter> [seconds|forever]\n\
  hyperx-mic-lite lighting-vu-test <0-100> [seconds]\n\
  hyperx-mic-lite lighting-save [--packet-log]\n\
  hyperx-mic-lite config <path|dump|export|import|validate|reset>\n\
  hyperx-mic-lite logs <path|tail>\n\
  hyperx-mic-lite diagnostics export [directory]\n\
  hyperx-mic-lite eventlog <register|unregister|status>\n\
  hyperx-mic-lite service <install|uninstall|start|stop|status|run>\n\
  hyperx-mic-lite startup <install|uninstall|status>\n\
  hyperx-mic-lite gui [--start-minimized]"
    );
}

fn run_startup_command(args: &[String]) {
    if args.is_empty() {
        startup_usage();
        process::exit(2);
    }

    let result = match args[0].as_str() {
        "install" => install_user_gui_startup(&args[1..]),
        "uninstall" | "delete" => uninstall_user_gui_startup(),
        "status" => print_user_gui_startup_status(),
        _ => {
            startup_usage();
            process::exit(2);
        }
    };

    if let Err(error) = result {
        eprintln!("{error}");
        log_event("error", "startup.command.error", &[("message", error)]);
        process::exit(1);
    }
}

fn startup_usage() {
    eprintln!(
        "Usage:\n\
  hyperx-mic-lite startup install [--minimized|--normal]\n\
  hyperx-mic-lite startup uninstall\n\
  hyperx-mic-lite startup status"
    );
}

fn install_user_gui_startup(args: &[String]) -> Result<(), String> {
    let start_minimized = !args.iter().any(|arg| arg == "--normal");
    if args
        .iter()
        .any(|arg| arg != "--minimized" && arg != "--normal")
    {
        return Err("Usage: hyperx-mic-lite startup install [--minimized|--normal]".to_string());
    }
    let executable_path = env::current_exe().map_err(|error| error.to_string())?;
    let command = if start_minimized {
        format!("\"{}\" gui --start-minimized", executable_path.display())
    } else {
        format!("\"{}\" gui", executable_path.display())
    };
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run_key, _) = hkcu
        .create_subkey(RUN_KEY_PATH)
        .map_err(|error| error.to_string())?;
    run_key
        .set_value(STARTUP_VALUE_NAME, &command)
        .map_err(|error| error.to_string())?;
    println!("Installed per-user GUI startup: {command}");
    log_event(
        "info",
        "startup.gui.install",
        &[("command", command.to_string())],
    );
    Ok(())
}

fn uninstall_user_gui_startup() -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu
        .open_subkey_with_flags(RUN_KEY_PATH, winreg::enums::KEY_SET_VALUE)
        .map_err(|error| error.to_string())?;
    match run_key.delete_value(STARTUP_VALUE_NAME) {
        Ok(()) => {
            println!("Removed per-user GUI startup.");
            log_event("info", "startup.gui.uninstall", &[]);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("Per-user GUI startup was not installed.");
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn print_user_gui_startup_status() -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu
        .open_subkey(RUN_KEY_PATH)
        .map_err(|error| error.to_string())?;
    let command = run_key.get_value::<String, _>(STARTUP_VALUE_NAME);
    match command {
        Ok(command) => {
            println!(
                "{{\"installed\":true,\"command\":{}}}",
                json_string(&command)
            );
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("{{\"installed\":false}}");
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn run_eventlog_command(args: &[String]) {
    if args.is_empty() {
        eventlog_usage();
        process::exit(2);
    }

    let result = match args[0].as_str() {
        "register" => register_event_log_source().map(|path| {
            println!("Registered Event Viewer source at {path}");
        }),
        "unregister" => unregister_event_log_source().map(|_| {
            println!("Unregistered Event Viewer source {APP_NAME}.");
        }),
        "status" => print_event_log_source_status(),
        _ => {
            eventlog_usage();
            process::exit(2);
        }
    };

    if let Err(error) = result {
        eprintln!("{error}");
        log_event("error", "eventlog.command.error", &[("message", error)]);
        process::exit(1);
    }
}

fn eventlog_usage() {
    eprintln!(
        "Usage:\n\
  hyperx-mic-lite eventlog register\n\
  hyperx-mic-lite eventlog unregister\n\
  hyperx-mic-lite eventlog status"
    );
}

fn register_event_log_source() -> Result<String, String> {
    let executable_path = env::current_exe()
        .map_err(|error| error.to_string())?
        .display()
        .to_string();
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let (source_key, _) = hklm
        .create_subkey(EVENTLOG_SOURCE_PATH)
        .map_err(registry_admin_error)?;
    source_key
        .set_value("EventMessageFile", &executable_path)
        .map_err(registry_admin_error)?;
    source_key
        .set_value("TypesSupported", &EVENTLOG_TYPES_SUPPORTED)
        .map_err(registry_admin_error)?;
    source_key
        .set_value("CustomSource", &1u32)
        .map_err(registry_admin_error)?;
    log_event(
        "info",
        "eventlog.source.register",
        &[("message_file", executable_path.clone())],
    );
    Ok(executable_path)
}

fn unregister_event_log_source() -> Result<(), String> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    match hklm.delete_subkey_all(EVENTLOG_SOURCE_PATH) {
        Ok(()) => {
            log_event("info", "eventlog.source.unregister", &[]);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(registry_admin_error(error)),
    }
}

fn print_event_log_source_status() -> Result<(), String> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    match hklm.open_subkey_with_flags(EVENTLOG_SOURCE_PATH, KEY_READ) {
        Ok(source_key) => {
            let message_file = source_key
                .get_value::<String, _>("EventMessageFile")
                .unwrap_or_default();
            let types_supported = source_key
                .get_value::<u32, _>("TypesSupported")
                .unwrap_or_default();
            let output = serde_json::json!({
                "registered": true,
                "source": APP_NAME,
                "registry_path": format!("HKLM\\{EVENTLOG_SOURCE_PATH}"),
                "event_message_file": message_file,
                "types_supported": types_supported,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&output).map_err(|error| error.to_string())?
            );
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "{{\"registered\":false,\"source\":{}}}",
                json_string(APP_NAME)
            );
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn registry_admin_error(error: std::io::Error) -> String {
    match error.raw_os_error() {
        Some(5) => "Access denied. Run this command from an elevated terminal.".to_string(),
        _ => error.to_string(),
    }
}

fn run_service_command(args: &[String]) {
    if args.is_empty() {
        service_usage();
        process::exit(2);
    }

    let result = match args[0].as_str() {
        "install" => install_windows_service(),
        "uninstall" | "delete" => uninstall_windows_service(),
        "start" => start_installed_service(),
        "stop" => stop_installed_service(),
        "status" => print_installed_service_status(),
        "plan" => print_service_ownership_plan(),
        "run" => run_service_worker_console(),
        _ => {
            service_usage();
            process::exit(2);
        }
    };

    if let Err(error) = result {
        eprintln!("{error}");
        log_event("error", "service.command.error", &[("message", error)]);
        process::exit(1);
    }
}

fn service_usage() {
    eprintln!(
        "Usage:\n\
  hyperx-mic-lite service install\n\
  hyperx-mic-lite service uninstall\n\
  hyperx-mic-lite service start\n\
  hyperx-mic-lite service stop\n\
  hyperx-mic-lite service status\n\
  hyperx-mic-lite service plan\n\
  hyperx-mic-lite service run"
    );
}

fn run_diagnostics_command(args: &[String]) {
    if args.is_empty() {
        diagnostics_usage();
        process::exit(2);
    }

    let result = match args[0].as_str() {
        "export" => {
            let destination = args
                .get(1)
                .map(PathBuf::from)
                .unwrap_or_else(default_diagnostics_dir);
            export_diagnostics_bundle(&destination)
        }
        _ => {
            diagnostics_usage();
            process::exit(2);
        }
    };

    if let Err(error) = result {
        eprintln!("{error}");
        log_event("error", "diagnostics.export.error", &[("message", error)]);
        process::exit(1);
    }
}

fn diagnostics_usage() {
    eprintln!("Usage:\n  hyperx-mic-lite diagnostics export [directory]");
}

fn default_diagnostics_dir() -> PathBuf {
    app_data_dir()
        .join("diagnostics")
        .join(format!("diagnostics-{}", unix_timestamp_seconds()))
}

fn export_diagnostics_bundle(destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination)
        .map_err(|error| format!("{}: {error}", destination.display()))?;

    let manifest = serde_json::json!({
        "schema_version": 1,
        "app": APP_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "generated_at": log_timestamp(),
        "binary": env::current_exe().map(|path| path.display().to_string()).unwrap_or_default(),
        "config_path": config_path().display().to_string(),
        "log_path": log_file_path().display().to_string(),
    });
    write_json_file(&destination.join("manifest.json"), &manifest)?;

    let config_value = match fs::read_to_string(config_path()) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .map(redact_json_value)
            .unwrap_or_else(|error| serde_json::json!({ "error": error.to_string() })),
        Err(error) => serde_json::json!({ "error": error.to_string() }),
    };
    write_json_file(&destination.join("config.redacted.json"), &config_value)?;

    if log_file_path().exists() {
        fs::copy(log_file_path(), destination.join("app.log"))
            .map_err(|error| format!("copy app.log: {error}"))?;
    }

    write_json_file(
        &destination.join("audio-devices.json"),
        &match list_capture_devices() {
            Ok(devices) => serde_json::json!(devices),
            Err(error) => serde_json::json!({ "error": error.to_string() }),
        },
    )?;
    write_json_file(
        &destination.join("mic-status.json"),
        &match mic_status() {
            Ok(status) => serde_json::json!(status),
            Err(error) => serde_json::json!({ "error": error.to_string() }),
        },
    )?;
    write_json_file(
        &destination.join("lighting-hid.json"),
        &collect_lighting_hid_diagnostics(),
    )?;
    write_json_file(
        &destination.join("service-health.json"),
        &match read_service_health() {
            Ok(health) => serde_json::json!(health),
            Err(error) => serde_json::json!({ "error": error }),
        },
    )?;

    println!("Exported diagnostics bundle to {}", destination.display());
    log_event(
        "info",
        "diagnostics.export",
        &[("path", destination.display().to_string())],
    );
    Ok(())
}

fn collect_lighting_hid_diagnostics() -> serde_json::Value {
    let api = match hidapi::HidApi::new() {
        Ok(api) => api,
        Err(error) => return serde_json::json!({ "error": error.to_string() }),
    };

    let devices = api
        .device_list()
        .filter(|device| is_supported_lighting_device(device))
        .map(|device| {
            let caps = match hid_caps_for_path(device.path()) {
                Ok(caps) => serde_json::json!({
                    "input_report_bytes": caps.InputReportByteLength,
                    "output_report_bytes": caps.OutputReportByteLength,
                    "feature_report_bytes": caps.FeatureReportByteLength,
                    "input_button_caps": caps.NumberInputButtonCaps,
                    "input_value_caps": caps.NumberInputValueCaps,
                    "output_button_caps": caps.NumberOutputButtonCaps,
                    "output_value_caps": caps.NumberOutputValueCaps,
                    "feature_button_caps": caps.NumberFeatureButtonCaps,
                    "feature_value_caps": caps.NumberFeatureValueCaps,
                }),
                Err(error) => serde_json::json!({ "error": error }),
            };

            serde_json::json!({
                "vendor_id": format!("{:04x}", device.vendor_id()),
                "product_id": format!("{:04x}", device.product_id()),
                "interface": device.interface_number(),
                "usage_page": format!("{:04x}", device.usage_page()),
                "usage": format!("{:04x}", device.usage()),
                "manufacturer": device.manufacturer_string().unwrap_or(""),
                "product": device.product_string().unwrap_or(""),
                "score": {
                    "tuple": format!("{:?}", lighting_device_score(device)),
                },
                "caps": caps,
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "detected": detect_lighting_device(),
        "interfaces": devices,
    })
}

fn write_json_file(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    fs::write(path, text).map_err(|error| format!("{}: {error}", path.display()))
}

fn redact_json_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    if lower.contains("path")
                        || lower.contains("endpoint_id")
                        || lower.ends_with("_id")
                    {
                        (key, serde_json::Value::String("<redacted>".to_string()))
                    } else {
                        (key, redact_json_value(value))
                    }
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(redact_json_value).collect())
        }
        other => other,
    }
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    if let Err(error) = run_service_worker() {
        log_event(
            "error",
            "service.worker.error",
            &[("message", error.to_string())],
        );
    }
}

fn install_windows_service() -> Result<(), String> {
    let executable_path = env::current_exe().map_err(|error| error.to_string())?;
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(windows_service_error)?;

    let service_info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path,
        launch_arguments: vec![OsString::from("service-run")],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };

    let service = manager
        .create_service(
            &service_info,
            ServiceAccess::CHANGE_CONFIG | ServiceAccess::QUERY_STATUS,
        )
        .map_err(windows_service_error)?;
    service
        .set_description(SERVICE_DESCRIPTION)
        .map_err(windows_service_error)?;
    let event_message_file = register_event_log_source()?;
    update_service_config(true)?;
    println!(
        "Installed {SERVICE_DISPLAY_NAME} as auto-start service. Event Viewer source uses {event_message_file}."
    );
    log_event("info", "service.install", &[]);
    Ok(())
}

fn uninstall_windows_service() -> Result<(), String> {
    let manager = service_manager(ServiceManagerAccess::CONNECT)?;
    let service = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
        )
        .map_err(windows_service_error)?;

    let status = service.query_status().map_err(windows_service_error)?;
    if status.current_state != ServiceState::Stopped {
        let _ = service.stop();
    }
    service.delete().map_err(windows_service_error)?;
    update_service_config(false)?;
    println!("Uninstalled {SERVICE_DISPLAY_NAME}. It may disappear after it fully stops.");
    log_event("info", "service.uninstall", &[]);
    Ok(())
}

fn start_installed_service() -> Result<(), String> {
    let service = open_installed_service(ServiceAccess::START | ServiceAccess::QUERY_STATUS)?;
    service.start::<&str>(&[]).map_err(windows_service_error)?;
    println!("Start requested for {SERVICE_DISPLAY_NAME}.");
    log_event("info", "service.start.request", &[]);
    Ok(())
}

fn stop_installed_service() -> Result<(), String> {
    let service = open_installed_service(ServiceAccess::STOP | ServiceAccess::QUERY_STATUS)?;
    let status = service.stop().map_err(windows_service_error)?;
    println!(
        "Stop requested for {SERVICE_DISPLAY_NAME}; current state: {}.",
        service_state_label(status.current_state)
    );
    log_event("info", "service.stop.request", &[]);
    Ok(())
}

fn print_installed_service_status() -> Result<(), String> {
    let service = open_installed_service(ServiceAccess::QUERY_STATUS)?;
    let status = service.query_status().map_err(windows_service_error)?;
    let output = serde_json::json!({
        "name": SERVICE_NAME,
        "display_name": SERVICE_DISPLAY_NAME,
        "state": service_state_label(status.current_state),
        "pid": status.process_id.unwrap_or(0),
        "ownership": service_ownership_plan_json(),
        "health": read_service_health().ok(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn print_service_ownership_plan() -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(&service_ownership_plan_json())
            .map_err(|error| error.to_string())?
    );
    Ok(())
}

fn service_ownership_plan_json() -> serde_json::Value {
    let config = load_or_create_config().unwrap_or_else(|_| AppConfig::default());
    serde_json::json!({
        "service_owns_now": [
            "install/start/stop/status lifecycle",
            "boot-time microphone restore when service.restore_on_startup is enabled",
            "service health heartbeat",
            "Event Viewer source registration during service install"
        ],
        "user_session_owns_now": [
            "GUI rendering and user interaction",
            "per-user GUI startup",
            "interactive lighting effect streams",
            "physical HID mute/pattern monitoring for UI refresh"
        ],
        "planned_service_candidates": [
            "lighting loop/effects only if we want effects without the GUI running",
            "HID monitoring only if a future background policy needs physical control events",
            "tray/GUI handoff via a separate per-user tray process, not directly from the service"
        ],
        "current_flags": {
            "enabled": config.service.enabled,
            "restore_on_startup": config.service.restore_on_startup,
            "owns_startup_restore": config.service.owns_startup_restore,
            "owns_lighting_loop": config.service.owns_lighting_loop,
            "owns_hid_monitoring": config.service.owns_hid_monitoring,
            "owns_tray_handoff": config.service.owns_tray_handoff
        },
        "decision": "Keep the Windows service small: boot restore and health only. Keep GUI, lighting streams, HID UI refresh, and tray behavior in the logged-in user session until there is a clear reason to move them."
    })
}

fn run_windows_service() -> Result<(), windows_service::Error> {
    log_event("info", "service.dispatcher.start", &[]);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

fn run_service_worker_console() -> Result<(), String> {
    println!("Running service worker in console mode. Press Ctrl+C to stop.");
    let mut health = ServiceHealth::new("console_running");
    restore_service_settings()?;
    health.last_restore = Some(log_timestamp());
    health.restore_on_startup = load_or_create_config()
        .map(|config| config.service.restore_on_startup)
        .unwrap_or(false);
    let _ = write_service_health(&health);
    log_event("info", "service.console.running", &[]);
    loop {
        thread::sleep(Duration::from_secs(5));
        health.heartbeat_count += 1;
        health.updated_at = log_timestamp();
        let _ = write_service_health(&health);
        log_event(
            "info",
            "service.console.heartbeat",
            &[("count", health.heartbeat_count.to_string())],
        );
    }
}

fn run_service_worker() -> Result<(), windows_service::Error> {
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = shutdown_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)?;
    set_service_status(
        &status_handle,
        ServiceState::StartPending,
        1,
        Duration::from_secs(10),
    )?;

    let mut health = ServiceHealth::new("starting");
    health.restore_on_startup = load_or_create_config()
        .map(|config| config.service.restore_on_startup)
        .unwrap_or(false);
    let _ = write_service_health(&health);

    if let Err(error) = restore_service_settings() {
        health.last_error = Some(error.to_string());
        log_event(
            "error",
            "service.restore.error",
            &[("message", error.to_string())],
        );
    } else {
        health.last_restore = Some(log_timestamp());
    }

    set_service_status(
        &status_handle,
        ServiceState::Running,
        0,
        Duration::default(),
    )?;
    health.state = "running".to_string();
    health.updated_at = log_timestamp();
    let _ = write_service_health(&health);
    log_event("info", "service.running", &[]);

    loop {
        match shutdown_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                health.heartbeat_count += 1;
                health.updated_at = log_timestamp();
                let _ = write_service_health(&health);
                log_event(
                    "info",
                    "service.heartbeat",
                    &[("count", health.heartbeat_count.to_string())],
                );
            }
        }
    }

    set_service_status(
        &status_handle,
        ServiceState::StopPending,
        1,
        Duration::from_secs(5),
    )?;
    health.state = "stop_pending".to_string();
    health.updated_at = log_timestamp();
    let _ = write_service_health(&health);
    log_event("info", "service.stopping", &[]);
    set_service_status(
        &status_handle,
        ServiceState::Stopped,
        0,
        Duration::default(),
    )?;
    health.state = "stopped".to_string();
    health.updated_at = log_timestamp();
    let _ = write_service_health(&health);
    log_event("info", "service.stopped", &[]);
    Ok(())
}

fn restore_service_settings() -> Result<(), String> {
    let _com = ComApartment::init().map_err(|error| error.to_string())?;
    let config = load_or_create_config()?;
    if config.service.restore_on_startup {
        set_mic_volume_percent(config.audio.mic_volume).map_err(|error| error.to_string())?;
        set_mic_mute(config.audio.mute_on_app_start).map_err(|error| error.to_string())?;
        log_event(
            "info",
            "service.restore.audio",
            &[
                ("mic_volume", config.audio.mic_volume.to_string()),
                ("muted", config.audio.mute_on_app_start.to_string()),
            ],
        );
    } else {
        log_event("info", "service.restore.skipped", &[]);
    }
    Ok(())
}

fn set_service_status(
    status_handle: &service_control_handler::ServiceStatusHandle,
    current_state: ServiceState,
    checkpoint: u32,
    wait_hint: Duration,
) -> Result<(), windows_service::Error> {
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state,
        controls_accepted: if current_state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::Win32(0),
        checkpoint,
        wait_hint,
        process_id: None,
    })
}

fn service_manager(access: ServiceManagerAccess) -> Result<ServiceManager, String> {
    ServiceManager::local_computer(None::<&str>, access).map_err(windows_service_error)
}

fn open_installed_service(
    access: ServiceAccess,
) -> Result<windows_service::service::Service, String> {
    service_manager(ServiceManagerAccess::CONNECT)?
        .open_service(SERVICE_NAME, access)
        .map_err(windows_service_error)
}

fn update_service_config(enabled: bool) -> Result<(), String> {
    let mut config = load_or_create_config()?;
    config.service.enabled = enabled;
    save_config(&config)
}

fn read_service_health() -> Result<ServiceHealth, String> {
    let path = service_health_path();
    let text = fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
}

fn write_service_health(health: &ServiceHealth) -> Result<(), String> {
    let path = service_health_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(health).map_err(|error| error.to_string())?;
    fs::write(&path, text).map_err(|error| format!("{}: {error}", path.display()))
}

fn service_state_label(state: ServiceState) -> &'static str {
    match state {
        ServiceState::Stopped => "stopped",
        ServiceState::StartPending => "start_pending",
        ServiceState::StopPending => "stop_pending",
        ServiceState::Running => "running",
        ServiceState::ContinuePending => "continue_pending",
        ServiceState::PausePending => "pause_pending",
        ServiceState::Paused => "paused",
    }
}

fn windows_service_error(error: windows_service::Error) -> String {
    match error {
        windows_service::Error::Winapi(io_error) => match io_error.raw_os_error() {
            Some(5) => "Access denied. Run this command from an elevated terminal.".to_string(),
            Some(1060) => format!("{SERVICE_DISPLAY_NAME} is not installed."),
            _ => format!("{io_error}"),
        },
        other => format!("{other}"),
    }
}

fn run_logs_command(args: &[String]) {
    if args.is_empty() {
        logs_usage();
        process::exit(2);
    }

    let result = match args[0].as_str() {
        "path" => {
            println!("{}", log_file_path().display());
            Ok(())
        }
        "tail" => {
            let lines = args
                .get(1)
                .map(|value| value.parse::<usize>())
                .transpose()
                .unwrap_or_else(|_| {
                    eprintln!("Line count must be a whole number.");
                    process::exit(2);
                })
                .unwrap_or(80);
            tail_log(lines)
        }
        _ => {
            logs_usage();
            process::exit(2);
        }
    };

    if let Err(error) = result {
        eprintln!("{error}");
        process::exit(1);
    }
}

fn logs_usage() {
    eprintln!(
        "Usage:\n\
  hyperx-mic-lite logs path\n\
  hyperx-mic-lite logs tail [lines]"
    );
}

fn tail_log(lines: usize) -> Result<(), String> {
    let path = log_file_path();
    let mut file = OpenOptions::new()
        .read(true)
        .open(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let len = file
        .metadata()
        .map_err(|error| format!("{}: {error}", path.display()))?
        .len();
    let start = len.saturating_sub(64 * 1024);
    file.seek(SeekFrom::Start(start))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut output = text.lines().rev().take(lines).collect::<Vec<_>>();
    output.reverse();
    for line in output {
        println!("{line}");
    }
    Ok(())
}

fn run_config_command(args: &[String]) {
    if args.is_empty() {
        config_usage();
        process::exit(2);
    }

    let result = match args[0].as_str() {
        "path" => {
            println!("{}", config_path().display());
            Ok(())
        }
        "dump" => match load_or_create_config() {
            Ok(config) => print_config_json(&config),
            Err(error) => Err(error),
        },
        "export" => {
            if args.len() != 2 {
                Err("Usage: hyperx-mic-lite config export <file>".to_string())
            } else {
                export_config(Path::new(&args[1]))
            }
        }
        "import" => {
            if args.len() != 2 {
                Err("Usage: hyperx-mic-lite config import <file>".to_string())
            } else {
                import_config(Path::new(&args[1]))
            }
        }
        "validate" => {
            let path = if args.len() == 2 {
                PathBuf::from(&args[1])
            } else {
                config_path()
            };
            validate_config_file(&path)
        }
        "reset" => reset_config(),
        _ => {
            config_usage();
            process::exit(2);
        }
    };

    if let Err(error) = result {
        eprintln!("{error}");
        process::exit(1);
    }
}

fn config_usage() {
    eprintln!(
        "Usage:\n\
  hyperx-mic-lite config path\n\
  hyperx-mic-lite config dump\n\
  hyperx-mic-lite config export <file>\n\
  hyperx-mic-lite config import <file>\n\
  hyperx-mic-lite config validate [file]\n\
  hyperx-mic-lite config reset"
    );
}

fn app_data_dir() -> PathBuf {
    env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(APP_NAME)
}

fn config_dir() -> PathBuf {
    app_data_dir()
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

fn load_or_create_config() -> Result<AppConfig, String> {
    let path = config_path();
    if !path.exists() {
        let config = AppConfig::default();
        save_config(&config)?;
        return Ok(config);
    }
    load_config_from_path(&path)
}

fn load_config_from_path(path: &Path) -> Result<AppConfig, String> {
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

fn save_config(config: &AppConfig) -> Result<(), String> {
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

fn print_config_json(config: &AppConfig) -> Result<(), String> {
    let text = serde_json::to_string_pretty(config).map_err(|error| error.to_string())?;
    println!("{text}");
    Ok(())
}

fn export_config(destination: &Path) -> Result<(), String> {
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

fn import_config(source: &Path) -> Result<(), String> {
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

fn validate_config_file(path: &Path) -> Result<(), String> {
    let config = load_config_from_path(path)?;
    config.validate()?;
    println!("Config is valid: {}", path.display());
    Ok(())
}

fn reset_config() -> Result<(), String> {
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

fn unix_timestamp_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn print_lighting_detection() {
    match detect_lighting_device() {
        Some(device) => {
            log_event(
                "info",
                "lighting.detect",
                &[
                    ("vendor_id", format!("{:04x}", device.vendor_id)),
                    ("product_id", format!("{:04x}", device.product_id)),
                    ("interface", device.interface_number.to_string()),
                    ("usage_page", format!("{:04x}", device.usage_page)),
                    ("usage", format!("{:04x}", device.usage)),
                ],
            );
            println!("Lighting controller detected:");
            println!(
                "  VID:PID: {:04x}:{:04x}",
                device.vendor_id, device.product_id
            );
            println!("  Interface: {}", device.interface_number);
            println!("  Usage: {:04x}:{:04x}", device.usage_page, device.usage);
            println!("  Name: {} {}", device.manufacturer, device.product);
        }
        None => {
            log_event("warn", "lighting.detect.none", &[]);
            println!("No supported HyperX QuadCast S lighting controller was detected.");
        }
    }
}

fn print_lighting_hid_dump() {
    let api = match hidapi::HidApi::new() {
        Ok(api) => api,
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    };

    for device in api
        .device_list()
        .filter(|device| is_supported_lighting_device(device))
    {
        println!(
            "{:04x}:{:04x} interface {} usage {:04x}:{:04x}",
            device.vendor_id(),
            device.product_id(),
            device.interface_number(),
            device.usage_page(),
            device.usage()
        );
        println!(
            "  {} {}",
            device.manufacturer_string().unwrap_or(""),
            device.product_string().unwrap_or("")
        );
        match hid_caps_for_path(device.path()) {
            Ok(caps) => {
                println!(
                    "  reports: input={} output={} feature={}",
                    caps.InputReportByteLength,
                    caps.OutputReportByteLength,
                    caps.FeatureReportByteLength
                );
                println!(
                    "  caps: input-values={} output-values={} feature-values={}",
                    caps.NumberInputValueCaps,
                    caps.NumberOutputValueCaps,
                    caps.NumberFeatureValueCaps
                );
            }
            Err(error) => println!("  caps: {error}"),
        }
    }
}

fn hid_caps_for_path(path: &CStr) -> Result<HIDP_CAPS, String> {
    let path = path.to_string_lossy();
    let wide_path = path.encode_utf16().chain([0]).collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide_path.as_ptr()),
            0,
            FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0),
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|error| error.to_string())?;

    let mut preparsed: PHIDP_PREPARSED_DATA = PHIDP_PREPARSED_DATA::default();
    let result = unsafe {
        if !HidD_GetPreparsedData(handle, &mut preparsed) {
            let _ = CloseHandle(handle);
            return Err("HidD_GetPreparsedData failed".to_string());
        }

        let mut caps = HIDP_CAPS::default();
        let status = HidP_GetCaps(preparsed, &mut caps);
        HidD_FreePreparsedData(preparsed);
        let _ = CloseHandle(handle);

        if status == HIDP_STATUS_SUCCESS {
            Ok(caps)
        } else {
            Err(format!(
                "HidP_GetCaps failed with NTSTATUS 0x{:08x}",
                status.0
            ))
        }
    };

    result
}

fn detect_lighting_device() -> Option<LightingDevice> {
    let api = hidapi::HidApi::new().ok()?;
    let supported = [
        (0x0951, 0x171f),
        (0x03f0, 0x0f8b),
        (0x03f0, 0x028c),
        (0x03f0, 0x048c),
        (0x03f0, 0x068c),
        (0x03f0, 0x098c),
    ];

    api.device_list()
        .filter(|device| {
            supported.iter().any(|(vendor, product)| {
                device.vendor_id() == *vendor && device.product_id() == *product
            })
        })
        .max_by_key(|device| lighting_device_score(device))
        .map(|device| LightingDevice {
            vendor_id: device.vendor_id(),
            product_id: device.product_id(),
            interface_number: device.interface_number(),
            usage_page: device.usage_page(),
            usage: device.usage(),
            manufacturer: device.manufacturer_string().unwrap_or("HyperX").to_string(),
            product: device.product_string().unwrap_or("QuadCast S").to_string(),
        })
}

fn is_supported_lighting_device(device: &hidapi::DeviceInfo) -> bool {
    matches!(
        (device.vendor_id(), device.product_id()),
        (0x0951, 0x171f)
            | (0x03f0, 0x0f8b)
            | (0x03f0, 0x028c)
            | (0x03f0, 0x048c)
            | (0x03f0, 0x068c)
            | (0x03f0, 0x098c)
    )
}

fn lighting_device_score(device: &hidapi::DeviceInfo) -> (u8, u8, u8, i32) {
    let rgb_collection = u8::from(device.usage_page() == 0xff90 && device.usage() == 0xff00);
    let vendor_defined = u8::from(
        device.usage_page() == 0xff90
            || device.usage_page() == 0xff00
            || device.usage_page() == 0xffff,
    );
    let controller_pid = u8::from(device.product_id() == 0x171f);
    let interface_preference = if device.interface_number() == 0 { 1 } else { 0 };
    (
        rgb_collection,
        vendor_defined,
        controller_pid,
        interface_preference,
    )
}

fn run_lighting_solid(args: &[String]) {
    let (args, packet_log) = split_packet_log_flag(args);
    if args.is_empty() || args.len() > 2 {
        eprintln!("Usage: hyperx-mic-lite lighting-solid ff0066 [seconds] [--packet-log]");
        process::exit(2);
    }

    let color = parse_rgb_hex(args[0]).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(2);
    });

    let seconds = args
        .get(1)
        .map(|value| value.parse::<u64>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("Duration must be a whole number of seconds.");
            process::exit(2);
        })
        .unwrap_or(3)
        .clamp(1, 60);

    let program = LightingProgram {
        effect: Effect::Solid,
        target: LightTarget::All,
        colors: vec![color],
        speed: 75,
        brightness: 100,
    };

    match stream_lighting_program(
        &program,
        StreamDuration::Timed(Duration::from_secs(seconds)),
        packet_log,
    ) {
        Ok(()) => {
            log_event(
                "info",
                "lighting.solid",
                &[
                    (
                        "color",
                        format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]),
                    ),
                    ("seconds", seconds.to_string()),
                ],
            );
            println!(
                "Streamed solid color #{:02x}{:02x}{:02x} for {seconds}s",
                color[0], color[1], color[2],
            )
        }
        Err(error) => {
            log_event(
                "error",
                "lighting.solid.error",
                &[("message", error.clone())],
            );
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

fn run_lighting_effect(args: &[String]) {
    let (args, packet_log) = split_packet_log_flag(args);
    if args.is_empty() || args.len() > 2 {
        eprintln!(
            "Usage: hyperx-mic-lite lighting-effect <solid|wave|cycle|pulse|blink|lightning|vu_meter> [seconds|forever] [--packet-log]"
        );
        process::exit(2);
    }

    let effect = Effect::from_config(args[0]);
    let stream_duration = args
        .get(1)
        .map(|value| parse_stream_duration(value))
        .transpose()
        .unwrap_or_else(|error| {
            eprintln!("{error}");
            process::exit(2);
        })
        .unwrap_or(StreamDuration::Forever);
    let config = load_or_create_config().unwrap_or_else(|_| AppConfig::default());
    let colors = config
        .lighting
        .colors
        .iter()
        .filter_map(|color| parse_rgb_hex(color).ok())
        .collect::<Vec<_>>();
    let program = LightingProgram {
        effect,
        target: LightTarget::from_config(&config.lighting.target),
        colors: if colors.is_empty() {
            vec![[0, 255, 0]]
        } else {
            colors
        },
        speed: config.lighting.speed,
        brightness: config.lighting.brightness,
    };

    match stream_lighting_program(&program, stream_duration, packet_log) {
        Ok(()) => println!("Streamed {}", effect.label()),
        Err(error) => {
            log_event(
                "error",
                "lighting.effect.error",
                &[("message", error.clone())],
            );
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

fn run_lighting_vu_test(args: &[String]) {
    let (args, packet_log) = split_packet_log_flag(args);
    if args.is_empty() || args.len() > 2 {
        eprintln!("Usage: hyperx-mic-lite lighting-vu-test <0-100> [seconds] [--packet-log]");
        process::exit(2);
    }

    let level = args[0].parse::<u8>().unwrap_or_else(|_| {
        eprintln!("Level must be a whole number from 0 to 100.");
        process::exit(2);
    });
    if level > 100 {
        eprintln!("Level must be a whole number from 0 to 100.");
        process::exit(2);
    }
    let seconds = args
        .get(1)
        .map(|value| value.parse::<u64>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("Duration must be a whole number of seconds.");
            process::exit(2);
        })
        .unwrap_or(2)
        .clamp(1, 30);

    let config = load_or_create_config().unwrap_or_else(|_| AppConfig::default());
    let frame = build_vu_frame(level as f32 / 100.0, config.lighting.brightness);
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        if let Err(error) = write_lighting_frame_once(frame, packet_log) {
            eprintln!("{error}");
            process::exit(1);
        }
        thread::sleep(Duration::from_millis(80));
    }
    println!("Streamed VU test level {level}% for {seconds}s");
}

fn run_lighting_save(args: &[String]) {
    let (args, packet_log) = split_packet_log_flag(args);
    if !args.is_empty() {
        eprintln!("Usage: hyperx-mic-lite lighting-save [--packet-log]");
        process::exit(2);
    }

    match save_lighting_to_microphone(packet_log) {
        Ok(()) => {
            log_event("info", "lighting.save.done", &[]);
            println!("Sent experimental Save to Microphone command sequence.");
        }
        Err(error) => {
            log_event(
                "error",
                "lighting.save.error",
                &[("message", error.to_string())],
            );
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

fn split_packet_log_flag(args: &[String]) -> (Vec<&str>, bool) {
    let mut packet_log = false;
    let mut filtered = Vec::new();
    for arg in args {
        if arg == "--packet-log" || arg == "--verbose-hid" {
            packet_log = true;
        } else {
            filtered.push(arg.as_str());
        }
    }
    (filtered, packet_log)
}

fn parse_stream_duration(value: &str) -> Result<StreamDuration, String> {
    if value.eq_ignore_ascii_case("forever") {
        return Ok(StreamDuration::Forever);
    }
    let seconds = value
        .parse::<u64>()
        .map_err(|_| "Duration must be seconds or 'forever'.".to_string())?
        .clamp(1, 120);
    Ok(StreamDuration::Timed(Duration::from_secs(seconds)))
}

fn run_hid_monitor(args: &[String]) {
    if args.len() > 1 {
        eprintln!("Usage: hyperx-mic-lite hid-monitor [seconds]");
        process::exit(2);
    }

    let seconds = args
        .first()
        .map(|value| value.parse::<u64>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("Duration must be a whole number of seconds.");
            process::exit(2);
        })
        .unwrap_or(15)
        .clamp(1, 120);

    if let Err(error) = monitor_hid_reports(Duration::from_secs(seconds)) {
        eprintln!("{error}");
        process::exit(1);
    }
}

fn run_level_monitor(args: &[String]) {
    if args.len() > 1 {
        eprintln!("Usage: hyperx-mic-lite level-monitor [seconds]");
        process::exit(2);
    }

    let seconds = args
        .first()
        .map(|value| value.parse::<u64>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("Duration must be a whole number of seconds.");
            process::exit(2);
        })
        .unwrap_or(15)
        .clamp(1, 120);

    let capture_monitor = match start_audio_peak_monitor() {
        Ok(monitor) => {
            println!("Using direct input capture peak monitor.");
            Some(monitor)
        }
        Err(error) => {
            println!("Direct input capture unavailable: {error}");
            println!("Falling back to Windows endpoint meter.");
            None
        }
    };

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        if let Some(monitor) = &capture_monitor {
            println!(
                "{:>6}ms input-peak={:.3}",
                started.elapsed().as_millis(),
                monitor.peak()
            );
        } else {
            match input_peak_value() {
                Ok(peak) => println!(
                    "{:>6}ms endpoint-peak={:.3}",
                    started.elapsed().as_millis(),
                    peak
                ),
                Err(error) => println!(
                    "{:>6}ms endpoint-peak-error={error}",
                    started.elapsed().as_millis()
                ),
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn monitor_hid_reports(duration: Duration) -> Result<(), String> {
    let api = hidapi::HidApi::new().map_err(|error| error.to_string())?;
    let mut devices = Vec::new();

    for info in api
        .device_list()
        .filter(|device| is_supported_lighting_device(device))
    {
        if let Ok(device) = api.open_path(info.path()) {
            device
                .set_blocking_mode(false)
                .map_err(|error| error.to_string())?;
            devices.push((
                format!(
                    "{:04x}:{:04x} if{} {:04x}:{:04x}",
                    info.vendor_id(),
                    info.product_id(),
                    info.interface_number(),
                    info.usage_page(),
                    info.usage()
                ),
                device,
                hid_caps_for_path(info.path()).ok(),
            ));
        }
    }

    if devices.is_empty() {
        return Err("No supported QuadCast S HID interfaces could be opened.".to_string());
    }

    println!(
        "Monitoring {} HID interface(s) for {}s.",
        devices.len(),
        duration.as_secs()
    );
    println!(
        "Press one physical control at a time: mute, pattern positions, then turn the gain dial."
    );

    let started = std::time::Instant::now();
    let mut last_volume = mic_status().ok().map(|status| status.volume);
    if let Some(volume) = last_volume {
        println!("{:>6}ms core-audio mic-volume={volume}", 0);
    }

    let mut buffers = devices
        .iter()
        .map(|(_, _, caps)| {
            let len = caps
                .as_ref()
                .map(|caps| caps.InputReportByteLength as usize)
                .unwrap_or(65)
                .max(65);
            vec![0u8; len]
        })
        .collect::<Vec<_>>();

    while started.elapsed() < duration {
        for (index, (label, device, _)) in devices.iter().enumerate() {
            match device.read_timeout(&mut buffers[index], 10) {
                Ok(0) => {}
                Ok(count) => {
                    let elapsed = started.elapsed().as_millis();
                    let decoded = decode_hid_report(&buffers[index][..count])
                        .map(|value| format!(" {value}"))
                        .unwrap_or_default();
                    println!(
                        "{elapsed:>6}ms {label} {}{}",
                        hex_bytes(&buffers[index][..count]),
                        decoded
                    );
                }
                Err(error) => {
                    let elapsed = started.elapsed().as_millis();
                    println!("{elapsed:>6}ms {label} read-error: {error}");
                }
            }
        }

        if let Ok(status) = mic_status() {
            if last_volume != Some(status.volume) {
                let elapsed = started.elapsed().as_millis();
                println!("{elapsed:>6}ms core-audio mic-volume={}", status.volume);
                last_volume = Some(status.volume);
            }
        }

        thread::sleep(Duration::from_millis(10));
    }

    Ok(())
}

fn spawn_hid_event_listener() -> Receiver<HidEvent> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let api = match hidapi::HidApi::new() {
            Ok(api) => api,
            Err(_) => return,
        };

        let mut devices = Vec::new();
        for info in api
            .device_list()
            .filter(|device| is_supported_lighting_device(device))
        {
            if info.usage_page() != 0xffff || info.usage() != 0x0001 {
                continue;
            }
            if let Ok(device) = api.open_path(info.path()) {
                let _ = device.set_blocking_mode(false);
                devices.push(device);
            }
        }

        let mut buffer = [0u8; 65];
        loop {
            for device in &devices {
                if let Ok(count) = device.read_timeout(&mut buffer, 50) {
                    if count > 0 {
                        match decode_hid_event(&buffer[..count]) {
                            Some(HidEvent::Mute(is_live)) => {
                                let _ = sender.send(HidEvent::Mute(is_live));
                            }
                            Some(HidEvent::Pattern(pattern)) => {
                                let _ = sender.send(HidEvent::Pattern(pattern));
                            }
                            None => {}
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
    });
    receiver
}

fn decode_hid_event(report: &[u8]) -> Option<HidEvent> {
    match report {
        [0x05, 0x10, value, ..] => Some(HidEvent::Mute(*value == 1)),
        [0x05, 0x11, value, ..] => Some(HidEvent::Pattern(PolarPattern::from_report(*value))),
        _ => None,
    }
}

fn decode_hid_report(report: &[u8]) -> Option<String> {
    match decode_hid_event(report)? {
        HidEvent::Mute(is_live) => Some(format!(
            "mute={}",
            if is_live { "live/unmuted" } else { "muted" }
        )),
        HidEvent::Pattern(pattern) => Some(format!("pattern={}", pattern.label())),
    }
}

fn pattern_description(pattern: PolarPattern) -> &'static str {
    match pattern {
        PolarPattern::Stereo => "Captures left and right channels for wider sources.",
        PolarPattern::Omni => "Captures sound evenly from around the microphone.",
        PolarPattern::Cardioid => "Best for podcasts, streaming, voiceovers and instruments.",
        PolarPattern::Bidirectional => "Captures from the front and back for interviews.",
        PolarPattern::Unknown(_) => "Unrecognized hardware pattern report.",
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_rgb_hex(value: &str) -> Result<[u8; 3], String> {
    let trimmed = value.trim().trim_start_matches('#');
    if trimmed.len() != 6 {
        return Err("Color must be six hex digits, for example ff0066.".to_string());
    }

    let parsed = u32::from_str_radix(trimmed, 16)
        .map_err(|_| "Color must contain only hex digits.".to_string())?;
    Ok([
        ((parsed >> 16) & 0xff) as u8,
        ((parsed >> 8) & 0xff) as u8,
        (parsed & 0xff) as u8,
    ])
}

fn color_to_hex(color: egui::Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

fn stream_lighting_program(
    program: &LightingProgram,
    duration: StreamDuration,
    packet_log: bool,
) -> Result<(), String> {
    stream_lighting_program_cancelable(program, duration, None, packet_log)
}

fn stream_lighting_program_cancelable(
    program: &LightingProgram,
    duration: StreamDuration,
    cancel: Option<Arc<AtomicBool>>,
    packet_log: bool,
) -> Result<(), String> {
    let _com = if program.effect == Effect::VuMeter {
        match ComApartment::init() {
            Ok(com) => Some(com),
            Err(error) => {
                log_event(
                    "warn",
                    "lighting.vu.com_init.error",
                    &[("message", error.to_string())],
                );
                None
            }
        }
    } else {
        None
    };
    let api = hidapi::HidApi::new().map_err(|error| error.to_string())?;
    let info = api
        .device_list()
        .filter(|device| is_supported_lighting_device(device))
        .max_by_key(|device| lighting_device_score(device))
        .ok_or_else(|| "No supported QuadCast S lighting HID interface detected.".to_string())?;

    let device = api
        .open_path(info.path())
        .map_err(|error| error.to_string())?;

    let header = build_display_header_packet();
    let frames = build_effect_frames(program);
    if frames.is_empty() {
        return Err("No lighting frames were generated.".to_string());
    }
    let started = std::time::Instant::now();
    let mut index = 0usize;
    let frame_delay = effect_frame_delay(program.speed);
    let mut vu_level = 0.18f32;
    let mut meter_error_logged = false;
    let capture_monitor = if program.effect == Effect::VuMeter {
        match start_audio_peak_monitor() {
            Ok(monitor) => {
                log_event("info", "lighting.vu.capture.start", &[]);
                Some(monitor)
            }
            Err(error) => {
                log_event("warn", "lighting.vu.capture.error", &[("message", error)]);
                None
            }
        }
    } else {
        None
    };

    while match duration {
        StreamDuration::Timed(duration) => started.elapsed() < duration,
        StreamDuration::Forever => true,
    } {
        if cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        {
            break;
        }
        let frame = if program.effect == Effect::VuMeter {
            let peak = if let Some(monitor) = &capture_monitor {
                monitor.peak()
            } else {
                match input_peak_value() {
                    Ok(peak) => peak,
                    Err(error) => {
                        if !meter_error_logged {
                            log_event(
                                "warn",
                                "lighting.vu.meter.error",
                                &[("message", error.to_string())],
                            );
                            meter_error_logged = true;
                        }
                        0.0
                    }
                }
            };
            let target = peak.sqrt().clamp(0.0, 1.0);
            vu_level = smooth_vu_level(vu_level, target);
            build_vu_frame(vu_level, program.brightness)
        } else {
            let frame = frames[index % frames.len()];
            index += 1;
            frame
        };
        send_feature_packet(&device, &header, packet_log)?;
        send_feature_packet(&device, &build_frame_packet(frame), packet_log)?;
        thread::sleep(frame_delay);
    }

    Ok(())
}

fn build_display_header_packet() -> [u8; 64] {
    let mut packet = [0u8; 64];
    packet[0] = 0x04;
    packet[1] = 0xf2;
    packet[8] = 0x01;
    packet
}

fn build_frame_packet(frame: LightingFrame) -> [u8; 64] {
    let mut packet = [0u8; 64];
    for (index, color) in frame.iter().enumerate() {
        let offset = index * 4;
        packet[offset] = 0x81;
        packet[offset + 1] = color[0];
        packet[offset + 2] = color[1];
        packet[offset + 3] = color[2];
    }
    packet
}

fn build_save_prepare_packet() -> [u8; 64] {
    let mut packet = [0u8; 64];
    packet[0] = 0x04;
    packet[1] = 0x53;
    packet[8] = 0x01;
    packet
}

fn build_save_state_packet() -> [u8; 64] {
    let mut packet = [0u8; 64];
    packet[0] = 0x04;
    packet[1] = 0x02;
    packet
}

fn build_save_commit_packet() -> [u8; 64] {
    let mut packet = [0u8; 64];
    packet[0] = 0x04;
    packet[1] = 0x23;
    packet[8] = 0x01;
    packet
}

fn build_save_sentinel_packet() -> [u8; 64] {
    let mut packet = [0u8; 64];
    packet[0] = 0x08;
    packet[59] = 0x28;
    packet[60] = 0x01;
    packet[61] = 0x00;
    packet[62] = 0xaa;
    packet[63] = 0x55;
    packet
}

fn build_effect_frames(program: &LightingProgram) -> Vec<LightingFrame> {
    let colors = normalized_colors(program);
    match program.effect {
        Effect::Solid => vec![solid_frame(colors[0])],
        Effect::Cycle => cycle_frames(&colors, program.speed, false),
        Effect::Wave => cycle_frames(&colors, program.speed, true),
        Effect::Pulse => pulse_frames(&colors, program.speed),
        Effect::Blink => blink_frames(&colors, program.speed),
        Effect::Lightning => lightning_frames(&colors, program.speed),
        Effect::VuMeter => vec![solid_frame([0, 0, 0])],
    }
}

fn normalized_colors(program: &LightingProgram) -> Vec<[u8; 3]> {
    let colors = if program.colors.is_empty() {
        vec![[0, 255, 0]]
    } else {
        program.colors.clone()
    };
    colors
        .into_iter()
        .map(|color| scale_color(color, program.brightness))
        .collect()
}

fn effect_frame_delay(speed: u8) -> Duration {
    let millis = 140u64.saturating_sub(speed as u64);
    Duration::from_millis(millis.clamp(24, 140))
}

fn transition_steps(speed: u8, min: usize, max: usize) -> usize {
    let span = max.saturating_sub(min);
    max.saturating_sub((span * speed as usize) / 100).max(min)
}

fn solid_frame(color: [u8; 3]) -> LightingFrame {
    [color; LIGHTING_CELL_COUNT]
}

fn cycle_frames(colors: &[[u8; 3]], speed: u8, wave: bool) -> Vec<LightingFrame> {
    let steps = transition_steps(speed, 8, 48);
    let mut sequence = Vec::new();
    for index in 0..colors.len() {
        let start = colors[index];
        let end = colors[(index + 1) % colors.len()];
        for step in 0..steps {
            sequence.push(lerp_color(start, end, step, steps));
        }
    }

    if sequence.is_empty() {
        return vec![solid_frame([0, 0, 0])];
    }

    let wave_span = (sequence.len() / 2).max(1);
    sequence
        .iter()
        .enumerate()
        .map(|(index, color)| {
            let mut frame = solid_frame(*color);
            if wave {
                for (cell, slot) in frame.iter_mut().enumerate() {
                    let offset = (cell * wave_span) / LIGHTING_CELL_COUNT;
                    *slot = sequence[(index + offset) % sequence.len()];
                }
            }
            frame
        })
        .collect()
}

fn pulse_frames(colors: &[[u8; 3]], speed: u8) -> Vec<LightingFrame> {
    let steps = transition_steps(speed, 6, 36);
    let mut frames = Vec::new();
    for &color in colors {
        for step in 0..steps {
            let value = lerp_color([0, 0, 0], color, step, steps);
            frames.push(solid_frame(value));
        }
        for step in 0..steps {
            let value = lerp_color(color, [0, 0, 0], step, steps);
            frames.push(solid_frame(value));
        }
    }
    frames
}

fn blink_frames(colors: &[[u8; 3]], speed: u8) -> Vec<LightingFrame> {
    let lit = transition_steps(speed, 2, 16);
    let dark = transition_steps(speed, 2, 24);
    let mut frames = Vec::new();
    for &color in colors {
        for _ in 0..lit {
            frames.push(solid_frame(color));
        }
        for _ in 0..dark {
            frames.push(solid_frame([0, 0, 0]));
        }
    }
    frames
}

fn lightning_frames(colors: &[[u8; 3]], speed: u8) -> Vec<LightingFrame> {
    let flash = transition_steps(speed, 1, 6);
    let fade = transition_steps(speed, 8, 42);
    let mut frames = Vec::new();
    for &color in colors {
        for _ in 0..flash {
            let mut frame = solid_frame([0, 0, 0]);
            for cell in (0..LIGHTING_CELL_COUNT).step_by(3) {
                frame[cell] = color;
            }
            frames.push(frame);
        }
        for step in 0..fade {
            let value = lerp_color(color, [0, 0, 0], step, fade);
            frames.push(solid_frame(value));
        }
        frames.push(solid_frame([0, 0, 0]));
    }
    frames
}

fn smooth_vu_level(current: f32, target: f32) -> f32 {
    let coefficient = if target > current { 0.42 } else { 0.10 };
    current + (target - current) * coefficient
}

fn build_vu_frame(level: f32, brightness: u8) -> LightingFrame {
    let level = level.clamp(0.0, 1.0);
    let mut frame = solid_frame([0, 0, 0]);
    let lit_cells =
        ((level * LIGHTING_CELL_COUNT as f32).ceil() as usize).clamp(1, LIGHTING_CELL_COUNT);
    for cell in 0..LIGHTING_CELL_COUNT {
        let height = (LIGHTING_CELL_COUNT - 1 - cell) as f32 / (LIGHTING_CELL_COUNT - 1) as f32;
        let threshold = cell + 1;
        if threshold <= lit_cells {
            let energy = (level * 1.25 - height * 0.18).clamp(0.20, 1.0);
            frame[LIGHTING_CELL_COUNT - 1 - cell] = vu_color(height.max(energy), brightness);
        }
    }
    frame
}

fn vu_color(strength: f32, brightness: u8) -> [u8; 3] {
    let base = if strength < 0.35 {
        lerp_color_float([45, 0, 0], [255, 42, 0], strength / 0.35)
    } else if strength < 0.72 {
        lerp_color_float([255, 42, 0], [255, 185, 0], (strength - 0.35) / 0.37)
    } else {
        lerp_color_float([255, 185, 0], [255, 255, 210], (strength - 0.72) / 0.28)
    };
    let effective = ((strength * brightness as f32).round() as u8).clamp(18, 100);
    scale_color(base, effective)
}

fn lerp_color(start: [u8; 3], end: [u8; 3], step: usize, steps: usize) -> [u8; 3] {
    let denominator = steps.saturating_sub(1).max(1) as f32;
    let t = step as f32 / denominator;
    [
        (start[0] as f32 + (end[0] as f32 - start[0] as f32) * t).round() as u8,
        (start[1] as f32 + (end[1] as f32 - start[1] as f32) * t).round() as u8,
        (start[2] as f32 + (end[2] as f32 - start[2] as f32) * t).round() as u8,
    ]
}

fn lerp_color_float(start: [u8; 3], end: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (start[0] as f32 + (end[0] as f32 - start[0] as f32) * t).round() as u8,
        (start[1] as f32 + (end[1] as f32 - start[1] as f32) * t).round() as u8,
        (start[2] as f32 + (end[2] as f32 - start[2] as f32) * t).round() as u8,
    ]
}

fn scale_color(color: [u8; 3], percent: u8) -> [u8; 3] {
    [
        ((color[0] as u16 * percent as u16) / 100) as u8,
        ((color[1] as u16 * percent as u16) / 100) as u8,
        ((color[2] as u16 * percent as u16) / 100) as u8,
    ]
}

fn send_feature_packet(
    device: &hidapi::HidDevice,
    packet: &[u8; 64],
    packet_log: bool,
) -> Result<(), String> {
    let mut with_report_id = [0u8; 65];
    with_report_id[1..].copy_from_slice(packet);

    let mut errors = Vec::new();

    if let Err(error) = device.send_feature_report(&with_report_id) {
        log_hid_packet_attempt(
            packet_log,
            "feature+id",
            false,
            packet,
            Some(error.to_string()),
        );
        errors.push(format!("feature+id: {error}"));
    } else {
        log_hid_packet_attempt(packet_log, "feature+id", true, packet, None);
        return Ok(());
    }

    if let Err(error) = device.send_feature_report(packet) {
        log_hid_packet_attempt(
            packet_log,
            "feature",
            false,
            packet,
            Some(error.to_string()),
        );
        errors.push(format!("feature: {error}"));
    } else {
        log_hid_packet_attempt(packet_log, "feature", true, packet, None);
        return Ok(());
    }

    if let Err(error) = device.write(&with_report_id) {
        log_hid_packet_attempt(
            packet_log,
            "output+id",
            false,
            packet,
            Some(error.to_string()),
        );
        errors.push(format!("output+id: {error}"));
    } else {
        log_hid_packet_attempt(packet_log, "output+id", true, packet, None);
        return Ok(());
    }

    if let Err(error) = device.write(packet) {
        log_hid_packet_attempt(packet_log, "output", false, packet, Some(error.to_string()));
        errors.push(format!("output: {error}"));
    } else {
        log_hid_packet_attempt(packet_log, "output", true, packet, None);
        return Ok(());
    }

    Err(errors.join("; "))
}

fn read_feature_packet(device: &hidapi::HidDevice, packet_log: bool) -> Result<[u8; 64], String> {
    let mut with_report_id = [0u8; 65];
    let mut errors = Vec::new();
    match device.get_feature_report(&mut with_report_id) {
        Ok(length) if length >= 65 => {
            let mut packet = [0u8; 64];
            packet.copy_from_slice(&with_report_id[1..65]);
            log_hid_packet_attempt(packet_log, "feature-read+id", true, &packet, None);
            return Ok(packet);
        }
        Ok(length) => errors.push(format!("feature-read+id: short report length {length}")),
        Err(error) => errors.push(format!("feature-read+id: {error}")),
    }

    let mut packet = [0u8; 64];
    match device.get_feature_report(&mut packet) {
        Ok(length) if length >= 64 => {
            log_hid_packet_attempt(packet_log, "feature-read", true, &packet, None);
            Ok(packet)
        }
        Ok(length) => Err(format!(
            "Unable to read feature report. Short report length {length}. Previous errors: {}",
            errors.join("; ")
        )),
        Err(error) => {
            errors.push(format!("feature-read: {error}"));
            Err(format!(
                "Unable to read feature report: {}",
                errors.join("; ")
            ))
        }
    }
}

fn save_lighting_to_microphone(packet_log: bool) -> Result<(), String> {
    let api = hidapi::HidApi::new().map_err(|error| error.to_string())?;
    let info = api
        .device_list()
        .filter(|device| is_supported_lighting_device(device))
        .max_by_key(|device| lighting_device_score(device))
        .ok_or_else(|| "No supported QuadCast S lighting HID interface detected.".to_string())?;

    let device = api
        .open_path(info.path())
        .map_err(|error| error.to_string())?;

    send_feature_packet(&device, &build_save_prepare_packet(), packet_log)?;
    send_feature_packet(&device, &build_save_state_packet(), packet_log)?;
    if let Err(error) = read_feature_packet(&device, packet_log) {
        log_event(
            "warn",
            "lighting.save.readback.error",
            &[("message", error)],
        );
    }
    send_feature_packet(&device, &build_save_commit_packet(), packet_log)?;
    if let Err(error) = read_feature_packet(&device, packet_log) {
        log_event(
            "warn",
            "lighting.save.readback.error",
            &[("message", error)],
        );
    }
    send_feature_packet(&device, &build_save_sentinel_packet(), packet_log)?;
    send_feature_packet(&device, &build_save_state_packet(), packet_log)?;
    if let Err(error) = read_feature_packet(&device, packet_log) {
        log_event(
            "warn",
            "lighting.save.readback.error",
            &[("message", error)],
        );
    }
    Ok(())
}

fn write_solid_lighting_once(
    color: [u8; 3],
    brightness: u8,
    packet_log: bool,
) -> Result<(), String> {
    let color = scale_color(color, brightness);
    write_lighting_frame_once(solid_frame(color), packet_log)
}

fn write_lighting_frame_once(frame: LightingFrame, packet_log: bool) -> Result<(), String> {
    let api = hidapi::HidApi::new().map_err(|error| error.to_string())?;
    let info = api
        .device_list()
        .filter(|device| is_supported_lighting_device(device))
        .max_by_key(|device| lighting_device_score(device))
        .ok_or_else(|| "No supported QuadCast S lighting HID interface detected.".to_string())?;
    let device = api
        .open_path(info.path())
        .map_err(|error| error.to_string())?;
    send_feature_packet(&device, &build_display_header_packet(), packet_log)?;
    send_feature_packet(&device, &build_frame_packet(frame), packet_log)
}

fn live_mute_lighting_color(is_live: bool) -> [u8; 3] {
    if is_live { [0, 255, 70] } else { [255, 0, 28] }
}

fn log_hid_packet_attempt(
    enabled: bool,
    transport: &str,
    success: bool,
    packet: &[u8; 64],
    error: Option<String>,
) {
    if !enabled {
        return;
    }

    let mut fields = vec![
        ("transport", transport.to_string()),
        ("success", success.to_string()),
        ("packet", hex_bytes(packet)),
    ];
    if let Some(error) = error {
        fields.push(("error", error));
    }
    log_event("info", "hid.packet.write", &fields);
}

impl MicLiteApp {
    fn new(start_minimized: bool) -> Self {
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
            polar_pattern: PolarPattern::Unknown(255),
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
            ui.add_space(24.0);
            let previous_tab = self.tab;
            tab_button(ui, &mut self.tab, Tab::Audio, "Audio");
            tab_button(ui, &mut self.tab, Tab::Lights, "Lights");
            if self.tab != previous_tab {
                self.save_config_snapshot();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Refresh").clicked() {
                    self.refresh_status();
                }
            });
        });
    }

    fn ui_audio(&mut self, ui: &mut egui::Ui) {
        self.drain_hid_events();
        self.refresh_status_periodic();
        self.refresh_input_peak();
        let muted = self.status.as_ref().is_some_and(|status| status.muted);
        ui.vertical(|ui| {
            self.ui_mic_stage(ui);
            ui.separator();
            ui.columns(2, |columns| {
                columns[0].vertical(|ui| {
                    ui.set_min_width(360.0);
                    section_label(ui, "MIC VOLUME");
                    if percent_slider(ui, &mut self.mic_volume, 280.0).changed() {
                        self.set_volume();
                        self.save_config_snapshot();
                    }

                    ui.add_space(18.0);
                    section_label(ui, "INPUT LEVEL");
                    let display_peak = self.input_peak.sqrt().clamp(0.0, 1.0);
                    ui.add(
                        egui::ProgressBar::new(display_peak)
                            .desired_width(260.0)
                            .text(format!("{:.1}%", display_peak * 100.0)),
                    );
                    ui.small("Speak while turning the bottom gain dial to set hardware gain.");

                    ui.add_space(18.0);
                    section_label(ui, "MIC MONITORING");
                    if percent_slider(ui, &mut self.mic_monitoring, 280.0).changed() {
                        self.set_mic_monitoring();
                        self.save_config_snapshot();
                    }

                    ui.add_space(18.0);
                    section_label(ui, "HEADPHONE VOLUME");
                    if percent_slider(ui, &mut self.headphone_volume, 280.0).changed() {
                        self.set_headphone_volume();
                        self.save_config_snapshot();
                    }

                    ui.add_space(18.0);
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
                        ui.add_space(12.0);
                        ui.colored_label(egui::Color32::from_rgb(255, 120, 120), error);
                    }
                });

                columns[1].vertical(|ui| {
                    ui.set_min_width(430.0);
                    section_label(ui, "POLAR PATTERN");
                    ui.horizontal(|ui| {
                        polar_button(ui, "Stereo", self.polar_pattern == PolarPattern::Stereo);
                        polar_button(ui, "Omni", self.polar_pattern == PolarPattern::Omni);
                        polar_button(ui, "Cardioid", self.polar_pattern == PolarPattern::Cardioid);
                        polar_button(
                            ui,
                            "Bidirectional",
                            self.polar_pattern == PolarPattern::Bidirectional,
                        );
                    });
                    ui.add_space(16.0);
                    ui.strong(self.polar_pattern.label());
                    ui.label(pattern_description(self.polar_pattern));
                });
            });
        });
    }

    fn ui_lights(&mut self, ui: &mut egui::Ui) {
        self.drain_hid_events();
        self.refresh_status_periodic();
        ui.vertical(|ui| {
            self.ui_mic_stage(ui);
            ui.separator();
            ui.columns(4, |columns| {
                columns[0].vertical(|ui| {
                    ui.set_min_width(180.0);
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
                });

                columns[1].vertical(|ui| {
                    ui.set_min_width(190.0);
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
                    ui.add_space(18.0);
                    section_label(ui, "OPACITY");
                    if percent_slider(ui, &mut self.lighting.opacity, 180.0).changed() {
                        self.save_config_snapshot();
                    }
                });

                columns[2].vertical(|ui| {
                    ui.set_min_width(260.0);
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
                    ui.add_space(10.0);
                    let mut color_changed = false;
                    if let Some(color) = self.lighting.colors.get_mut(self.lighting.selected_color)
                    {
                        color_changed = ui.color_edit_button_srgba(color).changed();
                    }
                    if color_changed {
                        self.save_config_snapshot();
                    }
                    ui.add_space(18.0);
                    section_label(ui, "BRIGHTNESS");
                    if percent_slider(ui, &mut self.lighting.brightness, 220.0).changed() {
                        self.save_config_snapshot();
                    }
                    if ui
                        .checkbox(&mut self.lighting.live_when_muted, "Lights show live state")
                        .changed()
                    {
                        self.save_config_snapshot();
                        if self.lighting.live_when_muted {
                            if let Some(is_live) = self.status.as_ref().map(|status| !status.muted)
                            {
                                self.apply_live_mute_lighting(is_live);
                            }
                        }
                    }
                });

                columns[3].vertical(|ui| {
                    ui.set_min_width(220.0);
                    section_label(ui, "SPEED");
                    if percent_slider(ui, &mut self.lighting.speed, 220.0).changed() {
                        self.save_config_snapshot();
                    }
                    ui.add_space(24.0);
                    if ui
                        .add_sized([180.0, 28.0], egui::Button::new("Apply to Microphone"))
                        .clicked()
                    {
                        self.apply_lighting_to_microphone();
                    }
                    if ui
                        .add_sized([180.0, 28.0], egui::Button::new("Save to Microphone"))
                        .on_hover_text("Experimental persistent device write")
                        .clicked()
                    {
                        self.save_lighting_to_microphone();
                    }
                    if ui
                        .add_sized([180.0, 28.0], egui::Button::new("Stop Lighting Stream"))
                        .clicked()
                    {
                        if let Some(cancel) = &self.lighting_cancel {
                            cancel.store(true, Ordering::Relaxed);
                        }
                        self.lighting_cancel = None;
                        self.lighting_message = "Lighting stream stopped.".to_string();
                        log_event("info", "lighting.apply.stop", &[]);
                    }
                    ui.add_space(12.0);
                    if let Some(device) = &self.lighting_device {
                        ui.small(format!("{} {}", device.manufacturer, device.product));
                    }
                    ui.label(&self.lighting_message);
                });
            });
        });
    }

    fn ui_mic_stage(&self, ui: &mut egui::Ui) {
        let available = ui.available_width();
        let height = (ui.available_height() * 0.38).clamp(190.0, 290.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(available, height), egui::Sense::hover());
        let painter = ui.painter_at(rect);

        painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(22, 23, 23));
        let center = rect.center();
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
    }
}

impl eframe::App for MicLiteApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.start_minimized && !self.start_minimized_applied {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            self.start_minimized_applied = true;
            log_event("info", "gui.start_minimized", &[]);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(50));
        ui.vertical(|ui| {
            ui.add_space(8.0);
            self.ui_top_bar(ui);
            ui.add_space(8.0);
            ui.separator();
            match self.tab {
                Tab::Audio => self.ui_audio(ui),
                Tab::Lights => self.ui_lights(ui),
            }
        });
    }
}

fn tab_button(ui: &mut egui::Ui, current: &mut Tab, tab: Tab, label: &str) {
    let selected = *current == tab;
    if ui.selectable_label(selected, label).clicked() {
        *current = tab;
    }
}

fn section_label(ui: &mut egui::Ui, label: &str) {
    ui.label(egui::RichText::new(label).color(egui::Color32::from_rgb(180, 184, 188)));
}

fn percent_slider(ui: &mut egui::Ui, value: &mut u8, width: f32) -> egui::Response {
    ui.horizontal(|ui| {
        let response = ui.add_sized(
            [width, 20.0],
            egui::Slider::new(value, 0..=100).show_value(false),
        );
        ui.add_sized([34.0, 20.0], egui::Label::new(format!("{}", *value)));
        response
    })
    .inner
}

fn target_button(ui: &mut egui::Ui, current: &mut LightTarget, target: LightTarget) -> bool {
    let response = ui.add_sized(
        [58.0, 30.0],
        egui::Button::new(target.label()).selected(*current == target),
    );
    if response.clicked() {
        *current = target;
        true
    } else {
        false
    }
}

fn polar_button(ui: &mut egui::Ui, label: &str, selected: bool) {
    let response = ui.add_sized([88.0, 44.0], egui::Button::new(label).selected(selected));
    if response.hovered() {
        response.on_hover_text(label);
    }
}

fn color_swatch(ui: &mut egui::Ui, color: egui::Color32, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 34.0), egui::Sense::click());
    ui.painter().rect_filled(rect, 0.0, color);
    if selected {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            0.0,
            egui::Stroke::new(2.0, egui::Color32::WHITE),
            egui::StrokeKind::Outside,
        );
    }
    response
}

fn draw_microphone(painter: &egui::Painter, center: egui::Pos2, stage_height: f32) {
    let body_width = stage_height * 0.18;
    let body_height = stage_height * 0.58;
    let top = center.y - body_height * 0.42;
    let left = center.x - body_width / 2.0;
    let body =
        egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(body_width, body_height));

    painter.rect_filled(body, 18.0, egui::Color32::from_rgb(18, 18, 18));
    painter.rect_stroke(
        body,
        18.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 50, 50)),
        egui::StrokeKind::Outside,
    );

    let grille = egui::Rect::from_min_max(
        body.left_top() + egui::vec2(8.0, 42.0),
        body.right_bottom() - egui::vec2(8.0, body_height * 0.34),
    );
    let dot_color = egui::Color32::from_rgb(86, 30, 54);
    let mut y = grille.top();
    while y < grille.bottom() {
        let mut x = grille.left();
        while x < grille.right() {
            painter.circle_filled(egui::pos2(x, y), 2.4, dot_color);
            x += 8.0;
        }
        y += 7.0;
    }

    let mount_y = body.bottom() - body_height * 0.22;
    painter.rect_filled(
        egui::Rect::from_center_size(
            egui::pos2(center.x, mount_y),
            egui::vec2(body_width * 1.35, 16.0),
        ),
        3.0,
        egui::Color32::from_rgb(9, 9, 9),
    );
    painter.rect_filled(
        egui::Rect::from_center_size(
            egui::pos2(center.x, body.bottom() + 36.0),
            egui::vec2(18.0, 80.0),
        ),
        3.0,
        egui::Color32::from_rgb(12, 12, 12),
    );
    painter.rect_filled(
        egui::Rect::from_center_size(
            egui::pos2(center.x, body.bottom() + 84.0),
            egui::vec2(body_width * 1.6, 14.0),
        ),
        7.0,
        egui::Color32::from_rgb(18, 18, 18),
    );
}

fn run_gui(args: &[String]) {
    let start_minimized = args
        .iter()
        .any(|arg| arg == "--start-minimized" || arg == "--minimized");
    if args
        .iter()
        .any(|arg| arg != "--start-minimized" && arg != "--minimized")
    {
        eprintln!("Usage: hyperx-mic-lite gui [--start-minimized]");
        process::exit(2);
    }
    log_event("info", "gui.start", &[]);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("HyperX Mic Lite")
            .with_inner_size([980.0, 640.0])
            .with_min_inner_size([820.0, 540.0]),
        ..Default::default()
    };

    let result = eframe::run_native(
        "HyperX Mic Lite",
        options,
        Box::new(|context| {
            context.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(MicLiteApp::new(start_minimized)))
        }),
    );

    if let Err(error) = result {
        log_event("error", "gui.error", &[("message", error.to_string())]);
        eprintln!("{error}");
        process::exit(1);
    }
    log_event("info", "gui.exit", &[]);
}

fn set_volume(args: &[String]) -> WinResult<()> {
    if args.len() != 1 {
        eprintln!("Usage: hyperx-mic-lite volume 75");
        process::exit(2);
    }

    let percent = args[0].parse::<u8>().unwrap_or_else(|_| {
        eprintln!("Volume must be a number from 0 to 100.");
        process::exit(2);
    });

    if percent > 100 {
        eprintln!("Volume must be a number from 0 to 100.");
        process::exit(2);
    }

    set_mic_volume_percent(percent)?;
    print_status_json(&mic_status()?);
    Ok(())
}

fn run_audio_command(args: &[String]) -> WinResult<()> {
    if args.is_empty() {
        audio_usage();
        process::exit(2);
    }

    match args[0].as_str() {
        "volume" => {
            if args.len() != 3 {
                audio_usage();
                process::exit(2);
            }
            let control = AudioClassControl::parse(&args[1]).unwrap_or_else(|| {
                eprintln!("Unknown audio control '{}'.", args[1]);
                audio_usage();
                process::exit(2);
            });
            let percent = parse_percent_arg(&args[2]);
            set_audio_control_volume(control, percent)?;
            println!(
                "{{\"control\":{},\"volume\":{}}}",
                json_string(control.label()),
                percent
            );
            Ok(())
        }
        "mute" => {
            if args.len() != 3 {
                audio_usage();
                process::exit(2);
            }
            let control = AudioClassControl::parse(&args[1]).unwrap_or_else(|| {
                eprintln!("Unknown audio control '{}'.", args[1]);
                audio_usage();
                process::exit(2);
            });
            let muted = parse_on_off_arg(&args[2]);
            set_audio_control_mute(control, muted)?;
            println!(
                "{{\"control\":{},\"muted\":{}}}",
                json_string(control.label()),
                muted
            );
            Ok(())
        }
        "topology" => {
            if args.len() != 2 {
                audio_usage();
                process::exit(2);
            }
            let flow = match args[1].as_str() {
                "capture" | "mic" | "input" => eCapture,
                "render" | "headphone" | "output" => eRender,
                _ => {
                    audio_usage();
                    process::exit(2);
                }
            };
            print_audio_topology(flow)?;
            Ok(())
        }
        _ => {
            audio_usage();
            process::exit(2);
        }
    }
}

fn audio_usage() {
    eprintln!(
        "Usage:\n\
  hyperx-mic-lite audio volume <mic|monitoring|headphone> <0-100>\n\
  hyperx-mic-lite audio mute <mic|monitoring|headphone> <on|off>\n\
  hyperx-mic-lite audio topology <capture|render>"
    );
}

fn parse_percent_arg(value: &str) -> u8 {
    let percent = value.parse::<u8>().unwrap_or_else(|_| {
        eprintln!("Percent must be a number from 0 to 100.");
        process::exit(2);
    });
    if percent > 100 {
        eprintln!("Percent must be a number from 0 to 100.");
        process::exit(2);
    }
    percent
}

fn parse_on_off_arg(value: &str) -> bool {
    match value.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "muted" => true,
        "off" | "unmuted" | "live" | "false" | "0" => false,
        _ => {
            eprintln!("Mute value must be on/off or true/false.");
            process::exit(2);
        }
    }
}

#[derive(Clone, Copy)]
enum AudioClassControl {
    Mic,
    Monitoring,
    Headphone,
}

impl AudioClassControl {
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "mic" | "microphone" | "input" => Some(Self::Mic),
            "monitoring" | "monitor" | "sidetone" => Some(Self::Monitoring),
            "headphone" | "headphones" | "output" => Some(Self::Headphone),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Mic => "mic",
            Self::Monitoring => "monitoring",
            Self::Headphone => "headphone",
        }
    }

    fn volume_part_id(self) -> u32 {
        match self {
            Self::Mic => 0x20008,
            Self::Monitoring => 0x2000a,
            Self::Headphone => 0x20006,
        }
    }

    fn mute_part_id(self) -> u32 {
        match self {
            Self::Mic => 0x20007,
            Self::Monitoring => 0x20009,
            Self::Headphone => 0x20005,
        }
    }

    fn db_range(self) -> (f32, f32) {
        match self {
            Self::Mic => (-8.0, 7.0),
            Self::Monitoring => (-30.0, 6.0),
            Self::Headphone => (-40.0, -9.0),
        }
    }

    fn endpoint_flow(self) -> windows::Win32::Media::Audio::EDataFlow {
        match self {
            Self::Headphone => eRender,
            Self::Mic | Self::Monitoring => eCapture,
        }
    }
}

fn set_mic_mute(muted: bool) -> WinResult<()> {
    let result =
        unsafe { endpoint_volume(&default_capture_device()?)?.SetMute(muted, std::ptr::null()) };
    if result.is_ok() {
        log_event("info", "audio.mute.set", &[("muted", muted.to_string())]);
    }
    result
}

fn set_mic_volume_percent(percent: u8) -> WinResult<()> {
    let result = unsafe {
        endpoint_volume(&default_capture_device()?)?
            .SetMasterVolumeLevelScalar(percent as f32 / 100.0, std::ptr::null())
    };
    if result.is_ok() {
        if let Err(error) = set_topology_control_volume(AudioClassControl::Mic, percent) {
            log_event(
                "warn",
                "audio.usb_class.volume.mic.error",
                &[("message", error.to_string())],
            );
        }
    }
    if result.is_ok() {
        log_event(
            "info",
            "audio.volume.set",
            &[("percent", percent.to_string())],
        );
    }
    result
}

fn set_audio_control_volume(control: AudioClassControl, percent: u8) -> WinResult<()> {
    match control {
        AudioClassControl::Mic => set_mic_volume_percent(percent),
        AudioClassControl::Monitoring => set_topology_control_volume(control, percent),
        AudioClassControl::Headphone => {
            if let Ok(device) = hyperx_render_device() {
                unsafe {
                    endpoint_volume(&device)?
                        .SetMasterVolumeLevelScalar(percent as f32 / 100.0, std::ptr::null())?;
                }
            }
            set_topology_control_volume(control, percent)
        }
    }?;
    log_event(
        "info",
        "audio.usb_class.volume.set",
        &[
            ("control", control.label().to_string()),
            ("percent", percent.to_string()),
        ],
    );
    Ok(())
}

fn set_audio_control_mute(control: AudioClassControl, muted: bool) -> WinResult<()> {
    match control {
        AudioClassControl::Mic => set_mic_mute(muted),
        AudioClassControl::Monitoring | AudioClassControl::Headphone => {
            set_topology_control_mute(control, muted)
        }
    }?;
    log_event(
        "info",
        "audio.usb_class.mute.set",
        &[
            ("control", control.label().to_string()),
            ("muted", muted.to_string()),
        ],
    );
    Ok(())
}

fn set_topology_control_volume(control: AudioClassControl, percent: u8) -> WinResult<()> {
    let device = hyperx_audio_device(control.endpoint_flow())?;
    let topology: IDeviceTopology = unsafe { device.Activate(CLSCTX_ALL, None)? };
    let part = find_topology_part(&topology, control.volume_part_id())?
        .or_else(|| unsafe { topology.GetPartById(control.volume_part_id()).ok() })
        .ok_or_else(|| {
            Error::new(
                HRESULT(0x80070490u32 as i32),
                "Topology volume part not found",
            )
        })?;
    let volume = activate_part_interface::<IAudioVolumeLevel>(&part)?;
    let (captured_min, captured_max) = control.db_range();
    let mut target = captured_min + (captured_max - captured_min) * percent as f32 / 100.0;
    unsafe {
        let channels = volume.GetChannelCount().unwrap_or(2).max(1);
        let mut min = 0.0f32;
        let mut max = 0.0f32;
        let mut stepping = 0.0f32;
        if volume
            .GetLevelRange(0, &mut min, &mut max, &mut stepping)
            .is_ok()
        {
            target = target.clamp(min, max);
        }
        for channel in 0..channels {
            volume.SetLevel(channel, target, None)?;
        }
    }
    Ok(())
}

fn set_topology_control_mute(control: AudioClassControl, muted: bool) -> WinResult<()> {
    let device = hyperx_audio_device(control.endpoint_flow())?;
    let topology: IDeviceTopology = unsafe { device.Activate(CLSCTX_ALL, None)? };
    let part = find_topology_part(&topology, control.mute_part_id())?
        .or_else(|| unsafe { topology.GetPartById(control.mute_part_id()).ok() })
        .ok_or_else(|| {
            Error::new(
                HRESULT(0x80070490u32 as i32),
                "Topology mute part not found",
            )
        })?;
    let mute = activate_part_interface::<IAudioMute>(&part)?;
    unsafe { mute.SetMute(muted, None) }
}

fn activate_part_interface<T: Interface>(
    part: &windows::Win32::Media::Audio::IPart,
) -> WinResult<T> {
    let mut raw = std::ptr::null_mut();
    unsafe {
        part.Activate(CLSCTX_ALL.0 as u32, &T::IID, Some(&mut raw))?;
        Type::from_abi(raw)
    }
}

fn hyperx_render_device() -> WinResult<IMMDevice> {
    hyperx_audio_device(eRender)
}

fn hyperx_audio_device(flow: windows::Win32::Media::Audio::EDataFlow) -> WinResult<IMMDevice> {
    let enumerator = device_enumerator()?;
    let collection =
        unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE(DEVICE_STATEMASK_ALL))? };
    let count = unsafe { collection.GetCount()? };
    for index in 0..count {
        let device = unsafe { collection.Item(index)? };
        let name = device_name(&device)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.contains("hyperx") || name.contains("quadcast") {
            return Ok(device);
        }
    }
    unsafe { enumerator.GetDefaultAudioEndpoint(flow, eCommunications) }
}

fn print_audio_topology(flow: windows::Win32::Media::Audio::EDataFlow) -> WinResult<()> {
    let device = hyperx_audio_device(flow)?;
    let topology: IDeviceTopology = unsafe { device.Activate(CLSCTX_ALL, None)? };
    let device_name = device_name(&device).unwrap_or_else(|_| "Unknown".to_string());
    println!("Topology for {device_name}");
    let mut visited = Vec::new();
    unsafe {
        let subunit_count = topology.GetSubunitCount()?;
        for index in 0..subunit_count {
            let subunit = topology.GetSubunit(index)?;
            if let Ok(part) = subunit.cast() {
                print_topology_part(&part, 0, &mut visited)?;
            }
        }
        let connector_count = topology.GetConnectorCount()?;
        for index in 0..connector_count {
            let connector = topology.GetConnector(index)?;
            if let Ok(connected) = connector.GetConnectedTo() {
                if let Ok(part) = connected.cast() {
                    print_topology_part(&part, 0, &mut visited)?;
                }
            }
        }
    }
    Ok(())
}

fn find_topology_part(
    topology: &IDeviceTopology,
    id: u32,
) -> WinResult<Option<windows::Win32::Media::Audio::IPart>> {
    let mut visited = Vec::new();
    unsafe {
        let subunit_count = topology.GetSubunitCount()?;
        for index in 0..subunit_count {
            let subunit = topology.GetSubunit(index)?;
            if let Ok(part) = subunit.cast() {
                if let Some(found) = find_topology_part_from(&part, id, &mut visited)? {
                    return Ok(Some(found));
                }
            }
        }
        let connector_count = topology.GetConnectorCount()?;
        for index in 0..connector_count {
            let connector = topology.GetConnector(index)?;
            if let Ok(connected) = connector.GetConnectedTo() {
                if let Ok(part) = connected.cast() {
                    if let Some(found) = find_topology_part_from(&part, id, &mut visited)? {
                        return Ok(Some(found));
                    }
                }
            }
        }
    }
    Ok(None)
}

fn find_topology_part_from(
    part: &windows::Win32::Media::Audio::IPart,
    id: u32,
    visited: &mut Vec<u32>,
) -> WinResult<Option<windows::Win32::Media::Audio::IPart>> {
    unsafe {
        let local_id = part.GetLocalId()?;
        if local_id == id {
            return Ok(Some(part.clone()));
        }
        if visited.contains(&local_id) {
            return Ok(None);
        }
        visited.push(local_id);
        if let Ok(parts) = part.EnumPartsIncoming() {
            let count = parts.GetCount().unwrap_or(0);
            for index in 0..count {
                if let Ok(child) = parts.GetPart(index) {
                    if let Some(found) = find_topology_part_from(&child, id, visited)? {
                        return Ok(Some(found));
                    }
                }
            }
        }
        if let Ok(parts) = part.EnumPartsOutgoing() {
            let count = parts.GetCount().unwrap_or(0);
            for index in 0..count {
                if let Ok(child) = parts.GetPart(index) {
                    if let Some(found) = find_topology_part_from(&child, id, visited)? {
                        return Ok(Some(found));
                    }
                }
            }
        }
    }
    Ok(None)
}

fn print_topology_part(
    part: &windows::Win32::Media::Audio::IPart,
    depth: usize,
    visited: &mut Vec<u32>,
) -> WinResult<()> {
    unsafe {
        let id = part.GetLocalId()?;
        if visited.contains(&id) {
            return Ok(());
        }
        visited.push(id);
        let indent = "  ".repeat(depth);
        let name = part
            .GetName()
            .ok()
            .and_then(|value| value.to_string().ok())
            .unwrap_or_default();
        let subtype = part.GetSubType().ok();
        println!("{indent}part id=0x{id:02x} name={name} subtype={subtype:?}");
        let control_count = part.GetControlInterfaceCount().unwrap_or(0);
        for index in 0..control_count {
            if let Ok(control) = part.GetControlInterface(index) {
                let control_name = control
                    .GetName()
                    .ok()
                    .and_then(|value| value.to_string().ok())
                    .unwrap_or_default();
                let iid = control.GetIID().ok();
                println!("{indent}  control name={control_name} iid={iid:?}");
            }
        }
        if let Ok(parts) = part.EnumPartsIncoming() {
            let count = parts.GetCount().unwrap_or(0);
            for index in 0..count {
                if let Ok(child) = parts.GetPart(index) {
                    print_topology_part(&child, depth + 1, visited)?;
                }
            }
        }
        if let Ok(parts) = part.EnumPartsOutgoing() {
            let count = parts.GetCount().unwrap_or(0);
            for index in 0..count {
                if let Ok(child) = parts.GetPart(index) {
                    print_topology_part(&child, depth + 1, visited)?;
                }
            }
        }
    }
    Ok(())
}

fn input_peak_value() -> WinResult<f32> {
    let device = default_capture_device()?;
    let meter = endpoint_meter(&device)?;
    unsafe { meter.GetPeakValue() }
}

fn start_audio_peak_monitor() -> Result<AudioPeakMonitor, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "No default input device is available.".to_string())?;
    let config = device
        .default_input_config()
        .map_err(|error| error.to_string())?;
    let channels = config.channels() as usize;
    let stream_config: cpal::StreamConfig = config.clone().into();
    let peak_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            build_peak_stream::<f32>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::F64 => {
            build_peak_stream::<f64>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::I8 => {
            build_peak_stream::<i8>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::I16 => {
            build_peak_stream::<i16>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::I32 => {
            build_peak_stream::<i32>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::I64 => {
            build_peak_stream::<i64>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::U8 => {
            build_peak_stream::<u8>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::U16 => {
            build_peak_stream::<u16>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::U32 => {
            build_peak_stream::<u32>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::U64 => {
            build_peak_stream::<u64>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        other => Err(format!("Unsupported input sample format: {other:?}")),
    }?;
    stream.play().map_err(|error| error.to_string())?;
    Ok(AudioPeakMonitor {
        peak_bits,
        _stream: stream,
    })
}

fn build_peak_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    peak_bits: Arc<AtomicU32>,
) -> Result<cpal::Stream, String>
where
    T: cpal::Sample + cpal::SizedSample + ToPeakSample + Send + 'static,
{
    device
        .build_input_stream(
            config,
            move |data: &[T], _| update_peak_from_samples(data, channels, &peak_bits),
            |error| {
                log_event(
                    "warn",
                    "audio.capture.stream.error",
                    &[("message", error.to_string())],
                );
            },
            None,
        )
        .map_err(|error| error.to_string())
}

fn update_peak_from_samples<T>(data: &[T], channels: usize, peak_bits: &AtomicU32)
where
    T: ToPeakSample,
{
    let step = channels.max(1);
    let peak = data
        .chunks(step)
        .flat_map(|frame| frame.iter())
        .map(|sample| sample.to_peak_sample().abs())
        .fold(0.0f32, f32::max)
        .clamp(0.0, 1.0);
    peak_bits.store(peak.to_bits(), Ordering::Relaxed);
}

trait ToPeakSample {
    fn to_peak_sample(&self) -> f32;
}

impl ToPeakSample for f32 {
    fn to_peak_sample(&self) -> f32 {
        *self
    }
}

impl ToPeakSample for f64 {
    fn to_peak_sample(&self) -> f32 {
        *self as f32
    }
}

impl ToPeakSample for i8 {
    fn to_peak_sample(&self) -> f32 {
        *self as f32 / i8::MAX as f32
    }
}

impl ToPeakSample for i16 {
    fn to_peak_sample(&self) -> f32 {
        *self as f32 / i16::MAX as f32
    }
}

impl ToPeakSample for i32 {
    fn to_peak_sample(&self) -> f32 {
        *self as f32 / i32::MAX as f32
    }
}

impl ToPeakSample for i64 {
    fn to_peak_sample(&self) -> f32 {
        *self as f32 / i64::MAX as f32
    }
}

impl ToPeakSample for u8 {
    fn to_peak_sample(&self) -> f32 {
        (*self as f32 - 128.0) / 128.0
    }
}

impl ToPeakSample for u16 {
    fn to_peak_sample(&self) -> f32 {
        (*self as f32 - 32768.0) / 32768.0
    }
}

impl ToPeakSample for u32 {
    fn to_peak_sample(&self) -> f32 {
        (*self as f32 - 2147483648.0) / 2147483648.0
    }
}

impl ToPeakSample for u64 {
    fn to_peak_sample(&self) -> f32 {
        (*self as f64 - 9223372036854775808.0) as f32 / 9223372036854775808.0_f32
    }
}

fn mic_status() -> WinResult<MicStatus> {
    let device = default_capture_device()?;
    let mut info = describe_device(&device)?;
    info.is_default = true;

    let volume = endpoint_volume(&device)?;
    let scalar = unsafe { volume.GetMasterVolumeLevelScalar()? };
    let muted = unsafe { volume.GetMute()?.as_bool() };

    Ok(MicStatus {
        device: info,
        volume: (scalar * 100.0).round().clamp(0.0, 100.0) as u8,
        muted,
    })
}

fn list_capture_devices() -> WinResult<Vec<DeviceInfo>> {
    let enumerator = device_enumerator()?;
    let default_id = default_capture_device_with(&enumerator)
        .and_then(|device| unsafe { device.GetId() })
        .map(|id| unsafe { id.to_string().unwrap_or_default() })
        .unwrap_or_default();

    let collection =
        unsafe { enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE(DEVICE_STATEMASK_ALL))? };

    let count = unsafe { collection.GetCount()? };
    let mut devices = Vec::with_capacity(count as usize);

    for index in 0..count {
        let device = unsafe { collection.Item(index)? };
        let mut info = describe_device(&device)?;
        info.is_default = info.id == default_id;
        devices.push(info);
    }

    Ok(devices)
}

fn default_capture_device() -> WinResult<IMMDevice> {
    default_capture_device_with(&device_enumerator()?)
}

fn default_capture_device_with(enumerator: &IMMDeviceEnumerator) -> WinResult<IMMDevice> {
    unsafe { enumerator.GetDefaultAudioEndpoint(eCapture, eCommunications) }
}

fn device_enumerator() -> WinResult<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

fn endpoint_volume(device: &IMMDevice) -> WinResult<IAudioEndpointVolume> {
    unsafe { device.Activate(CLSCTX_ALL, None) }
}

fn endpoint_meter(device: &IMMDevice) -> WinResult<IAudioMeterInformation> {
    unsafe { device.Activate(CLSCTX_ALL, None) }
}

fn describe_device(device: &IMMDevice) -> WinResult<DeviceInfo> {
    let id = unsafe { device.GetId()?.to_string().unwrap_or_default() };
    let state = unsafe { device.GetState()? };

    Ok(DeviceInfo {
        id,
        name: device_name(device)?,
        state: state_name(state.0),
        is_default: false,
    })
}

fn device_name(device: &IMMDevice) -> WinResult<String> {
    let store = unsafe { device.OpenPropertyStore(STGM_READ)? };
    let mut value = unsafe { store.GetValue(&PKEY_Device_FriendlyName)? };
    let name = unsafe {
        value
            .Anonymous
            .Anonymous
            .Anonymous
            .pwszVal
            .to_string()
            .unwrap_or_default()
    };
    unsafe { PropVariantClear(&mut value)? };

    if name.trim().is_empty() {
        Ok("Unknown microphone".to_string())
    } else {
        Ok(name)
    }
}

fn state_name(state: u32) -> String {
    match state {
        value if value == DEVICE_STATE_ACTIVE.0 => "active",
        value if value == DEVICE_STATE_DISABLED.0 => "disabled",
        value if value == DEVICE_STATE_NOTPRESENT.0 => "not_present",
        value if value == DEVICE_STATE_UNPLUGGED.0 => "unplugged",
        other => return format!("unknown_{other}"),
    }
    .to_string()
}

fn print_devices_json(devices: &[DeviceInfo]) {
    println!("[");
    for (index, device) in devices.iter().enumerate() {
        let comma = if index + 1 == devices.len() { "" } else { "," };
        println!(
            "  {{\n    \"id\": \"{}\",\n    \"name\": \"{}\",\n    \"state\": \"{}\",\n    \"isDefault\": {}\n  }}{}",
            json_escape(&device.id),
            json_escape(&device.name),
            json_escape(&device.state),
            device.is_default,
            comma
        );
    }
    println!("]");
}

fn print_status_json(status: &MicStatus) {
    println!(
        "{{\n  \"device\": {{\n    \"id\": \"{}\",\n    \"name\": \"{}\",\n    \"state\": \"{}\",\n    \"isDefault\": {}\n  }},\n  \"volume\": {},\n  \"muted\": {}\n}}",
        json_escape(&status.device.id),
        json_escape(&status.device.name),
        json_escape(&status.device.state),
        status.device.is_default,
        status.volume,
        status.muted
    );
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if c.is_control() => escaped.push_str(&format!("\\u{:04x}", c as u32)),
            c => escaped.push(c),
        }
    }
    escaped
}
