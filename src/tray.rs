use std::{
    mem,
    sync::atomic::{AtomicBool, AtomicIsize, Ordering},
    thread,
};

use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{
            NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
            DispatchMessageW, GetCursorPos, GetMessageW, IDI_APPLICATION, LoadIconW, MF_STRING,
            MSG, PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, TPM_LEFTALIGN,
            TPM_RETURNCMD, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CLOSE,
            WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_RBUTTONUP, WNDCLASSW,
        },
    },
};
use windows::core::PCWSTR;

use crate::{
    constants::{TRAY_MENU_EXIT, TRAY_MENU_OPEN, TRAY_UID},
    logging::log_event,
};

const TRAY_CALLBACK_MESSAGE: u32 = WM_APP + 17;
static TRAY_SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);
static TRAY_EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);

pub(crate) struct TrayHandle;

impl TrayHandle {
    pub(crate) fn start() -> Self {
        thread::spawn(|| {
            if let Err(error) = run_tray_message_loop() {
                log_event("warn", "tray.start.error", &[("message", error)]);
            }
        });
        Self
    }

    pub(crate) fn show_requested() -> bool {
        TRAY_SHOW_REQUESTED.swap(false, Ordering::Relaxed)
    }

    pub(crate) fn exit_requested() -> bool {
        TRAY_EXIT_REQUESTED.swap(false, Ordering::Relaxed)
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
    unsafe extern "system" fn tray_window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            TRAY_CALLBACK_MESSAGE => match lparam.0 as u32 {
                WM_LBUTTONDBLCLK => {
                    TRAY_SHOW_REQUESTED.store(true, Ordering::Relaxed);
                    LRESULT(0)
                }
                WM_RBUTTONUP => {
                    show_tray_menu(hwnd);
                    LRESULT(0)
                }
                _ => LRESULT(0),
            },
            WM_COMMAND => {
                match wparam.0 & 0xffff {
                    TRAY_MENU_OPEN => TRAY_SHOW_REQUESTED.store(true, Ordering::Relaxed),
                    TRAY_MENU_EXIT => TRAY_EXIT_REQUESTED.store(true, Ordering::Relaxed),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_CLOSE | WM_DESTROY => {
                remove_tray_icon(hwnd);
                unsafe {
                    PostQuitMessage(0);
                }
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    let instance = unsafe { GetModuleHandleW(None).map_err(|error| error.to_string())? };
    let class_name = wide_null("HyperXMicLiteTrayWindow");
    let window_name = wide_null("HyperX Mic Lite Tray");
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(tray_window_proc),
        hInstance: instance.into(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    let atom = unsafe { RegisterClassW(&window_class) };
    if atom == 0 {
        return Err("RegisterClassW failed for tray window.".to_string());
    }

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
    TRAY_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
    add_tray_icon(hwnd)?;
    log_event("info", "tray.start", &[]);

    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0).into() } {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    TRAY_HWND.store(0, Ordering::Relaxed);
    log_event("info", "tray.stop", &[]);
    Ok(())
}

fn add_tray_icon(hwnd: HWND) -> Result<(), String> {
    let icon = unsafe { LoadIconW(None, IDI_APPLICATION).map_err(|error| error.to_string())? };
    let mut data = NOTIFYICONDATAW {
        cbSize: mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_CALLBACK_MESSAGE,
        hIcon: icon,
        ..Default::default()
    };
    copy_wide_into(&mut data.szTip, "HyperX Mic Lite");
    let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &data).as_bool() };
    if ok {
        Ok(())
    } else {
        Err("Shell_NotifyIconW(NIM_ADD) failed.".to_string())
    }
}

fn remove_tray_icon(hwnd: HWND) {
    let data = NOTIFYICONDATAW {
        cbSize: mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

fn show_tray_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        if menu.is_invalid() {
            return;
        }
        let open = wide_null("Open");
        let exit = wide_null("Exit");
        let _ = AppendMenuW(menu, MF_STRING, TRAY_MENU_OPEN, PCWSTR(open.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, TRAY_MENU_EXIT, PCWSTR(exit.as_ptr()));
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
