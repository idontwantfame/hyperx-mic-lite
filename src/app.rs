use std::{env, error::Error, process};

use eframe::egui;
#[cfg(windows)]
use windows::{
    Win32::{
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::Console::{
            ATTACH_PARENT_PROCESS, AttachConsole, GetConsoleProcessList, STD_ERROR_HANDLE,
            STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
        },
    },
    core::w,
};

use crate::{
    audio::{
        list_capture_devices, mic_status, print_devices_json, print_status_json, run_audio_command,
        set_mic_mute, set_volume, toggle_mic_mute,
    },
    config::{AppConfig, load_or_create_config},
    config_cli::run_config_command,
    diagnostics::run_diagnostics_command,
    gui::MicLiteApp,
    lighting::{
        init_hid_on_this_thread, print_lighting_detection, print_lighting_hid_dump,
        run_hid_monitor, run_level_monitor, run_lighting_effect, run_lighting_save,
        run_lighting_solid, run_lighting_vu_test,
    },
    logging::{install_panic_hook, log_event},
    logs::run_logs_command,
    startup::run_startup_command,
};
#[cfg(windows)]
use crate::{
    com::ComApartment,
    eventlog::run_eventlog_command,
    service::{run_service_command, run_windows_service, windows_service_error},
};
pub fn run_app() {
    let launched_from_terminal = attach_parent_console();
    install_panic_hook();
    log_event(
        "info",
        "app.start",
        &[("args", env::args().skip(1).collect::<Vec<_>>().join(" "))],
    );
    if let Err(error) = run(launched_from_terminal) {
        log_event("error", "app.error", &[("message", error.to_string())]);
        eprintln!("{error}");
        process::exit(1);
    }
    log_event("info", "app.exit", &[]);
}

fn run(launched_from_terminal: bool) -> Result<(), Box<dyn Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    // Every path below may reach WASAPI from this thread.
    #[cfg(windows)]
    let _com = ComApartment::init()?;
    if args.is_empty() {
        if launched_from_terminal {
            startup_info();
        }
        run_gui(&[]);
        return Ok(());
    }

    match args[0].as_str() {
        "help" | "--help" | "-h" => usage(),
        "list" => print_devices_json(&list_capture_devices()?),
        "status" => print_status_json(&mic_status()?),
        "lighting-detect" => print_lighting_detection(),
        "lighting-hid-dump" => print_lighting_hid_dump(),
        "hid-monitor" => run_hid_monitor(&args[1..]),
        "level-monitor" => run_level_monitor(&args[1..]),
        "lighting-solid" => run_lighting_solid(&args[1..]),
        "lighting-effect" => run_lighting_effect(&args[1..]),
        "lighting-vu-test" => run_lighting_vu_test(&args[1..]),
        "lighting-save" => run_lighting_save(&args[1..]),
        "audio" => run_audio_command(&args[1..])?,
        "config" => run_config_command(&args[1..]),
        "logs" => run_logs_command(&args[1..]),
        "diagnostics" => run_diagnostics_command(&args[1..]),
        #[cfg(windows)]
        "eventlog" => run_eventlog_command(&args[1..]),
        #[cfg(windows)]
        "service" => run_service_command(&args[1..]),
        "startup" => run_startup_command(&args[1..]),
        #[cfg(windows)]
        "service-run" => {
            if let Err(error) = run_windows_service() {
                let message = windows_service_error(error);
                log_event("error", "service.dispatcher.error", &[("message", message)]);
                process::exit(1);
            }
        }
        #[cfg(not(windows))]
        "eventlog" | "service" | "service-run" => {
            eprintln!("`{}` is only available on Windows.", args[0]);
            process::exit(2);
        }
        "gui" => run_gui(&args[1..]),
        "mute" => {
            set_mic_mute(true)?;
            print_status_json(&mic_status()?);
        }
        "unmute" => {
            set_mic_mute(false)?;
            print_status_json(&mic_status()?);
        }
        "toggle" => {
            toggle_mic_mute()?;
            print_status_json(&mic_status()?);
        }
        "volume" => set_volume(&args[1..])?,
        _ => {
            usage();
            process::exit(2);
        }
    }

    Ok(())
}

/// A process started from a terminal on macOS already shares its stdio; only report whether
/// there is one, so the GUI knows to print the command hints.
#[cfg(not(windows))]
fn attach_parent_console() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

#[cfg(windows)]
fn attach_parent_console() -> bool {
    let attached =
        (unsafe { AttachConsole(ATTACH_PARENT_PROCESS).is_ok() }) || attached_to_existing_console();
    if attached {
        redirect_stdio_to_console();
    }
    attached
}

#[cfg(windows)]
fn attached_to_existing_console() -> bool {
    let mut processes = [0u32; 8];
    let count = unsafe { GetConsoleProcessList(&mut processes) };
    count > 1
}

#[cfg(windows)]
fn redirect_stdio_to_console() {
    unsafe {
        if let Ok(output) = CreateFileW(
            w!("CONOUT$"),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        ) {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, output);
            let _ = SetStdHandle(STD_ERROR_HANDLE, output);
        }

        if let Ok(input) = CreateFileW(
            w!("CONIN$"),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        ) {
            let _ = SetStdHandle(STD_INPUT_HANDLE, input);
        }
    }
}

