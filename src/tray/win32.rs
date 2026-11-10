//! The Windows notification-area icon, the counterpart of the macOS menu bar item.
//!
//! A thread of its own owns the icon, a hidden window receiving its clicks, and the popup
//! menu. The GUI hands it new states through `TrayHandle::update`, and the menu is rebuilt
//! from the latest one on every right-click.

use std::{
    ffi::c_void,
    mem, ptr,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicPtr, Ordering},
    },
    thread,
};

use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{
            NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFY_ICON_MESSAGE,
            NOTIFYICONDATAW, Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
            DispatchMessageW, GetCursorPos, GetMessageW, HICON, HMENU, IDI_APPLICATION, LoadIconW,
            MENU_ITEM_FLAGS, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG,
            PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetMenuDefaultItem,
            TPM_LEFTALIGN, TPM_RETURNCMD, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE,
            WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_RBUTTONUP, WNDCLASSW,
        },
    },
};
use windows::core::PCWSTR;

use super::{
    LIGHTS_ON_LABEL, MenuAction, TrayCommand, TrayState,
    glyph::{GLYPH_SIZE, GlyphPart, glyph_part, muted_glyph_part},
};
use crate::{
    constants::{
        TRAY_MENU_EFFECT_BASE, TRAY_MENU_EXIT, TRAY_MENU_LIGHTING, TRAY_MENU_MUTE, TRAY_MENU_OPEN,
        TRAY_UID,
    },
    logging::log_event,
    model::Effect,
};

const TRAY_CALLBACK_MESSAGE: u32 = WM_APP + 17;
/// Posted by `update` to wake the tray thread after a state change.
const TRAY_STATE_MESSAGE: u32 = WM_APP + 18;
/// The tooltip until the GUI reports the first state.
const APP_TOOLTIP: &str = "HyperX Mic Lite";

/// The slashed microphone is grey under a red slash; the live one keeps the stand grey.
const MUTED_GREY: [u8; 3] = [138, 142, 148];
const SLASH_RED: [u8; 3] = [255, 64, 64];
const STAND_GREY: [u8; 3] = [210, 214, 218];

/// The tray thread's hidden window, null until it exists and again once it is destroyed.
static TRAY_WINDOW: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
/// The state last passed to `update`, which the icon shows and the menu is built from.
static LATEST_STATE: Mutex<Option<TrayState>> = Mutex::new(None);

