use std::{env, path::PathBuf};

use crate::constants::APP_NAME;

pub(crate) fn app_data_dir() -> PathBuf {
    env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(APP_NAME)
}

pub(crate) fn config_dir() -> PathBuf {
    app_data_dir()
}

pub(crate) fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub(crate) fn log_file_path() -> PathBuf {
    app_data_dir().join("logs").join("app.log")
}

pub(crate) fn service_health_path() -> PathBuf {
    app_data_dir().join("service-health.json")
}
