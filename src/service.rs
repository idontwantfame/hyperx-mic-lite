use std::{env, ffi::OsString, fs, process, sync::mpsc, thread, time::Duration};

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

use crate::{
    audio::{set_mic_mute, set_mic_volume_percent},
    com::ComApartment,
    config::{AppConfig, load_or_create_config, save_config},
    constants::{SERVICE_DESCRIPTION, SERVICE_DISPLAY_NAME, SERVICE_NAME},
    eventlog::register_event_log_source,
    logging::{log_event, log_timestamp},
    model::ServiceHealth,
    paths::service_health_path,
};

define_windows_service!(ffi_service_main, service_main);

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

pub(crate) fn run_service_command(args: &[String]) {
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

pub(crate) fn service_main(_arguments: Vec<OsString>) {
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

pub(crate) fn run_windows_service() -> Result<(), windows_service::Error> {
    log_event("info", "service.dispatcher.start", &[]);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

pub(crate) fn run_service_worker_console() -> Result<(), String> {
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

pub(crate) fn read_service_health() -> Result<ServiceHealth, String> {
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

pub(crate) fn windows_service_error(error: windows_service::Error) -> String {
    match error {
        windows_service::Error::Winapi(io_error) => match io_error.raw_os_error() {
            Some(5) => "Access denied. Run this command from an elevated terminal.".to_string(),
            Some(1060) => format!("{SERVICE_DISPLAY_NAME} is not installed."),
            _ => format!("{io_error}"),
        },
        other => format!("{other}"),
    }
}
