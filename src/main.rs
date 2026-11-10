#![cfg(any(windows, target_os = "macos"))]
#![windows_subsystem = "windows"]

fn main() {
    hyperx_mic_lite::run_app();
}
