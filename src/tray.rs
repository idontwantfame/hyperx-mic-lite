use std::{
    mem,
    sync::atomic::{AtomicBool, Ordering},
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
            AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
            DispatchMessageW, GetCursorPos, GetMessageW, HICON, IDI_APPLICATION, LoadIconW,
            MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow,
            TPM_LEFTALIGN, TPM_RETURNCMD, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE,
            WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_RBUTTONUP, WNDCLASSW,
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
    add_tray_icon(hwnd)?;
    log_event("info", "tray.start", &[]);

    let mut message = MSG::default();
    // SAFETY: message is a valid MSG out-param owned by this frame; a None HWND filter is
    // explicitly allowed and retrieves messages for any window on this thread.
    while unsafe { GetMessageW(&mut message, None, 0, 0).into() } {
        // SAFETY: message was just filled in by the successful GetMessageW call above.
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    log_event("info", "tray.stop", &[]);
    Ok(())
}

/// Build a 32x32 microphone tray icon at runtime (no external .ico resource needed).
/// Transparency comes from the 1bpp AND mask; the 32bpp XOR bitmap carries the colours.
/// Both bitmaps are stored bottom-up as GDI expects.
fn create_app_icon() -> Option<HICON> {
    const SIZE: usize = 32;
    let mut xor = vec![0u8; SIZE * SIZE * 4];
    let mut and = vec![0xffu8; (SIZE / 8) * SIZE]; // start fully transparent

    let mut plot = |x: usize, y: usize, r: u8, g: u8, b: u8| {
        let row = SIZE - 1 - y; // bottom-up
        let idx = (row * SIZE + x) * 4;
        xor[idx] = b;
        xor[idx + 1] = g;
        xor[idx + 2] = r;
        xor[idx + 3] = 255;
        let byte = row * (SIZE / 8) + x / 8;
        and[byte] &= !(0x80u8 >> (x % 8)); // clear mask bit => opaque pixel
    };

    let rounded = |x: usize, y: usize, x0: usize, y0: usize, x1: usize, y1: usize, rad: f32| {
        if x < x0 || x > x1 || y < y0 || y > y1 {
            return false;
        }
        let cxl = x0 as f32 + rad;
        let cxr = x1 as f32 - rad;
        let cyt = y0 as f32 + rad;
        let cyb = y1 as f32 - rad;
        let dx = (cxl - x as f32).max(0.0).max(x as f32 - cxr);
        let dy = (cyt - y as f32).max(0.0).max(y as f32 - cyb);
        dx * dx + dy * dy <= rad * rad + 0.5
    };

    for y in 0..SIZE {
        for x in 0..SIZE {
            if rounded(x, y, 11, 3, 20, 19, 4.5) {
                // Capsule: vertical gradient from red to pink (matches the lighting theme).
                let t = ((y as f32 - 3.0) / 16.0).clamp(0.0, 1.0);
                let g = (32.0 * (1.0 - t)) as u8;
                let b = (16.0 + 138.0 * t) as u8;
                plot(x, y, 255, g, b);
            } else if (15..=16).contains(&x) && (19..=24).contains(&y) {
                plot(x, y, 210, 214, 218); // stem
            } else if rounded(x, y, 9, 25, 22, 27, 2.0) {
                plot(x, y, 210, 214, 218); // base
            }
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

fn add_tray_icon(hwnd: HWND) -> Result<(), String> {
    let icon = match create_app_icon() {
        Some(icon) => icon,
        // SAFETY: a None instance with IDI_APPLICATION loads the predefined system icon; no
        // caller-owned pointers are passed.
        None => unsafe { LoadIconW(None, IDI_APPLICATION).map_err(|error| error.to_string())? },
    };
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
    // SAFETY: data is a fully initialized NOTIFYICONDATAW with cbSize set to the struct size;
    // the tip text lives in the in-struct szTip array, so no external pointers are involved.
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
    // SAFETY: data has cbSize set and identifies the icon by hWnd/uID, the only fields
    // NIM_DELETE reads; no external pointers are involved.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

fn show_tray_menu(hwnd: HWND) {
    // SAFETY: the menu handle is checked for validity before use and destroyed before return;
    // open/exit are null-terminated UTF-16 buffers outliving AppendMenuW; hwnd comes from the
    // window proc, and GetCursorPos writes only to the local POINT out-param.
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
