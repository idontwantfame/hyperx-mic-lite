//! The icon the GUI hides into: a notification-area icon on Windows, a menu bar item on
//! macOS.
//!
//! Both backends expose the same `TrayHandle`. `start()` creates the icon, and the GUI
//! polls `show_requested()` and `exit_requested()` every frame.

mod glyph;
#[cfg(target_os = "macos")]
mod menu_bar;
#[cfg(windows)]
mod win32;

#[cfg(target_os = "macos")]
pub(crate) use menu_bar::TrayHandle;
#[cfg(windows)]
pub(crate) use win32::TrayHandle;