fn startup_info() {
    eprintln!(
        "Starting HyperX Mic Lite GUI.\n\n\
Useful command-line options:\n\
  hyperx-mic-lite status                         Show current microphone state as JSON\n\
  hyperx-mic-lite mute | unmute | toggle         Control microphone mute\n\
  hyperx-mic-lite volume 75                      Set microphone volume\n\
  hyperx-mic-lite lighting-detect                Detect the lighting HID interface\n\
  hyperx-mic-lite lighting-effect wave forever   Run a lighting effect from the terminal\n\
  hyperx-mic-lite config dump                    Print saved app configuration\n\
  hyperx-mic-lite diagnostics export             Create a diagnostic bundle"
    );
    #[cfg(windows)]
    eprintln!("hyperx-mic-lite service status                 Show background service status");
    eprintln!(
        "hyperx-mic-lite gui --start-minimized          Start GUI hidden/minimized\n\n\
Run `hyperx-mic-lite help` to show the full command list.\n"
    );
}

fn usage() {
    eprintln!(
        "hyperx-mic-lite controls the HyperX QuadCast S. Commands that touch the microphone\n\
fail while it is unplugged rather than acting on another input device.\n\n\
Usage:\n\
  hyperx-mic-lite\n\
  hyperx-mic-lite list\n\
  hyperx-mic-lite status\n\
  hyperx-mic-lite mute\n\
  hyperx-mic-lite unmute\n\
  hyperx-mic-lite toggle\n\
  hyperx-mic-lite volume 75\n\
  hyperx-mic-lite audio volume <mic|monitoring|headphone> <0-100>\n\
  hyperx-mic-lite audio mute <mic|monitoring|headphone> <on|off>\n\
  hyperx-mic-lite lighting-detect\n\
  hyperx-mic-lite lighting-hid-dump\n\
  hyperx-mic-lite hid-monitor [seconds]\n\
  hyperx-mic-lite level-monitor [seconds]\n\
  hyperx-mic-lite lighting-solid ff0066 [seconds]\n\
  hyperx-mic-lite lighting-effect <solid|wave|cycle|pulse|blink|lightning|vu_meter> [seconds|forever]\n\
  hyperx-mic-lite lighting-vu-test <0-100> [seconds]\n\
  hyperx-mic-lite lighting-save [--packet-log]\n\
  hyperx-mic-lite config <path|dump|export|import|validate|reset>\n\
  hyperx-mic-lite logs <path|tail>\n\
  hyperx-mic-lite diagnostics export [directory]"
    );
    #[cfg(windows)]
    eprintln!(
        "hyperx-mic-lite eventlog <register|unregister|status>\n\
  hyperx-mic-lite service <install|uninstall|start|stop|status|run>"
    );
    eprintln!(
        "hyperx-mic-lite startup <install|uninstall|status>\n\
  hyperx-mic-lite gui [--start-minimized]"
    );
}

/// Run as a menu bar app on macOS: no Dock icon or app menu, and the menu bar item opens
/// the window. The bundle also sets `LSUIElement` so no Dock icon flashes at launch.
#[cfg(target_os = "macos")]
fn platform_event_loop_hook() -> Option<eframe::EventLoopBuilderHook> {
    use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
    Some(Box::new(|builder| {
        builder.with_activation_policy(ActivationPolicy::Accessory);
    }))
}

#[cfg(not(target_os = "macos"))]
fn platform_event_loop_hook() -> Option<eframe::EventLoopBuilderHook> {
    None
}

/// The GUI is laid out for one fixed window size.
const WINDOW_SIZE: [f32; 2] = [700.0, 598.0];

pub(crate) fn run_gui(args: &[String]) {
    let start_minimized = args
        .iter()
        .any(|arg| arg == "--start-minimized" || arg == "--minimized");
    if args
        .iter()
        .any(|arg| arg != "--start-minimized" && arg != "--minimized")
    {
        eprintln!("Usage: hyperx-mic-lite gui [--start-minimized]");
        process::exit(2);
    }
    log_event("info", "gui.start", &[]);
    if let Err(error) = init_hid_on_this_thread() {
        log_event(
            "warn",
            "gui.hid_init.error",
            &[("message", error.to_string())],
        );
    }
    let config = load_or_create_config().unwrap_or_else(|_| AppConfig::default());
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("HyperX Mic Lite")
        .with_inner_size(WINDOW_SIZE)
        .with_min_inner_size(WINDOW_SIZE)
        .with_max_inner_size(WINDOW_SIZE)
        .with_resizable(false);
    if let (Some(x), Some(y)) = (config.ui.window_x, config.ui.window_y) {
        viewport = viewport.with_position([x, y]);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        event_loop_builder: platform_event_loop_hook(),
        ..Default::default()
    };

    let result = eframe::run_native(
        "HyperX Mic Lite",
        options,
        Box::new(|context| {
            crate::gui::theme::apply(&context.egui_ctx);
            Ok(Box::new(MicLiteApp::new(start_minimized)))
        }),
    );

    if let Err(error) = result {
        log_event("error", "gui.error", &[("message", error.to_string())]);
        eprintln!("{error}");
        process::exit(1);
    }
    log_event("info", "gui.exit", &[]);
}
