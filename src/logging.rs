use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    process,
};

use windows::{
    Win32::System::EventLog::{
        DeregisterEventSource, EVENTLOG_ERROR_TYPE, EVENTLOG_INFORMATION_TYPE,
        EVENTLOG_WARNING_TYPE, RegisterEventSourceW, ReportEventW,
    },
    core::{PCWSTR, w},
};

use crate::{
    constants::EVENTLOG_MESSAGE_ID,
    paths::{app_data_dir, config_path, log_file_path},
    time::unix_timestamp_seconds,
};

pub(crate) fn install_panic_hook() {
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

pub(crate) fn log_event(level: &str, event: &str, fields: &[(&str, String)]) {
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

    // SAFETY: the source name is a static null-terminated wide literal; strings points at
    // wide_message, a null-terminated UTF-16 buffer that outlives ReportEventW, and the
    // handle returned by RegisterEventSourceW is deregistered exactly once before leaving.
    unsafe {
        if let Ok(handle) = RegisterEventSourceW(None, w!("HyperXMicLite")) {
            let _ = ReportEventW(
                handle,
                event_type,
                0,
                EVENTLOG_MESSAGE_ID,
                None,
                0,
                Some(&strings),
                None,
            );
            let _ = DeregisterEventSource(handle);
        }
    }
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
        "version": env!("HYPERX_BUILD_VERSION"),
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

pub(crate) fn log_timestamp() -> String {
    let seconds = unix_timestamp_seconds();
    format!("{seconds}")
}

pub(crate) fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}
