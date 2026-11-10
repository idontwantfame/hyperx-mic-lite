use std::{env, path::PathBuf};

use crate::constants::APP_NAME;

/// Per-user application data: `%APPDATA%\HyperXMicLite` on Windows and
/// `~/Library/Application Support/HyperXMicLite` on macOS.
pub(crate) fn app_data_dir() -> PathBuf {
    data_root()
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(APP_NAME)
}

#[cfg(windows)]
fn data_root() -> Option<PathBuf> {
    env::var_os("APPDATA").map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn data_root() -> Option<PathBuf> {
    home_dir().map(|home| home.join("Library").join("Application Support"))
}

/// The user's home directory, from `$HOME` as every macOS login session sets it.
#[cfg(target_os = "macos")]
pub(crate) fn home_dir() -> Option<PathBuf> {
    home_from(env::var_os("HOME"))
}

#[cfg(target_os = "macos")]
fn home_from(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.filter(|home| !home.is_empty()).map(PathBuf::from)
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

#[cfg(windows)]
pub(crate) fn service_health_path() -> PathBuf {
    app_data_dir().join("service-health.json")
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn home_comes_from_the_environment_value() {
        assert_eq!(
            home_from(Some("/Users/someone".into())),
            Some(PathBuf::from("/Users/someone"))
        );
    }

    #[test]
    fn missing_or_empty_home_is_rejected() {
        assert_eq!(home_from(None), None);
        assert_eq!(home_from(Some("".into())), None);
    }
}
