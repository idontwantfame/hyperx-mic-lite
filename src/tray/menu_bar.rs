//! The macOS menu bar item, the counterpart of the Windows notification-area icon.

use std::sync::atomic::{AtomicBool, Ordering};

use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem},
};

use super::glyph::{GLYPH_SIZE, glyph_part};
use crate::logging::log_event;

const MENU_OPEN_ID: &str = "open";
const MENU_QUIT_ID: &str = "quit";

static TRAY_SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);
static TRAY_EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);

pub(crate) struct TrayHandle {
    /// Removed from the menu bar when dropped. `None` if creating it failed, which is logged;
    /// the GUI keeps working without it.
    _icon: Option<TrayIcon>,
}

impl TrayHandle {
    /// Create the menu bar item.
    ///
    /// AppKit requires this on the main thread with the event loop already running, which
    /// is where eframe builds and updates the GUI, the only caller.
    pub(crate) fn start() -> Self {
        MenuEvent::set_event_handler(Some(|event: MenuEvent| {
            if event.id == MENU_OPEN_ID {
                TRAY_SHOW_REQUESTED.store(true, Ordering::Relaxed);
            } else if event.id == MENU_QUIT_ID {
                TRAY_EXIT_REQUESTED.store(true, Ordering::Relaxed);
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
        Self { _icon: icon }
    }

    pub(crate) fn show_requested() -> bool {
        TRAY_SHOW_REQUESTED.swap(false, Ordering::Relaxed)
    }

    pub(crate) fn exit_requested() -> bool {
        TRAY_EXIT_REQUESTED.swap(false, Ordering::Relaxed)
    }
}

fn build_menu_bar_item() -> Result<TrayIcon, String> {
    let open = MenuItem::with_id(MENU_OPEN_ID, "Open HyperX Mic Lite", true, None);
    let quit = MenuItem::with_id(MENU_QUIT_ID, "Quit", true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &quit])
        .map_err(|error| error.to_string())?;
    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon_templated(template_icon()?)
        .with_tooltip("HyperX Mic Lite")
        .build()
        .map_err(|error| error.to_string())
}

/// The microphone glyph as a template image: opaque where the glyph is drawn and
/// transparent elsewhere. macOS only uses the alpha, tinting it to match the menu bar.
fn template_icon() -> Result<Icon, String> {
    let rgba = (0..GLYPH_SIZE)
        .flat_map(|y| (0..GLYPH_SIZE).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let alpha = if glyph_part(x, y).is_some() { 255 } else { 0 };
            [0, 0, 0, alpha]
        })
        .collect();
    Icon::from_rgba(rgba, GLYPH_SIZE as u32, GLYPH_SIZE as u32).map_err(|error| error.to_string())
}
