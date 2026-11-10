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

use crate::model::Effect;

/// What the icon and its menu reflect, refreshed by the GUI every tick.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrayState {
    pub(crate) connected: bool,
    pub(crate) muted: bool,
    pub(crate) lighting_on: bool,
    pub(crate) effect: Effect,
}

/// A menu choice for the GUI to carry out. Open and Quit have their own flags.
///
/// Only the macOS menu offers these so far; the Windows tray menu has just Open and Exit.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TrayCommand {
    ToggleMute,
    ToggleLighting,
    SelectEffect(Effect),
}

/// Whether the icon is shown for the app's whole life rather than only once the window is
/// hidden. macOS runs the GUI as a menu bar app without a Dock icon, so there the icon is
/// the only way back to the window.
pub(crate) const ALWAYS_SHOWN: bool = cfg!(target_os = "macos");
