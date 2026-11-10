pub(crate) const CONFIG_SCHEMA_VERSION: u32 = 1;
pub(crate) const APP_NAME: &str = "HyperXMicLite";
#[cfg(windows)]
pub(crate) const SERVICE_NAME: &str = "HyperXMicLite";
#[cfg(windows)]
pub(crate) const SERVICE_DISPLAY_NAME: &str = "HyperX Mic Lite";
#[cfg(windows)]
pub(crate) const SERVICE_DESCRIPTION: &str =
    "Restores HyperX Mic Lite microphone settings and hosts background device tasks.";
#[cfg(windows)]
pub(crate) const STARTUP_VALUE_NAME: &str = "HyperXMicLite";
#[cfg(windows)]
pub(crate) const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
pub(crate) const EVENTLOG_SOURCE_PATH: &str =
    r"SYSTEM\CurrentControlSet\Services\EventLog\Application\HyperXMicLite";
#[cfg(windows)]
pub(crate) const EVENTLOG_TYPES_SUPPORTED: u32 = 0x0007;
#[cfg(windows)]
pub(crate) const EVENTLOG_MESSAGE_ID: u32 = 0x40000001;
#[cfg(windows)]
pub(crate) const TRAY_UID: u32 = 1;
#[cfg(windows)]
pub(crate) const TRAY_MENU_OPEN: usize = 1001;
#[cfg(windows)]
pub(crate) const TRAY_MENU_EXIT: usize = 1002;
#[cfg(windows)]
pub(crate) const TRAY_MENU_MUTE: usize = 1003;
#[cfg(windows)]
pub(crate) const TRAY_MENU_LIGHTING: usize = 1004;
/// The tray menu's effect entries take this id plus their index in `Effect::ALL`.
#[cfg(windows)]
pub(crate) const TRAY_MENU_EFFECT_BASE: usize = 1100;
/// launchd label of the login item, also the plist's file name in `~/Library/LaunchAgents`.
#[cfg(target_os = "macos")]
pub(crate) const LAUNCH_AGENT_LABEL: &str = "local.hyperx-mic-lite.gui";