fn latest_state() -> Option<TrayState> {
    LATEST_STATE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

pub(crate) struct TrayHandle {
    /// The state last handed to the tray thread, so an unchanged state costs nothing per
    /// tick.
    shown: Option<TrayState>,
}

impl TrayHandle {
    pub(crate) fn start() -> Self {
        thread::spawn(|| {
            if let Err(error) = run_tray_message_loop() {
                log_event("warn", "tray.start.error", &[("message", error)]);
            }
        });
        Self { shown: None }
    }

    /// Bring the icon, tooltip and menu in line with `state`.
    ///
    /// The tray thread owns the icon, so this stores the state and wakes that thread.
    pub(crate) fn update(&mut self, state: &TrayState) {
        if self.shown.as_ref() == Some(state) {
            return;
        }
        state.log();
        *LATEST_STATE.lock().unwrap_or_else(PoisonError::into_inner) = Some(state.clone());
        self.shown = Some(state.clone());
        // Stored before the window is looked up: a tray thread that has not published its
        // window yet reads the state only after publishing, so it cannot miss this one.
        let window = TRAY_WINDOW.load(Ordering::SeqCst);
        if window.is_null() {
            return;
        }
        // SAFETY: PostMessageW only queues the message, takes no pointers, and fails cleanly
        // if the window has been destroyed in the meantime.
        if let Err(error) =
            unsafe { PostMessageW(Some(HWND(window)), TRAY_STATE_MESSAGE, WPARAM(0), LPARAM(0)) }
        {
            log_event(
                "warn",
                "tray.state.error",
                &[("message", error.to_string())],
            );
        }
    }
}

/// What the menu entry with `id` asks for.
fn menu_action(id: usize) -> Option<MenuAction> {
    match id {
        TRAY_MENU_OPEN => Some(MenuAction::Open),
        TRAY_MENU_EXIT => Some(MenuAction::Quit),
        TRAY_MENU_MUTE => Some(MenuAction::Command(TrayCommand::ToggleMute)),
        TRAY_MENU_LIGHTING => Some(MenuAction::Command(TrayCommand::ToggleLighting)),
        _ => id
            .checked_sub(TRAY_MENU_EFFECT_BASE)
            .and_then(|index| Effect::ALL.get(index).copied())
            .map(|effect| MenuAction::Command(TrayCommand::SelectEffect(effect))),
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

fn copy_wide_into<const N: usize>(target: &mut [u16; N], value: &str) {
    let wide = value.encode_utf16().take(N.saturating_sub(1));
    for (index, code) in wide.enumerate() {
        target[index] = code;
    }
}

fn run_tray_message_loop() -> Result<(), String> {
    // SAFETY: only registered as lpfnWndProc below; the OS invokes it on the thread that
    // created the window with a valid HWND and the raw message arguments it dispatched.
    unsafe extern "system" fn tray_window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            TRAY_CALLBACK_MESSAGE => match lparam.0 as u32 {
                WM_LBUTTONDBLCLK => {
                    MenuAction::Open.dispatch();
                    LRESULT(0)
                }
                WM_RBUTTONUP => {
                    show_tray_menu(hwnd);
                    LRESULT(0)
                }
                _ => LRESULT(0),
            },
            WM_COMMAND => {
                if let Some(action) = menu_action(wparam.0 & 0xffff) {
                    action.dispatch();
                }
                LRESULT(0)
            }
            WM_CLOSE | WM_DESTROY => {
                TRAY_WINDOW.store(ptr::null_mut(), Ordering::SeqCst);
                remove_tray_icon(hwnd);
                // SAFETY: PostQuitMessage takes no pointers and runs on the thread that owns
                // this window's message loop (the window proc is invoked on that thread).
                unsafe {
                    PostQuitMessage(0);
                }
                LRESULT(0)
            }
            // SAFETY: forwards the OS-supplied hwnd/message/wparam/lparam unchanged to the
            // default window procedure.
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    // SAFETY: GetModuleHandleW(None) passes a null module name, which is explicitly allowed
    // and returns the handle of the current process image.
    let instance = unsafe { GetModuleHandleW(None).map_err(|error| error.to_string())? };
    let class_name = wide_null("HyperXMicLiteTrayWindow");
    let window_name = wide_null("HyperX Mic Lite Tray");
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(tray_window_proc),
        hInstance: instance.into(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    // SAFETY: window_class points at class_name, a live null-terminated UTF-16 buffer, and
    // lpfnWndProc is Some(tray_window_proc); RegisterClassW copies the data it needs.
    let atom = unsafe { RegisterClassW(&window_class) };
    if atom == 0 {
        return Err("RegisterClassW failed for tray window.".to_string());
    }

    // SAFETY: class_name and window_name are null-terminated UTF-16 buffers that outlive the
    // call, and the class named by class_name was registered above with a valid window proc.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(window_name.as_ptr()),
            Default::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|error| error.to_string())?;
    let icons = TrayIcons::create()?;
    // Published before the icon reads the state; see `TrayHandle::update`.
    TRAY_WINDOW.store(hwnd.0, Ordering::SeqCst);
    notify_tray(NIM_ADD, hwnd, &icons)?;
    log_event("info", "tray.start", &[]);

    let mut message = MSG::default();
    // SAFETY: message is a valid MSG out-param owned by this frame; a None HWND filter is
    // explicitly allowed and retrieves messages for any window on this thread.
    while unsafe { GetMessageW(&mut message, None, 0, 0).into() } {
        // Handled here rather than in the window proc, which cannot reach the icons.
        if message.message == TRAY_STATE_MESSAGE {
            if let Err(error) = notify_tray(NIM_MODIFY, hwnd, &icons) {
                log_event("warn", "tray.icon.error", &[("message", error)]);
            }
            continue;
        }
        // SAFETY: message was just filled in by the successful GetMessageW call above.
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    log_event("info", "tray.stop", &[]);
    Ok(())
}

/// The two icons the tray switches between, created once and kept for the thread's life.
struct TrayIcons {
    live: HICON,
    slashed: HICON,
}

impl TrayIcons {
    fn create() -> Result<Self, String> {
        Ok(Self {
            live: tray_icon(false)?,
            slashed: tray_icon(true)?,
        })
    }

    fn for_state(&self, state: Option<&TrayState>) -> HICON {
        if state.is_some_and(TrayState::slashed) {
            self.slashed
        } else {
            self.live
        }
    }
}

/// The drawn microphone, or the stock application icon if drawing it fails.
fn tray_icon(slashed: bool) -> Result<HICON, String> {
    match create_glyph_icon(slashed) {
        Some(icon) => Ok(icon),
        // SAFETY: a None instance with IDI_APPLICATION loads the predefined system icon; no
        // caller-owned pointers are passed.
        None => unsafe { LoadIconW(None, IDI_APPLICATION).map_err(|error| error.to_string()) },
    }
}

/// The colour of one glyph pixel: the live microphone in the lighting theme's red to pink,
/// the slashed one grey under a red slash.
fn pixel_color(part: GlyphPart, y: usize, slashed: bool) -> [u8; 3] {
    match part {
        GlyphPart::Slash => SLASH_RED,
        _ if slashed => MUTED_GREY,
        GlyphPart::Capsule => {
            // Vertical gradient from red to pink.
            let t = ((y as f32 - 3.0) / 16.0).clamp(0.0, 1.0);
            [255, (32.0 * (1.0 - t)) as u8, (16.0 + 138.0 * t) as u8]
        }
        GlyphPart::Stand => STAND_GREY,
    }
}

/// Build a 32x32 microphone tray icon at runtime (no external .ico resource needed).
/// Transparency comes from the 1bpp AND mask; the 32bpp XOR bitmap carries the colours.
/// Both bitmaps are stored bottom-up as GDI expects.
fn create_glyph_icon(slashed: bool) -> Option<HICON> {
    const SIZE: usize = GLYPH_SIZE;
    let part = if slashed {
        muted_glyph_part
    } else {
        glyph_part
    };
    let mut xor = vec![0u8; SIZE * SIZE * 4];
    let mut and = [0xffu8; (SIZE / 8) * SIZE]; // start fully transparent

    for y in 0..SIZE {
        for x in 0..SIZE {
            let Some(part) = part(x, y) else {
                continue;
            };
            let [r, g, b] = pixel_color(part, y, slashed);
            let row = SIZE - 1 - y; // bottom-up
            let idx = (row * SIZE + x) * 4;
            xor[idx..idx + 4].copy_from_slice(&[b, g, r, 255]);
            and[row * (SIZE / 8) + x / 8] &= !(0x80u8 >> (x % 8)); // clear mask bit => opaque
        }
    }

    // SAFETY: and (1bpp mask, SIZE/8 bytes per row) and xor (32bpp BGRA) are sized exactly for
    // the 32x32, 1-plane, 32-bits-per-pixel icon requested, and both buffers outlive the call.
    unsafe {
        CreateIcon(
            None,
            SIZE as i32,
            SIZE as i32,
            1,
            32,
            and.as_ptr(),
            xor.as_ptr(),
        )
    }
    .ok()
}

/// Add (`NIM_ADD`) or refresh (`NIM_MODIFY`) the icon so it shows the latest state.
fn notify_tray(action: NOTIFY_ICON_MESSAGE, hwnd: HWND, icons: &TrayIcons) -> Result<(), String> {
    let state = latest_state();
    let mut data = NOTIFYICONDATAW {
        cbSize: mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_CALLBACK_MESSAGE,
        hIcon: icons.for_state(state.as_ref()),
        ..Default::default()
    };
    let tooltip = state.as_ref().map_or(APP_TOOLTIP, TrayState::status_text);
    copy_wide_into(&mut data.szTip, tooltip);
    // SAFETY: data is a fully initialized NOTIFYICONDATAW with cbSize set to the struct size;
    // the tip text lives in the in-struct szTip array, so no external pointers are involved.
    let ok = unsafe { Shell_NotifyIconW(action, &data).as_bool() };
    if ok {
        Ok(())
    } else {
        Err(format!("Shell_NotifyIconW({}) failed.", action.0))
    }
}

fn remove_tray_icon(hwnd: HWND) {
    let data = NOTIFYICONDATAW {
        cbSize: mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    };
    // SAFETY: data has cbSize set and identifies the icon by hWnd/uID, the only fields
    // NIM_DELETE reads; no external pointers are involved.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

/// Append one entry. The menu copies the text.
fn append_item(
    menu: HMENU,
    flags: MENU_ITEM_FLAGS,
    id: usize,
    text: &str,
) -> windows::core::Result<()> {
    let text = wide_null(text);
    // SAFETY: text is a null-terminated UTF-16 buffer that outlives the call; AppendMenuW
    // rejects an invalid menu handle with an error.
    unsafe { AppendMenuW(menu, flags, id, PCWSTR(text.as_ptr())) }
}

fn append_separator(menu: HMENU) -> windows::core::Result<()> {
    // SAFETY: a separator takes no text, so the null string pointer is never read.
    unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) }
}

fn checked(on: bool) -> MENU_ITEM_FLAGS {
    if on { MF_CHECKED } else { MF_STRING }
}

/// The effect submenu, with the running whole-mic effect checked.
fn append_effect_submenu(menu: HMENU, state: &TrayState) -> windows::core::Result<()> {
    // SAFETY: CreatePopupMenu takes no arguments; the handle is destroyed below on failure
    // and by the parent menu's DestroyMenu once attached.
    let effects = unsafe { CreatePopupMenu()? };
    let attached = Effect::ALL
        .into_iter()
        .enumerate()
        .try_for_each(|(index, effect)| {
            append_item(
                effects,
                checked(state.effect == Some(effect)),
                TRAY_MENU_EFFECT_BASE + index,
                effect.label(),
            )
        })
        .and_then(|()| append_item(menu, MF_POPUP, effects.0 as usize, &state.effect_label()));
    if attached.is_err() {
        // SAFETY: effects was created above and is not attached to the menu.
        unsafe {
            let _ = DestroyMenu(effects);
        }
    }
    attached
}

/// The menu's entries: the mic state and its actions once the GUI has reported a state,
/// then Open and Exit.
fn fill_tray_menu(menu: HMENU, state: Option<&TrayState>) -> windows::core::Result<()> {
    if let Some(state) = state {
        append_item(menu, MF_GRAYED, 0, state.status_text())?;
        let mute_flags = if state.connected {
            MF_STRING
        } else {
            MF_GRAYED
        };
        append_item(menu, mute_flags, TRAY_MENU_MUTE, state.mute_label())?;
        append_separator(menu)?;
        append_item(
            menu,
            checked(state.lighting_on),
            TRAY_MENU_LIGHTING,
            LIGHTS_ON_LABEL,
        )?;
        append_effect_submenu(menu, state)?;
        append_separator(menu)?;
    }
    append_item(menu, MF_STRING, TRAY_MENU_OPEN, "Open")?;
    append_item(menu, MF_STRING, TRAY_MENU_EXIT, "Exit")?;
    // Bold, as the action a double-click on the icon takes.
    // SAFETY: menu is a live menu handle owned by the caller; no pointers are passed.
    unsafe { SetMenuDefaultItem(menu, TRAY_MENU_OPEN as u32, 0) }
}

fn show_tray_menu(hwnd: HWND) {
    // SAFETY: the menu handle is checked for validity before use and destroyed (with its
    // submenu) before return; hwnd comes from the window proc, and GetCursorPos writes only
    // to the local POINT out-param.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        if menu.is_invalid() {
            return;
        }
        if let Err(error) = fill_tray_menu(menu, latest_state().as_ref()) {
            log_event("warn", "tray.menu.error", &[("message", error.to_string())]);
        }
        let mut point = POINT::default();
        if GetCursorPos(&mut point).is_ok() {
            let _ = SetForegroundWindow(hwnd);
            let command = TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_RETURNCMD,
                point.x,
                point.y,
                Some(0),
                hwnd,
                None,
            );
            if command.0 != 0 {
                let _ = PostMessageW(
                    Some(hwnd),
                    WM_COMMAND,
                    WPARAM(command.0 as usize),
                    LPARAM(0),
                );
            }
        }
        let _ = DestroyMenu(menu);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_map_to_their_actions() {
        assert_eq!(menu_action(TRAY_MENU_OPEN), Some(MenuAction::Open));
        assert_eq!(menu_action(TRAY_MENU_EXIT), Some(MenuAction::Quit));
        assert_eq!(
            menu_action(TRAY_MENU_MUTE),
            Some(MenuAction::Command(TrayCommand::ToggleMute))
        );
        assert_eq!(
            menu_action(TRAY_MENU_LIGHTING),
            Some(MenuAction::Command(TrayCommand::ToggleLighting))
        );
    }

    #[test]
    fn every_effect_round_trips_through_its_menu_id() {
        for (index, effect) in Effect::ALL.into_iter().enumerate() {
            assert_eq!(
                menu_action(TRAY_MENU_EFFECT_BASE + index),
                Some(MenuAction::Command(TrayCommand::SelectEffect(effect)))
            );
        }
    }

    #[test]
    fn unknown_menu_ids_do_nothing() {
        assert_eq!(menu_action(0), None);
        assert_eq!(menu_action(TRAY_MENU_LIGHTING + 1), None);
        assert_eq!(menu_action(TRAY_MENU_EFFECT_BASE + Effect::ALL.len()), None);
    }

    #[test]
    fn slashed_icon_is_grey_under_a_red_slash() {
        assert_eq!(pixel_color(GlyphPart::Slash, 10, true), SLASH_RED);
        assert_eq!(pixel_color(GlyphPart::Capsule, 10, true), MUTED_GREY);
        assert_eq!(pixel_color(GlyphPart::Stand, 26, true), MUTED_GREY);
    }

    #[test]
    fn live_icon_keeps_the_red_capsule_and_grey_stand() {
        assert_eq!(pixel_color(GlyphPart::Capsule, 3, false), [255, 32, 16]);
        assert_eq!(pixel_color(GlyphPart::Stand, 26, false), STAND_GREY);
    }
}
