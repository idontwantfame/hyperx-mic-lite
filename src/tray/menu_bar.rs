//! The macOS menu bar item, the counterpart of the Windows notification-area icon.
//!
//! The icon shows a slashed microphone while muted, and its menu offers the actions worth
//! having without opening the window: mute, lighting on/off and the effect.

use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
};

use super::{
    LIGHTS_ON_LABEL, MenuAction, TrayCommand, TrayState,
    glyph::{GLYPH_SIZE, glyph_part, muted_glyph_part},
};
use crate::{logging::log_event, model::Effect};

const MENU_OPEN_ID: &str = "open";
const MENU_QUIT_ID: &str = "quit";
const MENU_MUTE_ID: &str = "mute";
const MENU_LIGHTING_ID: &str = "lighting";
const EFFECT_ID_PREFIX: &str = "effect:";

fn effect_id(effect: Effect) -> String {
    format!("{EFFECT_ID_PREFIX}{}", effect.as_config())
}

/// What the menu item with `id` asks for.
fn menu_action(id: &str) -> Option<MenuAction> {
    match id {
        MENU_OPEN_ID => Some(MenuAction::Open),
        MENU_QUIT_ID => Some(MenuAction::Quit),
        MENU_MUTE_ID => Some(MenuAction::Command(TrayCommand::ToggleMute)),
        MENU_LIGHTING_ID => Some(MenuAction::Command(TrayCommand::ToggleLighting)),
        _ => id
            .strip_prefix(EFFECT_ID_PREFIX)
            .and_then(|name| {
                Effect::ALL
                    .into_iter()
                    .find(|effect| effect.as_config() == name)
            })
            .map(|effect| MenuAction::Command(TrayCommand::SelectEffect(effect))),
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
            if let Some(action) = menu_action(event.id.as_ref()) {
                action.dispatch();
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

    /// Bring the icon, tooltip and menu in line with `state`.
    pub(crate) fn update(&mut self, state: &TrayState) {
        if self.shown.as_ref() == Some(state) {
            return;
        }
        let Some((icon, items)) = &self.icon else {
            return;
        };
        state.log();
        match template_icon(state.slashed()) {
            Ok(glyph) => {
                if let Err(error) = icon.set_icon_templated(Some(glyph)) {
                    log_event("warn", "tray.icon.error", &[("message", error.to_string())]);
                }
            }
            Err(error) => log_event("warn", "tray.icon.error", &[("message", error)]),
        }
        let status = state.status_text();
        if let Err(error) = icon.set_tooltip(Some(status)) {
            log_event(
                "warn",
                "tray.tooltip.error",
                &[("message", error.to_string())],
            );
        }
        items.status.set_text(status);
        items.mute.set_text(state.mute_label());
        items.mute.set_enabled(state.connected);
        items.lighting.set_checked(state.lighting_on);
        items.effect_menu.set_text(state.effect_label());
        for (effect, item) in &items.effects {
            item.set_checked(state.effect == Some(*effect));
        }
        self.shown = Some(state.clone());
    }
}

fn build_menu_bar_item() -> Result<(TrayIcon, MenuItems), String> {
    let items = MenuItems {
        status: MenuItem::new("QuadCast S", false, None),
        mute: MenuItem::with_id(MENU_MUTE_ID, "Mute microphone", true, None),
        lighting: CheckMenuItem::with_id(MENU_LIGHTING_ID, LIGHTS_ON_LABEL, true, false, None),
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
fn template_icon(slashed: bool) -> Result<Icon, String> {
    let part = if slashed {
        muted_glyph_part
    } else {
        glyph_part
    };
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

    #[test]
    fn menu_ids_map_to_their_actions() {
        assert_eq!(menu_action(MENU_OPEN_ID), Some(MenuAction::Open));
        assert_eq!(menu_action(MENU_QUIT_ID), Some(MenuAction::Quit));
        assert_eq!(
            menu_action(MENU_MUTE_ID),
            Some(MenuAction::Command(TrayCommand::ToggleMute))
        );
        assert_eq!(
            menu_action(&effect_id(Effect::VuMeter)),
            Some(MenuAction::Command(TrayCommand::SelectEffect(
                Effect::VuMeter
            )))
        );
        assert_eq!(menu_action("effect:unknown"), None);
    }

    #[test]
    fn every_effect_round_trips_through_its_menu_id() {
        for effect in Effect::ALL {
            assert_eq!(
                menu_action(&effect_id(effect)),
                Some(MenuAction::Command(TrayCommand::SelectEffect(effect)))
            );
        }
    }
}
