use crate::{
    audio::{
        AudioClassControl, AudioPeakMonitor, input_peak_value, list_capture_devices, mic_status,
        peak_from_bits, print_devices_json, print_status_json, run_audio_command,
        set_audio_control_volume, set_mic_mute, set_mic_volume_percent, set_volume,
        start_audio_peak_monitor, toggle_mic_mute,
    },
    config::{
        AppConfig, AudioConfig, LightingConfig, UiConfig, load_or_create_config, save_config,
    },
    config_cli::run_config_command,
    constants::*,
    eventlog::{register_event_log_source, run_eventlog_command},
    logging::{install_panic_hook, log_event, log_timestamp},
    logs::run_logs_command,
    model::{
        Effect, HidEvent, LightTarget, LightingDevice, MicStatus, PolarPattern, ServiceHealth, Tab,
    },
    paths::{app_data_dir, config_path, log_file_path, service_health_path},
    startup::run_startup_command,
    time::unix_timestamp_seconds,
    tray::TrayHandle,
};
use eframe::egui;
use std::{
    env,
    ffi::{CStr, OsString},
    fs,
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

use windows::{
    Win32::{
        Devices::HumanInterfaceDevice::{
            HIDP_CAPS, HIDP_STATUS_SUCCESS, HidD_FreePreparsedData, HidD_GetPreparsedData,
            HidP_GetCaps, PHIDP_PREPARSED_DATA,
        },
        Foundation::CloseHandle,
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        },
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
    },
    core::PCWSTR,
    core::Result as WinResult,
};

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
    shared_peak_bits: Option<Arc<AtomicU32>>,
}

const LIGHTING_CELL_COUNT: usize = 16;
type LightingFrame = [[u8; 3]; LIGHTING_CELL_COUNT];

