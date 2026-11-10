//! The icon the GUI hides into: a notification-area icon on Windows, a menu bar item on
//! macOS.
//!
//! Both backends expose the same `TrayHandle`: `start()` creates the icon and `update()`
//! keeps the icon, tooltip and menu in line with the mic. Menu choices land in the flags
//! and queue below, which the GUI drains every tick.

mod glyph;
#[cfg(target_os = "macos")]
mod menu_bar;
#[cfg(windows)]
mod win32;

use std::{
    collections::VecDeque,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(target_os = "macos")]
pub(crate) use menu_bar::TrayHandle;
#[cfg(windows)]
pub(crate) use win32::TrayHandle;

use crate::{logging::log_event, model::Effect};

/// Not "Lighting": it sat one letter away from the Lightning effect below it.
const LIGHTS_ON_LABEL: &str = "Lights on";

static SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Menu choices waiting for the GUI, which drains them every tick.
static COMMANDS: Mutex<VecDeque<TrayCommand>> = Mutex::new(VecDeque::new());

/// What the icon and its menu reflect, refreshed by the GUI every tick.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrayState {
    pub(crate) connected: bool,
    pub(crate) muted: bool,
    pub(crate) lighting_on: bool,
    /// The whole-mic effect, or `None` in split mode, where each half runs its own.
    pub(crate) effect: Option<Effect>,
}

impl TrayState {
    /// Whether the icon shows the slashed microphone. Disconnected counts as muted:
    /// nothing is being captured either way.
    fn slashed(&self) -> bool {
        self.muted || !self.connected
    }

    /// The tooltip, and the menu's first line.
    fn status_text(&self) -> &'static str {
        if !self.connected {
            "QuadCast S: Disconnected"
        } else if self.muted {
            "QuadCast S: Muted"
        } else {
            "QuadCast S: Live"
        }
    }

    fn mute_label(&self) -> &'static str {
        if self.muted {
            "Unmute microphone"
        } else {
            "Mute microphone"
        }
    }

    /// The title of the effect submenu.
    fn effect_label(&self) -> String {
        match self.effect {
            Some(effect) => format!("Effect: {}", effect.label()),
            None => "Effect: Split".to_string(),
        }
    }

    fn log(&self) {
        log_event(
            "info",
            "tray.state",
            &[
                ("connected", self.connected.to_string()),
                ("muted", self.muted.to_string()),
                ("lighting", self.lighting_on.to_string()),
            ],
        );
    }
}

/// A menu choice for the GUI to carry out. Open and Quit have their own flags.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TrayCommand {
    ToggleMute,
    ToggleLighting,
    SelectEffect(Effect),
}

/// What a menu item asks for.
#[derive(Debug, PartialEq)]
enum MenuAction {
    Open,
    Quit,
    Command(TrayCommand),
}

impl MenuAction {
    /// Hand the choice to the GUI, which picks it up on its next tick.
    fn dispatch(self) {
        match self {
            Self::Open => SHOW_REQUESTED.store(true, Ordering::Relaxed),
            Self::Quit => EXIT_REQUESTED.store(true, Ordering::Relaxed),
            Self::Command(command) => COMMANDS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push_back(command),
        }
    }
}

/// Whether Open was chosen since the last call.
pub(crate) fn show_requested() -> bool {
    SHOW_REQUESTED.swap(false, Ordering::Relaxed)
}

/// Whether Quit was chosen since the last call.
pub(crate) fn exit_requested() -> bool {
    EXIT_REQUESTED.swap(false, Ordering::Relaxed)
}

/// The oldest menu command the GUI has not carried out yet.
pub(crate) fn take_command() -> Option<TrayCommand> {
    COMMANDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .pop_front()
}

/// Whether the icon is shown for the app's whole life rather than only once the window is
/// hidden. macOS runs the GUI as a menu bar app without a Dock icon, so there the icon is
/// the only way back to the window.
pub(crate) const ALWAYS_SHOWN: bool = cfg!(target_os = "macos");

#[cfg(test)]
mod tests {
    use super::*;

    fn state(connected: bool, muted: bool) -> TrayState {
        TrayState {
            connected,
            muted,
            lighting_on: false,
            effect: Some(Effect::Wave),
        }
    }

    #[test]
    fn status_line_names_the_mic_state() {
        assert_eq!(state(true, false).status_text(), "QuadCast S: Live");
        assert_eq!(state(true, true).status_text(), "QuadCast S: Muted");
        assert_eq!(state(false, true).status_text(), "QuadCast S: Disconnected");
    }

    #[test]
    fn icon_is_slashed_unless_connected_and_live() {
        assert!(!state(true, false).slashed());
        assert!(state(true, true).slashed());
        assert!(state(false, false).slashed());
    }

    #[test]
    fn mute_entry_offers_the_opposite_of_the_current_state() {
        assert_eq!(state(true, false).mute_label(), "Mute microphone");
        assert_eq!(state(true, true).mute_label(), "Unmute microphone");
    }

    #[test]
    fn effect_submenu_names_the_effect_or_split_mode() {
        let mut split = state(true, false);
        assert_eq!(split.effect_label(), "Effect: Wave");
        split.effect = None;
        assert_eq!(split.effect_label(), "Effect: Split");
    }
}
