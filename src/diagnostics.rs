use std::{
    env, fs,
    path::{Path, PathBuf},
    process,
};

use crate::{
    audio::{list_capture_devices, mic_status},
    constants::APP_NAME,
    lighting::{
        detect_lighting_device, hid_caps_for_path, is_supported_lighting_device,
        lighting_device_score,
    },
    logging::{log_event, log_timestamp},
    paths::{app_data_dir, config_path, log_file_path},
    service::read_service_health,
    time::unix_timestamp_seconds,
};
pub(crate) fn run_diagnostics_command(args: &[String]) {
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

pub(crate) fn default_diagnostics_dir() -> PathBuf {
    app_data_dir()
        .join("diagnostics")
        .join(format!("diagnostics-{}", unix_timestamp_seconds()))
}

pub(crate) fn export_diagnostics_bundle(destination: &Path) -> Result<(), String> {
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
