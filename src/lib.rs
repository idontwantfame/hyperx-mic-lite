#![cfg(windows)]

mod app;
mod audio;
mod com;
mod config;
mod config_cli;
mod constants;
mod diagnostics;
mod eventlog;
mod gui;
mod gui_widgets;
mod lighting;
mod logging;
mod logs;
mod model;
mod paths;
mod service;
mod startup;
mod time;
mod tray;

pub use app::run_app;
