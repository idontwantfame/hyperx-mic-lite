use std::{env, process};

use winreg::{RegKey, enums::HKEY_CURRENT_USER};

use crate::{
    constants::{RUN_KEY_PATH, STARTUP_VALUE_NAME},
    logging::{json_string, log_event},
};

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

fn install_user_gui_startup(args: &[String]) -> Result<(), String> {
    let start_minimized = !args.iter().any(|arg| arg == "--normal");
    if args
        .iter()
        .any(|arg| arg != "--minimized" && arg != "--normal")
    {
        return Err("Usage: hyperx-mic-lite startup install [--minimized|--normal]".to_string());
    }
    let executable_path = env::current_exe().map_err(|error| error.to_string())?;
    let command = if start_minimized {
        format!("\"{}\" gui --start-minimized", executable_path.display())
    } else {
        format!("\"{}\" gui", executable_path.display())
    };
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run_key, _) = hkcu
        .create_subkey(RUN_KEY_PATH)
        .map_err(|error| error.to_string())?;
    run_key
        .set_value(STARTUP_VALUE_NAME, &command)
        .map_err(|error| error.to_string())?;
    println!("Installed per-user GUI startup: {command}");
    log_event(
        "info",
        "startup.gui.install",
        &[("command", command.to_string())],
    );
    Ok(())
}

fn uninstall_user_gui_startup() -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu
        .open_subkey_with_flags(RUN_KEY_PATH, winreg::enums::KEY_SET_VALUE)
        .map_err(|error| error.to_string())?;
    match run_key.delete_value(STARTUP_VALUE_NAME) {
        Ok(()) => {
            println!("Removed per-user GUI startup.");
            log_event("info", "startup.gui.uninstall", &[]);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("Per-user GUI startup was not installed.");
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn print_user_gui_startup_status() -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu
        .open_subkey(RUN_KEY_PATH)
        .map_err(|error| error.to_string())?;
    let command = run_key.get_value::<String, _>(STARTUP_VALUE_NAME);
    match command {
        Ok(command) => {
            println!(
                "{{\"installed\":true,\"command\":{}}}",
                json_string(&command)
            );
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("{{\"installed\":false}}");
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}
