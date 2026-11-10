//! The macOS menu bar item, the counterpart of the Windows notification-area icon.
//!
//! The icon shows a slashed microphone while muted, and its menu offers the actions worth
//! having without opening the window: mute, lighting on/off and the effect.

use std::{
    collections::VecDeque,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
};

use super::{
    TrayCommand, TrayState,
    glyph::{GLYPH_SIZE, glyph_part, muted_glyph_part},
};
use crate::{logging::log_event, model::Effect};

const MENU_OPEN_ID: &str = "open";
const MENU_QUIT_ID: &str = "quit";
const MENU_MUTE_ID: &str = "mute";
const MENU_LIGHTING_ID: &str = "lighting";
const EFFECT_ID_PREFIX: &str = "effect:";

static TRAY_SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);
static TRAY_EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Menu choices waiting for the GUI, which drains them every tick.
static TRAY_COMMANDS: Mutex<VecDeque<TrayCommand>> = Mutex::new(VecDeque::new());

fn effect_id(effect: Effect) -> String {
    format!("{EFFECT_ID_PREFIX}{}", effect.as_config())
}

/// What a menu item asks for.
#[derive(Debug, PartialEq)]
enum MenuAction {
    Open,
    Quit,
    Command(TrayCommand),
}

impl MenuAction {
    fn from_id(id: &str) -> Option<Self> {
        match id {
            MENU_OPEN_ID => Some(Self::Open),
            MENU_QUIT_ID => Some(Self::Quit),
            MENU_MUTE_ID => Some(Self::Command(TrayCommand::ToggleMute)),
            MENU_LIGHTING_ID => Some(Self::Command(TrayCommand::ToggleLighting)),
            _ => id
                .strip_prefix(EFFECT_ID_PREFIX)
                .and_then(|name| {
                    Effect::ALL
                        .into_iter()
                        .find(|effect| effect.as_config() == name)
                })
                .map(|effect| Self::Command(TrayCommand::SelectEffect(effect))),
        }
    }
}

fn status_text(state: &TrayState) -> &'static str {
    if !state.connected {
        "QuadCast S: Disconnected"
    } else if state.muted {
        "QuadCast S: Muted"
    } else {
        "QuadCast S: Live"
    }
}

/// Menu entries whose text or check mark follows the mic state.
struct MenuItems {
    status: MenuItem,
    mute: MenuItem,
    lighting: CheckMenuItem,
    effect_menu: Submenu,
    effects: Vec<(Effect, CheckMenuItem)>,
}

pub(crate) struct TrayHandle {
    /// Removed from the menu bar when dropped. `None` if creating it failed, which is logged;
    /// the GUI keeps working without it.
    icon: Option<(TrayIcon, MenuItems)>,
    /// The state last shown, so an unchanged state costs nothing per tick.
    shown: Option<TrayState>,
}

impl TrayHandle {
    /// Create the menu bar item.
    ///
    /// AppKit requires this on the main thread with the event loop already running, which
    /// is where eframe builds and updates the GUI, the only caller.
    pub(crate) fn start() -> Self {
        MenuEvent::set_event_handler(Some(|event: MenuEvent| {
            match MenuAction::from_id(event.id.as_ref()) {
                Some(MenuAction::Open) => TRAY_SHOW_REQUESTED.store(true, Ordering::Relaxed),
                Some(MenuAction::Quit) => TRAY_EXIT_REQUESTED.store(true, Ordering::Relaxed),
                Some(MenuAction::Command(command)) => TRAY_COMMANDS
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push_back(command),
                None => {}
            }
        }));
        let icon = match build_menu_bar_item() {
            Ok(icon) => {
                log_event("info", "tray.start", &[]);
                Some(icon)
            }
            Err(error) => {
                log_event("warn", "tray.start.error", &[("message", error)]);
                None
            }
        };
        Self { icon, shown: None }
    }

    pub(crate) fn show_requested() -> bool {
        TRAY_SHOW_REQUESTED.swap(false, Ordering::Relaxed)
    }

    pub(crate) fn exit_requested() -> bool {
        TRAY_EXIT_REQUESTED.swap(false, Ordering::Relaxed)
    }

    pub(crate) fn take_command() -> Option<TrayCommand> {
        TRAY_COMMANDS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
    }