#[derive(Clone, Copy)]
enum StreamDuration {
    Timed(Duration),
    Forever,
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
    lighting_autostart_applied: bool,
    minimize_to_tray: bool,
    hidden_to_tray: bool,
    force_exit: bool,
    tray_handle: Option<TrayHandle>,
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

pub fn run_app() {
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
            toggle_mic_mute()?;
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
        shared_peak_bits: None,
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
        shared_peak_bits: None,
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
    let frame = build_vu_frame(level as f32 / 100.0, config.lighting.brightness, 0);
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
    let mut vu_tick = 0u32;
    let mut meter_error_logged = false;
    let capture_monitor = if program.effect == Effect::VuMeter && program.shared_peak_bits.is_none()
    {
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
        if program.effect == Effect::VuMeter {
            log_event("info", "lighting.vu.capture.shared", &[]);
        }
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
            let direct_peak = if let Some(peak_bits) = &program.shared_peak_bits {
                peak_from_bits(peak_bits)
            } else if let Some(monitor) = &capture_monitor {
                monitor.peak()
            } else {
                0.0
            };
            let endpoint_peak = if program.shared_peak_bits.is_none() {
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
            } else {
                0.0
            };
            let target = vu_target_level(direct_peak.max(endpoint_peak));
            vu_level = smooth_vu_level(vu_level, target);
            vu_tick = vu_tick.wrapping_add(1);
            if vu_tick % 50 == 0 {
                log_event(
                    "info",
                    "lighting.vu.level",
                    &[
                        ("direct", format!("{direct_peak:.4}")),
                        ("endpoint", format!("{endpoint_peak:.4}")),
                        ("target", format!("{target:.3}")),
                        ("level", format!("{vu_level:.3}")),
                    ],
                );
            }
            build_vu_frame(vu_level, program.brightness, vu_tick)
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
    let coefficient = if target > current { 0.42 } else { 0.14 };
    current + (target - current) * coefficient
}

fn vu_target_level(raw_peak: f32) -> f32 {
    let normalized = ((raw_peak - 0.00002).max(0.0) * 1200.0).clamp(0.0, 1.0);
    normalized.powf(0.48)
}

fn build_vu_frame(level: f32, brightness: u8, tick: u32) -> LightingFrame {
    let level = level.clamp(0.0, 1.0);
    let visible_level = level.powf(0.55).max(0.04);
    let lit_cells = ((visible_level * LIGHTING_CELL_COUNT as f32).ceil() as usize)
        .clamp(1, LIGHTING_CELL_COUNT);
    let mut frame = solid_frame(scale_color([10, 0, 0], brightness.max(70)));

    for cell in 0..LIGHTING_CELL_COUNT {
        let active = cell < lit_cells;
        let physical_cell = LIGHTING_CELL_COUNT - 1 - cell;
        if active {
            let position = if lit_cells <= 1 {
                0.0
            } else {
                cell as f32 / (lit_cells - 1) as f32
            };
            let shimmer = ((flame_wave(cell, tick) + 1.0) * 0.5).clamp(0.0, 1.0);
            frame[physical_cell] = vu_flame_color(position, shimmer, brightness.max(95));
        } else {
            let distance = cell.saturating_sub(lit_cells) as f32;
            if distance < 2.0 {
                let afterglow = (2.0 - distance) / 2.0;
                frame[physical_cell] = scale_color([80, 4, 0], (afterglow * 35.0) as u8);
            }
        }
    }
    frame
}

fn flame_wave(cell: usize, tick: u32) -> f32 {
    let phase_a = tick as f32 * 0.18 + cell as f32 * 0.72;
    let phase_b = tick as f32 * 0.11 + cell as f32 * 1.37;
    (phase_a.sin() * 0.65 + phase_b.sin() * 0.35).clamp(-1.0, 1.0)
}

fn vu_flame_color(position: f32, shimmer: f32, brightness: u8) -> [u8; 3] {
    let base = if position < 0.45 {
        lerp_color_float([255, 245, 120], [255, 170, 0], position / 0.45)
    } else if position < 0.78 {
        lerp_color_float([255, 170, 0], [255, 38, 0], (position - 0.45) / 0.33)
    } else {
        lerp_color_float([255, 38, 0], [160, 0, 0], (position - 0.78) / 0.22)
    };
    let effective = ((0.78 + shimmer * 0.22) * brightness as f32).round() as u8;
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
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
            let pattern_width = 220.0;
            let gap = 12.0;
            let available = ui.available_width();
            let stage_width = (available - pattern_width - gap).clamp(360.0, 720.0);
            ui.allocate_ui(egui::vec2(stage_width, 250.0), |ui| {
                self.ui_mic_stage(ui);
            });
            ui.add_space(gap);
            ui.allocate_ui(egui::vec2(pattern_width, 250.0), |ui| {
                self.ui_pattern_panel(ui);
            });
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

    fn ui_mic_stage(&self, ui: &mut egui::Ui) {
        let available = ui.available_width();
        let height = ui.available_height().clamp(190.0, 290.0);
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

fn pattern_tile(ui: &mut egui::Ui, pattern: PolarPattern, current: PolarPattern) {
    let selected = current == pattern;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(74.0, 64.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let fill = if selected {
        egui::Color32::from_rgb(52, 70, 80)
    } else {
        egui::Color32::from_rgb(38, 39, 40)
    };
    painter.rect_filled(rect, 4.0, fill);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(
            if selected { 2.0 } else { 1.0 },
            if selected {
                egui::Color32::from_rgb(0, 162, 255)
            } else {
                egui::Color32::from_rgb(70, 72, 74)
            },
        ),
        egui::StrokeKind::Outside,
    );
    draw_pattern_icon(&painter, rect, pattern, selected);
    painter.text(
        rect.center_bottom() - egui::vec2(0.0, 8.0),
        egui::Align2::CENTER_BOTTOM,
        pattern.label(),
        egui::FontId::proportional(11.0),
        egui::Color32::from_rgb(220, 224, 228),
    );
    if response.hovered() {
        response.on_hover_text(pattern_description(pattern));
    }
}

fn draw_pattern_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    pattern: PolarPattern,
    selected: bool,
) {
    let center = rect.center_top() + egui::vec2(0.0, 24.0);
    let active = if selected {
        egui::Color32::from_rgb(235, 242, 246)
    } else {
        egui::Color32::from_rgb(155, 160, 164)
    };
    let muted = egui::Color32::from_rgb(72, 75, 78);
    let stroke = egui::Stroke::new(2.0, active);
    match pattern {
        PolarPattern::Stereo => {
            painter.circle_stroke(center - egui::vec2(9.0, 0.0), 10.0, stroke);
            painter.circle_stroke(center + egui::vec2(9.0, 0.0), 10.0, stroke);
        }
        PolarPattern::Omni => {
            painter.circle_stroke(center, 14.0, stroke);
        }
        PolarPattern::Cardioid => {
            painter.circle_stroke(center + egui::vec2(0.0, 2.0), 12.0, stroke);
            painter.circle_filled(center + egui::vec2(0.0, 10.0), 6.0, muted);
        }
        PolarPattern::Bidirectional => {
            painter.circle_stroke(center - egui::vec2(0.0, 8.0), 7.0, stroke);
            painter.circle_stroke(center + egui::vec2(0.0, 8.0), 7.0, stroke);
            painter.circle_filled(center, 4.0, muted);
        }
        PolarPattern::Unknown(_) => {
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                "?",
                egui::FontId::proportional(24.0),
                active,
            );
        }
    }
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
