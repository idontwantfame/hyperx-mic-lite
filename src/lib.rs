#![cfg(any(windows, target_os = "macos"))]

mod app;
mod audio;
#[cfg(windows)]
mod com;
mod config;
mod config_cli;
mod constants;
mod devices;
mod diagnostics;
#[cfg(windows)]
mod eventlog;
mod gui;
mod gui_widgets;
mod hid_caps;
mod lighting;
mod lighting_port;
mod logging;
mod logs;
mod model;
mod mqtt;
mod paths;
#[cfg(windows)]
mod service;
mod startup;
mod time;
mod tray;

pub use app::run_app;