    /// Bring the icon, tooltip and menu in line with `state`.
    pub(crate) fn update(&mut self, state: &TrayState) {
        if self.shown.as_ref() == Some(state) {
            return;
        }
        let Some((icon, items)) = &self.icon else {
            return;
        };
        log_event(
            "info",
            "tray.state",
            &[
                ("connected", state.connected.to_string()),
                ("muted", state.muted.to_string()),
                ("lighting", state.lighting_on.to_string()),
            ],
        );
        // Disconnected counts as muted: nothing is being captured either way.
        let slashed = state.muted || !state.connected;
        match template_icon(slashed) {
            Ok(glyph) => {
                if let Err(error) = icon.set_icon_templated(Some(glyph)) {
                    log_event("warn", "tray.icon.error", &[("message", error.to_string())]);
                }
            }
            Err(error) => log_event("warn", "tray.icon.error", &[("message", error)]),
        }
        let status = status_text(state);
        if let Err(error) = icon.set_tooltip(Some(status)) {
            log_event(
                "warn",
                "tray.tooltip.error",
                &[("message", error.to_string())],
            );
        }
        items.status.set_text(status);
        items.mute.set_text(if state.muted {
            "Unmute microphone"
        } else {
            "Mute microphone"
        });
        items.mute.set_enabled(state.connected);
        items.lighting.set_checked(state.lighting_on);
        items
            .effect_menu
            .set_text(format!("Effect: {}", state.effect.label()));
        for (effect, item) in &items.effects {
            item.set_checked(*effect == state.effect);
        }
        self.shown = Some(state.clone());
    }
}

fn build_menu_bar_item() -> Result<(TrayIcon, MenuItems), String> {
    let items = MenuItems {
        status: MenuItem::new("QuadCast S", false, None),
        mute: MenuItem::with_id(MENU_MUTE_ID, "Mute microphone", true, None),
        // Not "Lighting": it sat one letter away from the Lightning effect below it.
        lighting: CheckMenuItem::with_id(MENU_LIGHTING_ID, "Lights on", true, false, None),
        effect_menu: Submenu::new("Effect", true),
        effects: Effect::ALL
            .into_iter()
            .map(|effect| {
                let item =
                    CheckMenuItem::with_id(effect_id(effect), effect.label(), true, false, None);
                (effect, item)
            })
            .collect(),
    };
    for (_, item) in &items.effects {
        items
            .effect_menu
            .append(item)
            .map_err(|error| error.to_string())?;
    }
    let open = MenuItem::with_id(MENU_OPEN_ID, "Open HyperX Mic Lite", true, None);
    let quit = MenuItem::with_id(MENU_QUIT_ID, "Quit", true, None);

    let menu = Menu::new();
    menu.append_items(&[
        &items.status,
        &items.mute,
        &PredefinedMenuItem::separator(),
        &items.lighting,
        &items.effect_menu,
        &PredefinedMenuItem::separator(),
        &open,
        &quit,
    ])
    .map_err(|error| error.to_string())?;

    let icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon_templated(template_icon(false)?)
        .with_tooltip("HyperX Mic Lite")
        .build()
        .map_err(|error| error.to_string())?;
    Ok((icon, items))
}

/// The microphone glyph as a template image: opaque where the glyph is drawn and
/// transparent elsewhere. macOS only uses the alpha, tinting it to match the menu bar.
fn template_icon(muted: bool) -> Result<Icon, String> {
    let part = if muted { muted_glyph_part } else { glyph_part };
    let rgba = (0..GLYPH_SIZE)
        .flat_map(|y| (0..GLYPH_SIZE).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let alpha = if part(x, y).is_some() { 255 } else { 0 };
            [0, 0, 0, alpha]
        })
        .collect();
    Icon::from_rgba(rgba, GLYPH_SIZE as u32, GLYPH_SIZE as u32).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(connected: bool, muted: bool) -> TrayState {
        TrayState {
            connected,
            muted,
            lighting_on: false,
            effect: Effect::Wave,
        }
    }

    #[test]
    fn menu_ids_map_to_their_actions() {
        assert_eq!(MenuAction::from_id(MENU_OPEN_ID), Some(MenuAction::Open));
        assert_eq!(MenuAction::from_id(MENU_QUIT_ID), Some(MenuAction::Quit));
        assert_eq!(
            MenuAction::from_id(MENU_MUTE_ID),
            Some(MenuAction::Command(TrayCommand::ToggleMute))
        );
        assert_eq!(
            MenuAction::from_id(&effect_id(Effect::VuMeter)),
            Some(MenuAction::Command(TrayCommand::SelectEffect(
                Effect::VuMeter
            )))
        );
        assert_eq!(MenuAction::from_id("effect:unknown"), None);
    }

    #[test]
    fn every_effect_round_trips_through_its_menu_id() {
        for effect in Effect::ALL {
            assert_eq!(
                MenuAction::from_id(&effect_id(effect)),
                Some(MenuAction::Command(TrayCommand::SelectEffect(effect)))
            );
        }
    }

    #[test]
    fn status_line_names_the_mic_state() {
        assert_eq!(status_text(&state(true, false)), "QuadCast S: Live");
        assert_eq!(status_text(&state(true, true)), "QuadCast S: Muted");
        assert_eq!(status_text(&state(false, true)), "QuadCast S: Disconnected");
    }
}
