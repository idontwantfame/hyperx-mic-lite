use std::{env, process};

use winreg::{
    RegKey,
    enums::{HKEY_LOCAL_MACHINE, KEY_READ},
};

use crate::{
    constants::{APP_NAME, EVENTLOG_SOURCE_PATH, EVENTLOG_TYPES_SUPPORTED},
    logging::{json_string, log_event},
};

pub(crate) fn run_eventlog_command(args: &[String]) {
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

pub(crate) fn register_event_log_source() -> Result<String, String> {
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
