#![cfg(windows)]

mod app;
mod audio;
mod config;
mod config_cli;
mod constants;
mod eventlog;
mod logging;
mod logs;
mod model;
mod paths;
mod startup;
mod time;
mod tray;

pub use app::run_app;
