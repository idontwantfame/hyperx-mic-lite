pub(crate) const CONFIG_SCHEMA_VERSION: u32 = 1;
pub(crate) const APP_NAME: &str = "HyperXMicLite";
pub(crate) const SERVICE_NAME: &str = "HyperXMicLite";
pub(crate) const SERVICE_DISPLAY_NAME: &str = "HyperX Mic Lite";
pub(crate) const SERVICE_DESCRIPTION: &str =
    "Restores HyperX Mic Lite microphone settings and hosts background device tasks.";
pub(crate) const STARTUP_VALUE_NAME: &str = "HyperXMicLite";
pub(crate) const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub(crate) const EVENTLOG_SOURCE_PATH: &str =
    r"SYSTEM\CurrentControlSet\Services\EventLog\Application\HyperXMicLite";
pub(crate) const EVENTLOG_TYPES_SUPPORTED: u32 = 0x0007;
pub(crate) const EVENTLOG_MESSAGE_ID: u32 = 0x40000001;
pub(crate) const TRAY_UID: u32 = 1;
pub(crate) const TRAY_MENU_OPEN: usize = 1001;
pub(crate) const TRAY_MENU_EXIT: usize = 1002;
