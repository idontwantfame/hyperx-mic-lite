//! Per-user GUI startup at login: the `Run` registry key on Windows, a LaunchAgent on macOS.

use std::{env, path::Path, process};

#[cfg(target_os = "macos")]
mod launch_agent;
#[cfg(windows)]
mod run_key;

#[cfg(target_os = "macos")]
use launch_agent as backend;
#[cfg(windows)]
use run_key as backend;

use crate::logging::{json_string, log_event};

pub(crate) fn run_startup_command(args: &[String]) {
    if args.is_empty() {
        startup_usage();
        process::exit(2);
    }

    let result = match args[0].as_str() {
        "install" => install_user_gui_startup(&args[1..]),
        "uninstall" | "delete" => uninstall_user_gui_startup(),
        "status" => print_user_gui_startup_status(),
        _ => {
            startup_usage();
            process::exit(2);
        }
    };

    if let Err(error) = result {
        eprintln!("{error}");
        log_event("error", "startup.command.error", &[("message", error)]);
        process::exit(1);
    }
}

fn startup_usage() {
    eprintln!(
        "Usage:\n\
  hyperx-mic-lite startup install [--minimized|--normal]\n\
  hyperx-mic-lite startup uninstall\n\
  hyperx-mic-lite startup status"
    );
}

/// The arguments the login item passes to the executable.
fn gui_arguments(start_minimized: bool) -> &'static [&'static str] {
    if start_minimized {
        &["gui", "--start-minimized"]
    } else {
        &["gui"]
    }
}

/// How a startup command is shown to the user, in the same form on every platform.
fn display_command(executable: &str, arguments: &[impl AsRef<str>]) -> String {
    let arguments = arguments
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join(" ");
    if arguments.is_empty() {
        format!("\"{executable}\"")
    } else {
        format!("\"{executable}\" {arguments}")
    }
}

fn install_user_gui_startup(args: &[String]) -> Result<(), String> {
    let start_minimized = !args.iter().any(|arg| arg == "--normal");
    if args
        .iter()
        .any(|arg| arg != "--minimized" && arg != "--normal")
    {
        return Err("Usage: hyperx-mic-lite startup install [--minimized|--normal]".to_string());
    }
    let executable_path = env::current_exe().map_err(|error| error.to_string())?;
    let command = install(&executable_path, gui_arguments(start_minimized))?;
    println!("Installed per-user GUI startup: {command}");
    log_event("info", "startup.gui.install", &[("command", command)]);
    Ok(())
}

fn install(executable: &Path, arguments: &[&str]) -> Result<String, String> {
    backend::install(executable, arguments)?;
    Ok(display_command(
        &executable.display().to_string(),
        arguments,
    ))
}

fn uninstall_user_gui_startup() -> Result<(), String> {
    if backend::uninstall()? {
        println!("Removed per-user GUI startup.");
        log_event("info", "startup.gui.uninstall", &[]);
    } else {
        println!("Per-user GUI startup was not installed.");
    }
    Ok(())
}

fn print_user_gui_startup_status() -> Result<(), String> {
    match backend::installed_command()? {
        Some(command) => println!(
            "{{\"installed\":true,\"command\":{}}}",
            json_string(&command)
        ),
        None => println!("{{\"installed\":false}}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimized_startup_passes_the_start_minimized_flag() {
        assert_eq!(gui_arguments(true), ["gui", "--start-minimized"]);
        assert_eq!(gui_arguments(false), ["gui"]);
    }

    #[test]
    fn displayed_command_quotes_the_executable_only() {
        assert_eq!(
            display_command(
                "/Applications/HyperX Mic Lite.app/x",
                &["gui", "--start-minimized"]
            ),
            "\"/Applications/HyperX Mic Lite.app/x\" gui --start-minimized"
        );
    }

    #[test]
    fn displayed_command_without_arguments_is_just_the_executable() {
        assert_eq!(display_command("/bin/x", &[] as &[&str]), "\"/bin/x\"");
    }
}
