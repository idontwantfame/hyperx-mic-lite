//! Login startup through the per-user `Run` registry key.

use std::{io, path::Path};

use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, KEY_SET_VALUE},
};

use super::display_command;
use crate::constants::{RUN_KEY_PATH, STARTUP_VALUE_NAME};

pub(super) fn install(executable: &Path, arguments: &[&str]) -> Result<(), String> {
    let command = display_command(&executable.display().to_string(), arguments);
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run_key, _) = hkcu
        .create_subkey(RUN_KEY_PATH)
        .map_err(|error| error.to_string())?;
    run_key
        .set_value(STARTUP_VALUE_NAME, &command)
        .map_err(|error| error.to_string())
}

/// Remove the startup entry; `false` means there was none.
pub(super) fn uninstall() -> Result<bool, String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu
        .open_subkey_with_flags(RUN_KEY_PATH, KEY_SET_VALUE)
        .map_err(|error| error.to_string())?;
    match run_key.delete_value(STARTUP_VALUE_NAME) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn installed_command() -> Result<Option<String>, String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu
        .open_subkey(RUN_KEY_PATH)
        .map_err(|error| error.to_string())?;
    match run_key.get_value::<String, _>(STARTUP_VALUE_NAME) {
        Ok(command) => Ok(Some(command)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}
