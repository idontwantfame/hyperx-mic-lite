use std::{env, process};

use eframe::egui;
use windows::core::Result as WinResult;

use crate::{
    audio::{
        list_capture_devices, mic_status, print_devices_json, print_status_json, run_audio_command,
        set_mic_mute, set_volume, toggle_mic_mute,
    },
    com::ComApartment,
    config_cli::run_config_command,
    diagnostics::run_diagnostics_command,
    eventlog::run_eventlog_command,
    gui::MicLiteApp,
    lighting::{
        print_lighting_detection, print_lighting_hid_dump, run_hid_monitor, run_level_monitor,
        run_lighting_effect, run_lighting_save, run_lighting_solid, run_lighting_vu_test,
    },
    logging::{install_panic_hook, log_event},
    logs::run_logs_command,
    service::{run_service_command, run_windows_service, windows_service_error},
    startup::run_startup_command,
};
pub fn run_app() {
    install_panic_hook();
    log_event(
        "info",
        "app.start",
        &[("args", env::args().skip(1).collect::<Vec<_>>().join(" "))],
    );
    if let Err(error) = run() {
        log_event("error", "app.error", &[("message", error.to_string())]);
        eprintln!("{error}");
        process::exit(1);
    }
    log_event("info", "app.exit", &[]);
}

fn run() -> WinResult<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        usage();
        process::exit(2);
    }

    let _com = ComApartment::init()?;

    match args[0].as_str() {
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
        "eventlog" => run_eventlog_command(&args[1..]),
        "service" => run_service_command(&args[1..]),
        "startup" => run_startup_command(&args[1..]),
        "service-run" => {
            if let Err(error) = run_windows_service() {
                let message = windows_service_error(error);
                log_event("error", "service.dispatcher.error", &[("message", message)]);
                process::exit(1);
            }
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

fn usage() {
    eprintln!(
        "hyperx-mic-lite controls the default Windows microphone.\n\n\
Usage:\n\
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
  hyperx-mic-lite diagnostics export [directory]\n\
  hyperx-mic-lite eventlog <register|unregister|status>\n\
  hyperx-mic-lite service <install|uninstall|start|stop|status|run>\n\
  hyperx-mic-lite startup <install|uninstall|status>\n\
  hyperx-mic-lite gui [--start-minimized] [--layout-edit]"
    );
}

pub(crate) fn run_gui(args: &[String]) {
    let start_minimized = args
        .iter()
        .any(|arg| arg == "--start-minimized" || arg == "--minimized");
    let layout_edit = args.iter().any(|arg| arg == "--layout-edit");
    if args
        .iter()
        .any(|arg| arg != "--start-minimized" && arg != "--minimized" && arg != "--layout-edit")
    {
        eprintln!("Usage: hyperx-mic-lite gui [--start-minimized] [--layout-edit]");
        process::exit(2);
    }
    log_event("info", "gui.start", &[]);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("HyperX Mic Lite")
            .with_inner_size([980.0, 640.0])
            .with_min_inner_size([820.0, 540.0]),
        ..Default::default()
    };

    let result = eframe::run_native(
        "HyperX Mic Lite",
        options,
        Box::new(|context| {
            context.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(MicLiteApp::new(start_minimized, layout_edit)))
        }),
    );

    if let Err(error) = result {
        log_event("error", "gui.error", &[("message", error.to_string())]);
        eprintln!("{error}");
        process::exit(1);
    }
    log_event("info", "gui.exit", &[]);
}
