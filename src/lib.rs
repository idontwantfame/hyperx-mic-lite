#![cfg(windows)]

mod app;
mod config;
mod constants;
mod logging;
mod model;
mod paths;
mod time;

pub use app::run_app;
