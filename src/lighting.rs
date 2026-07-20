use eframe::egui;
use std::{
    ffi::CStr,
    process,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Devices::HumanInterfaceDevice::{
            HIDP_CAPS, HIDP_STATUS_SUCCESS, HidD_FreePreparsedData, HidD_GetPreparsedData,
            HidP_GetCaps, PHIDP_PREPARSED_DATA,
        },
        Foundation::CloseHandle,
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        },
    },
    core::PCWSTR,
};

use crate::{
    audio::{
        AudioPeakMonitor, input_peak_value, mic_status, peak_from_bits, start_audio_peak_monitor,
    },
    com::ComApartment,
    config::{AppConfig, load_or_create_config},
    logging::log_event,
    model::{Effect, HidEvent, LightTarget, LightingDevice, PolarPattern},
};
#[derive(Debug)]
pub(crate) enum LightingError {
    NoDevice,
    Hid(String),
    Invalid(String),
}

impl std::fmt::Display for LightingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDevice => {
                write!(
                    formatter,
                    "No supported QuadCast S lighting HID interface detected."
                )
            }
            Self::Hid(message) | Self::Invalid(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for LightingError {}

pub(crate) struct LightingState {
    pub(crate) effect: Effect,
    pub(crate) target: LightTarget,
    pub(crate) split_layers: bool,
    pub(crate) top_effect: Effect,
    pub(crate) bottom_effect: Effect,
    pub(crate) colors: Vec<egui::Color32>,
    pub(crate) selected_color: usize,
    pub(crate) opacity: u8,
    pub(crate) speed: u8,
    pub(crate) brightness: u8,
    pub(crate) live_when_muted: bool,
}

#[derive(Clone)]
pub(crate) struct LightingProgram {
    pub(crate) effect: Effect,
    pub(crate) target: LightTarget,
    pub(crate) split_layers: bool,
    pub(crate) top_effect: Effect,
    pub(crate) bottom_effect: Effect,
    pub(crate) colors: Vec<[u8; 3]>,
    pub(crate) speed: u8,
    pub(crate) brightness: u8,
    pub(crate) shared_peak_bits: Option<Arc<AtomicU32>>,
}

const LIGHTING_CELL_COUNT: usize = 16;
type LightingFrame = [[u8; 3]; LIGHTING_CELL_COUNT];

#[derive(Clone, Copy)]
pub(crate) enum StreamDuration {
    Timed(Duration),
    Forever,
}

pub(crate) fn print_lighting_detection() {
    match detect_lighting_device() {
        Some(device) => {
            log_event(
                "info",
                "lighting.detect",
                &[
                    ("vendor_id", format!("{:04x}", device.vendor_id)),
                    ("product_id", format!("{:04x}", device.product_id)),
                    ("interface", device.interface_number.to_string()),
                    ("usage_page", format!("{:04x}", device.usage_page)),
                    ("usage", format!("{:04x}", device.usage)),
                ],
            );
            println!("Lighting controller detected:");
            println!(
                "  VID:PID: {:04x}:{:04x}",
                device.vendor_id, device.product_id
            );
            println!("  Interface: {}", device.interface_number);
            println!("  Usage: {:04x}:{:04x}", device.usage_page, device.usage);
            println!("  Name: {} {}", device.manufacturer, device.product);
        }
        None => {
            log_event("warn", "lighting.detect.none", &[]);
            println!("No supported HyperX QuadCast S lighting controller was detected.");
        }
    }
}

pub(crate) fn print_lighting_hid_dump() {
    let api = match hidapi::HidApi::new() {
        Ok(api) => api,
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    };

    for device in api
        .device_list()
        .filter(|device| is_supported_lighting_device(device))
    {
        println!(
            "{:04x}:{:04x} interface {} usage {:04x}:{:04x}",
            device.vendor_id(),
            device.product_id(),
            device.interface_number(),
            device.usage_page(),
            device.usage()
        );
        println!(
            "  {} {}",
            device.manufacturer_string().unwrap_or(""),
            device.product_string().unwrap_or("")
        );
        match hid_caps_for_path(device.path()) {
            Ok(caps) => {
                println!(
                    "  reports: input={} output={} feature={}",
                    caps.InputReportByteLength,
                    caps.OutputReportByteLength,
                    caps.FeatureReportByteLength
                );
                println!(
                    "  caps: input-values={} output-values={} feature-values={}",
                    caps.NumberInputValueCaps,
                    caps.NumberOutputValueCaps,
                    caps.NumberFeatureValueCaps
                );
            }
            Err(error) => println!("  caps: {error}"),
        }
    }
}

pub(crate) fn hid_caps_for_path(path: &CStr) -> Result<HIDP_CAPS, LightingError> {
    let path = path.to_string_lossy();
    let wide_path = path.encode_utf16().chain([0]).collect::<Vec<_>>();
    // SAFETY: wide_path is a null-terminated UTF-16 buffer that outlives the call; access mask 0
    // with OPEN_EXISTING is valid for opening a HID device just to query its capabilities.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide_path.as_ptr()),
            0,
            FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0),
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|error| LightingError::Hid(error.to_string()))?;

    let mut preparsed: PHIDP_PREPARSED_DATA = PHIDP_PREPARSED_DATA::default();
    // SAFETY: handle is the valid HID device handle opened above; preparsed is only used after
    // HidD_GetPreparsedData returns TRUE and is freed exactly once, HidP_GetCaps writes only to
    // the local HIDP_CAPS out-param, and handle is closed on both the error and success paths.
    let result = unsafe {
        if !HidD_GetPreparsedData(handle, &mut preparsed) {
            let _ = CloseHandle(handle);
            return Err(LightingError::Hid(
                "HidD_GetPreparsedData failed".to_string(),
            ));
        }

        let mut caps = HIDP_CAPS::default();
        let status = HidP_GetCaps(preparsed, &mut caps);
        HidD_FreePreparsedData(preparsed);
        let _ = CloseHandle(handle);

        if status == HIDP_STATUS_SUCCESS {
            Ok(caps)
        } else {
            Err(LightingError::Hid(format!(
                "HidP_GetCaps failed with NTSTATUS 0x{:08x}",
                status.0
            )))
        }
    };

    result
}

pub(crate) fn detect_lighting_device() -> Option<LightingDevice> {
    let api = hidapi::HidApi::new().ok()?;
    let supported = [
        (0x0951, 0x171f),
        (0x03f0, 0x0f8b),
        (0x03f0, 0x028c),
        (0x03f0, 0x048c),
        (0x03f0, 0x068c),
        (0x03f0, 0x098c),
    ];

    api.device_list()
        .filter(|device| {
            supported.iter().any(|(vendor, product)| {
                device.vendor_id() == *vendor && device.product_id() == *product
            })
        })
        .max_by_key(|device| lighting_device_score(device))
        .map(|device| LightingDevice {
            vendor_id: device.vendor_id(),
            product_id: device.product_id(),
            interface_number: device.interface_number(),
            usage_page: device.usage_page(),
            usage: device.usage(),
            manufacturer: device.manufacturer_string().unwrap_or("HyperX").to_string(),
            product: device.product_string().unwrap_or("QuadCast S").to_string(),
        })
}

pub(crate) fn is_supported_lighting_device(device: &hidapi::DeviceInfo) -> bool {
    matches!(
        (device.vendor_id(), device.product_id()),
        (0x0951, 0x171f)
            | (0x03f0, 0x0f8b)
            | (0x03f0, 0x028c)
            | (0x03f0, 0x048c)
            | (0x03f0, 0x068c)
            | (0x03f0, 0x098c)
    )
}

pub(crate) fn lighting_device_score(device: &hidapi::DeviceInfo) -> (u8, u8, u8, i32) {
    let rgb_collection = u8::from(device.usage_page() == 0xff90 && device.usage() == 0xff00);
    let vendor_defined = u8::from(
        device.usage_page() == 0xff90
            || device.usage_page() == 0xff00
            || device.usage_page() == 0xffff,
    );
    let controller_pid = u8::from(device.product_id() == 0x171f);
    let interface_preference = if device.interface_number() == 0 { 1 } else { 0 };
    (
        rgb_collection,
        vendor_defined,
        controller_pid,
        interface_preference,
    )
}

pub(crate) fn run_lighting_solid(args: &[String]) {
    let (args, packet_log) = split_packet_log_flag(args);
    if args.is_empty() || args.len() > 2 {
        eprintln!("Usage: hyperx-mic-lite lighting-solid ff0066 [seconds] [--packet-log]");
        process::exit(2);
    }

    let color = parse_rgb_hex(args[0]).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(2);
    });

    let seconds = args
        .get(1)
        .map(|value| value.parse::<u64>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("Duration must be a whole number of seconds.");
            process::exit(2);
        })
        .unwrap_or(3)
        .clamp(1, 60);

    let program = LightingProgram {
        effect: Effect::Solid,
        target: LightTarget::All,
        split_layers: false,
        top_effect: Effect::Solid,
        bottom_effect: Effect::Solid,
        colors: vec![color],
        speed: 75,
        brightness: 100,
        shared_peak_bits: None,
    };

    match stream_lighting_program(
        &program,
        StreamDuration::Timed(Duration::from_secs(seconds)),
        packet_log,
    ) {
        Ok(()) => {
            log_event(
                "info",
                "lighting.solid",
                &[
                    (
                        "color",
                        format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]),
                    ),
                    ("seconds", seconds.to_string()),
                ],
            );
            println!(
                "Streamed solid color #{:02x}{:02x}{:02x} for {seconds}s",
                color[0], color[1], color[2],
            )
        }
        Err(error) => {
            log_event(
                "error",
                "lighting.solid.error",
                &[("message", error.to_string())],
            );
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

pub(crate) fn run_lighting_effect(args: &[String]) {
    let (args, packet_log) = split_packet_log_flag(args);
    if args.is_empty() || args.len() > 2 {
        eprintln!(
            "Usage: hyperx-mic-lite lighting-effect <solid|wave|cycle|pulse|blink|lightning|vu_meter> [seconds|forever] [--packet-log]"
        );
        process::exit(2);
    }

    let effect = Effect::from_config(args[0]);
    let stream_duration = args
        .get(1)
        .map(|value| parse_stream_duration(value))
        .transpose()
        .unwrap_or_else(|error| {
            eprintln!("{error}");
            process::exit(2);
        })
        .unwrap_or(StreamDuration::Forever);
    let config = load_or_create_config().unwrap_or_else(|_| AppConfig::default());
    let colors = config
        .lighting
        .colors
        .iter()
        .filter_map(|color| parse_rgb_hex(color).ok())
        .collect::<Vec<_>>();
    let program = LightingProgram {
        effect,
        target: LightTarget::from_config(&config.lighting.target),
        split_layers: config.lighting.split_layers,
        top_effect: Effect::from_config(&config.lighting.top_effect),
        bottom_effect: Effect::from_config(&config.lighting.bottom_effect),
        colors: if colors.is_empty() {
            vec![[0, 255, 0]]
        } else {
            colors
        },
        speed: config.lighting.speed,
        brightness: config.lighting.brightness,
        shared_peak_bits: None,
    };

    match stream_lighting_program(&program, stream_duration, packet_log) {
        Ok(()) => println!("Streamed {}", effect.label()),
        Err(error) => {
            log_event(
                "error",
                "lighting.effect.error",
                &[("message", error.to_string())],
            );
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

pub(crate) fn run_lighting_vu_test(args: &[String]) {
    let (args, packet_log) = split_packet_log_flag(args);
    if args.is_empty() || args.len() > 2 {
        eprintln!("Usage: hyperx-mic-lite lighting-vu-test <0-100> [seconds] [--packet-log]");
        process::exit(2);
    }

    let level = args[0].parse::<u8>().unwrap_or_else(|_| {
        eprintln!("Level must be a whole number from 0 to 100.");
        process::exit(2);
    });
    if level > 100 {
        eprintln!("Level must be a whole number from 0 to 100.");
        process::exit(2);
    }
    let seconds = args
        .get(1)
        .map(|value| value.parse::<u64>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("Duration must be a whole number of seconds.");
            process::exit(2);
        })
        .unwrap_or(2)
        .clamp(1, 30);

    let config = load_or_create_config().unwrap_or_else(|_| AppConfig::default());
    let frame = build_vu_frame(level as f32 / 100.0, config.lighting.brightness, 0);
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        if let Err(error) = write_lighting_frame_once(frame, packet_log) {
            eprintln!("{error}");
            process::exit(1);
        }
        thread::sleep(Duration::from_millis(80));
    }
    println!("Streamed VU test level {level}% for {seconds}s");
}

pub(crate) fn run_lighting_save(args: &[String]) {
    let (args, packet_log) = split_packet_log_flag(args);
    if !args.is_empty() {
        eprintln!("Usage: hyperx-mic-lite lighting-save [--packet-log]");
        process::exit(2);
    }

    match save_lighting_to_microphone(packet_log) {
        Ok(()) => {
            log_event("info", "lighting.save.done", &[]);
            println!("Sent experimental Save to Microphone command sequence.");
        }
        Err(error) => {
            log_event(
                "error",
                "lighting.save.error",
                &[("message", error.to_string())],
            );
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

fn split_packet_log_flag(args: &[String]) -> (Vec<&str>, bool) {
    let mut packet_log = false;
    let mut filtered = Vec::new();
    for arg in args {
        if arg == "--packet-log" || arg == "--verbose-hid" {
            packet_log = true;
        } else {
            filtered.push(arg.as_str());
        }
    }
    (filtered, packet_log)
}

fn parse_stream_duration(value: &str) -> Result<StreamDuration, String> {
    if value.eq_ignore_ascii_case("forever") {
        return Ok(StreamDuration::Forever);
    }
    let seconds = value
        .parse::<u64>()
        .map_err(|_| "Duration must be seconds or 'forever'.".to_string())?
        .clamp(1, 120);
    Ok(StreamDuration::Timed(Duration::from_secs(seconds)))
}

pub(crate) fn run_hid_monitor(args: &[String]) {
    if args.len() > 1 {
        eprintln!("Usage: hyperx-mic-lite hid-monitor [seconds]");
        process::exit(2);
    }

    let seconds = args
        .first()
        .map(|value| value.parse::<u64>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("Duration must be a whole number of seconds.");
            process::exit(2);
        })
        .unwrap_or(15)
        .clamp(1, 120);

    if let Err(error) = monitor_hid_reports(Duration::from_secs(seconds)) {
        eprintln!("{error}");
        process::exit(1);
    }
}

pub(crate) fn run_level_monitor(args: &[String]) {
    if args.len() > 1 {
        eprintln!("Usage: hyperx-mic-lite level-monitor [seconds]");
        process::exit(2);
    }

    let seconds = args
        .first()
        .map(|value| value.parse::<u64>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("Duration must be a whole number of seconds.");
            process::exit(2);
        })
        .unwrap_or(15)
        .clamp(1, 120);

    let capture_monitor = match start_audio_peak_monitor() {
        Ok(monitor) => {
            println!("Using direct input capture peak monitor.");
            Some(monitor)
        }
        Err(error) => {
            println!("Direct input capture unavailable: {error}");
            println!("Falling back to Windows endpoint meter.");
            None
        }
    };

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        if let Some(monitor) = &capture_monitor {
            println!(
                "{:>6}ms input-peak={:.3}",
                started.elapsed().as_millis(),
                monitor.peak()
            );
        } else {
            match input_peak_value() {
                Ok(peak) => println!(
                    "{:>6}ms endpoint-peak={:.3}",
                    started.elapsed().as_millis(),
                    peak
                ),
                Err(error) => println!(
                    "{:>6}ms endpoint-peak-error={error}",
                    started.elapsed().as_millis()
                ),
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn monitor_hid_reports(duration: Duration) -> Result<(), String> {
    let api = hidapi::HidApi::new().map_err(|error| error.to_string())?;
    let mut devices = Vec::new();

    for info in api
        .device_list()
        .filter(|device| is_supported_lighting_device(device))
    {
        if let Ok(device) = api.open_path(info.path()) {
            device
                .set_blocking_mode(false)
                .map_err(|error| error.to_string())?;
            devices.push((
                format!(
                    "{:04x}:{:04x} if{} {:04x}:{:04x}",
                    info.vendor_id(),
                    info.product_id(),
                    info.interface_number(),
                    info.usage_page(),
                    info.usage()
                ),
                device,
                hid_caps_for_path(info.path()).ok(),
            ));
        }
    }

    if devices.is_empty() {
        return Err("No supported QuadCast S HID interfaces could be opened.".to_string());
    }

    println!(
        "Monitoring {} HID interface(s) for {}s.",
        devices.len(),
        duration.as_secs()
    );
    println!(
        "Press one physical control at a time: mute, pattern positions, then turn the gain dial."
    );

    let started = std::time::Instant::now();
    let mut last_volume = mic_status().ok().map(|status| status.volume);
    if let Some(volume) = last_volume {
        println!("{:>6}ms core-audio mic-volume={volume}", 0);
    }

    let mut buffers = devices
        .iter()
        .map(|(_, _, caps)| {
            let len = caps
                .as_ref()
                .map(|caps| caps.InputReportByteLength as usize)
                .unwrap_or(65)
                .max(65);
            vec![0u8; len]
        })
        .collect::<Vec<_>>();

    while started.elapsed() < duration {
        for (index, (label, device, _)) in devices.iter().enumerate() {
            match device.read_timeout(&mut buffers[index], 10) {
                Ok(0) => {}
                Ok(count) => {
                    let elapsed = started.elapsed().as_millis();
                    let decoded = decode_hid_report(&buffers[index][..count])
                        .map(|value| format!(" {value}"))
                        .unwrap_or_default();
                    println!(
                        "{elapsed:>6}ms {label} {}{}",
                        hex_bytes(&buffers[index][..count]),
                        decoded
                    );
                }
                Err(error) => {
                    let elapsed = started.elapsed().as_millis();
                    println!("{elapsed:>6}ms {label} read-error: {error}");
                }
            }
        }

        if let Ok(status) = mic_status() {
            if last_volume != Some(status.volume) {
                let elapsed = started.elapsed().as_millis();
                println!("{elapsed:>6}ms core-audio mic-volume={}", status.volume);
                last_volume = Some(status.volume);
            }
        }

        thread::sleep(Duration::from_millis(10));
    }

    Ok(())
}

pub(crate) fn spawn_hid_event_listener() -> Receiver<HidEvent> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let api = match hidapi::HidApi::new() {
            Ok(api) => api,
            Err(error) => {
                log_event(
                    "error",
                    "hid.listener.init.error",
                    &[("message", error.to_string())],
                );
                return;
            }
        };

        let mut devices = Vec::new();
        for info in api
            .device_list()
            .filter(|device| is_supported_lighting_device(device))
        {
            match api.open_path(info.path()) {
                Ok(device) => {
                    if let Err(error) = device.set_blocking_mode(false) {
                        log_event(
                            "warn",
                            "hid.listener.nonblocking.error",
                            &[
                                ("interface", info.interface_number().to_string()),
                                ("usage_page", format!("{:04x}", info.usage_page())),
                                ("usage", format!("{:04x}", info.usage())),
                                ("message", error.to_string()),
                            ],
                        );
                    }
                    log_event(
                        "info",
                        "hid.listener.open",
                        &[
                            ("interface", info.interface_number().to_string()),
                            ("usage_page", format!("{:04x}", info.usage_page())),
                            ("usage", format!("{:04x}", info.usage())),
                        ],
                    );
                    devices.push(device);
                }
                Err(error) => {
                    log_event(
                        "warn",
                        "hid.listener.open.error",
                        &[
                            ("interface", info.interface_number().to_string()),
                            ("usage_page", format!("{:04x}", info.usage_page())),
                            ("usage", format!("{:04x}", info.usage())),
                            ("message", error.to_string()),
                        ],
                    );
                }
            }
        }

        if devices.is_empty() {
            log_event("warn", "hid.listener.no_devices", &[]);
            return;
        }

        let mut buffers = vec![[0u8; 65]; devices.len()];
        loop {
            for (index, device) in devices.iter().enumerate() {
                match device.read_timeout(&mut buffers[index], 10) {
                    Ok(0) => {}
                    Ok(count) => match decode_hid_event(&buffers[index][..count]) {
                        Some(HidEvent::Mute(is_live)) => {
                            log_event(
                                "info",
                                "hid.listener.event.mute",
                                &[("live", is_live.to_string())],
                            );
                            if sender.send(HidEvent::Mute(is_live)).is_err() {
                                log_event("info", "hid.listener.stop", &[]);
                                return;
                            }
                        }
                        Some(HidEvent::Pattern(pattern)) => {
                            log_event(
                                "info",
                                "hid.listener.event.pattern",
                                &[("pattern", pattern.label().to_string())],
                            );
                            if sender.send(HidEvent::Pattern(pattern)).is_err() {
                                log_event("info", "hid.listener.stop", &[]);
                                return;
                            }
                        }
                        None => {}
                    },
                    Err(error) => {
                        log_event(
                            "warn",
                            "hid.listener.read.error",
                            &[("message", error.to_string())],
                        );
                    }
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
    });
    receiver
}

fn decode_hid_event(report: &[u8]) -> Option<HidEvent> {
    match hid_event_payload(report)? {
        [0x05, 0x10, value, ..] => Some(HidEvent::Mute(*value == 1)),
        [0x05, 0x11, value, ..] => Some(HidEvent::Pattern(PolarPattern::from_report(*value))),
        _ => None,
    }
}

fn hid_event_payload(report: &[u8]) -> Option<&[u8]> {
    if matches!(report, [0x05, 0x10 | 0x11, ..]) {
        Some(report)
    } else if matches!(report, [_, 0x05, 0x10 | 0x11, ..]) {
        Some(&report[1..])
    } else {
        None
    }
}

fn decode_hid_report(report: &[u8]) -> Option<String> {
    match decode_hid_event(report)? {
        HidEvent::Mute(is_live) => Some(format!(
            "mute={}",
            if is_live { "live/unmuted" } else { "muted" }
        )),
        HidEvent::Pattern(pattern) => Some(format!("pattern={}", pattern.label())),
    }
}

pub(crate) fn pattern_description(pattern: PolarPattern) -> &'static str {
    match pattern {
        PolarPattern::Stereo => "Captures left and right channels for wider sources.",
        PolarPattern::Omni => "Captures sound evenly from around the microphone.",
        PolarPattern::Cardioid => "Best for podcasts, streaming, voiceovers and instruments.",
        PolarPattern::Bidirectional => "Captures from the front and back for interviews.",
        PolarPattern::Unknown(_) => "Unrecognized hardware pattern report.",
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn parse_rgb_hex(value: &str) -> Result<[u8; 3], LightingError> {
    let trimmed = value.trim().trim_start_matches('#');
    if trimmed.len() != 6 {
        return Err(LightingError::Invalid(
            "Color must be six hex digits, for example ff0066.".to_string(),
        ));
    }

    let parsed = u32::from_str_radix(trimmed, 16)
        .map_err(|_| LightingError::Invalid("Color must contain only hex digits.".to_string()))?;
    Ok([
        ((parsed >> 16) & 0xff) as u8,
        ((parsed >> 8) & 0xff) as u8,
        (parsed & 0xff) as u8,
    ])
}

pub(crate) fn color_to_hex(color: egui::Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

pub(crate) fn stream_lighting_program(
    program: &LightingProgram,
    duration: StreamDuration,
    packet_log: bool,
) -> Result<(), LightingError> {
    stream_lighting_program_cancelable(program, duration, None, packet_log)
}

pub(crate) fn stream_lighting_program_cancelable(
    program: &LightingProgram,
    duration: StreamDuration,
    cancel: Option<Arc<AtomicBool>>,
    packet_log: bool,
) -> Result<(), LightingError> {
    let active_effect = if program.split_layers {
        None
    } else {
        Some(program.effect)
    };
    let _com = if active_effect == Some(Effect::VuMeter) {
        match ComApartment::init() {
            Ok(com) => Some(com),
            Err(error) => {
                log_event(
                    "warn",
                    "lighting.vu.com_init.error",
                    &[("message", error.to_string())],
                );
                None
            }
        }
    } else {
        None
    };
    let api = hidapi::HidApi::new().map_err(|error| LightingError::Hid(error.to_string()))?;
    let info = api
        .device_list()
        .filter(|device| is_supported_lighting_device(device))
        .max_by_key(|device| lighting_device_score(device))
        .ok_or(LightingError::NoDevice)?;

    let device = api
        .open_path(info.path())
        .map_err(|error| LightingError::Hid(error.to_string()))?;

    let header = build_display_header_packet();
    let (frames, top_frames, bottom_frames) = build_program_frames(program)?;
    let started = std::time::Instant::now();
    let mut index = 0usize;
    let frame_delay = effect_frame_delay(program.speed);
    let mut vu_meter = VuMeterState::new();
    let capture_monitor = start_vu_capture_monitor(program, active_effect);

    while match duration {
        StreamDuration::Timed(duration) => started.elapsed() < duration,
        StreamDuration::Forever => true,
    } {
        if cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        {
            break;
        }
        let frame = if active_effect == Some(Effect::VuMeter) {
            vu_meter.next_frame(program, capture_monitor.as_ref())
        } else if program.split_layers {
            let top = top_frames[index % top_frames.len()];
            let bottom = bottom_frames[index % bottom_frames.len()];
            index += 1;
            combine_zone_frames(top, bottom)
        } else {
            let frame = frames[index % frames.len()];
            index += 1;
            frame
        };
        let frame = if program.split_layers {
            frame
        } else {
            apply_light_target(frame, program.target)
        };
        send_feature_packet(&device, &header, packet_log)?;
        send_feature_packet(&device, &build_frame_packet(frame), packet_log)?;
        thread::sleep(frame_delay);
    }

    Ok(())
}

type ProgramFrames = (Vec<LightingFrame>, Vec<LightingFrame>, Vec<LightingFrame>);

fn build_program_frames(program: &LightingProgram) -> Result<ProgramFrames, LightingError> {
    let frames = if program.split_layers {
        Vec::new()
    } else {
        build_effect_frames(program.effect, program)
    };
    if !program.split_layers && frames.is_empty() {
        return Err(LightingError::Invalid(
            "No lighting frames were generated.".to_string(),
        ));
    }
    let top_frames = if program.split_layers {
        build_effect_frames(program.top_effect, program)
    } else {
        Vec::new()
    };
    let bottom_frames = if program.split_layers {
        build_effect_frames(program.bottom_effect, program)
    } else {
        Vec::new()
    };
    if program.split_layers && (top_frames.is_empty() || bottom_frames.is_empty()) {
        return Err(LightingError::Invalid(
            "No layered lighting frames were generated.".to_string(),
        ));
    }
    Ok((frames, top_frames, bottom_frames))
}

fn start_vu_capture_monitor(
    program: &LightingProgram,
    active_effect: Option<Effect>,
) -> Option<AudioPeakMonitor> {
    if active_effect != Some(Effect::VuMeter) {
        return None;
    }
    if program.shared_peak_bits.is_some() {
        log_event("info", "lighting.vu.capture.shared", &[]);
        return None;
    }
    match start_audio_peak_monitor() {
        Ok(monitor) => {
            log_event("info", "lighting.vu.capture.start", &[]);
            Some(monitor)
        }
        Err(error) => {
            log_event("warn", "lighting.vu.capture.error", &[("message", error)]);
            None
        }
    }
}

struct VuMeterState {
    level: f32,
    tick: u32,
    meter_error_logged: bool,
}

impl VuMeterState {
    fn new() -> Self {
        Self {
            level: 0.18,
            tick: 0,
            meter_error_logged: false,
        }
    }

    fn next_frame(
        &mut self,
        program: &LightingProgram,
        capture_monitor: Option<&AudioPeakMonitor>,
    ) -> LightingFrame {
        let direct_peak = if let Some(peak_bits) = &program.shared_peak_bits {
            peak_from_bits(peak_bits)
        } else if let Some(monitor) = capture_monitor {
            monitor.peak()
        } else {
            0.0
        };
        // The per-frame endpoint meter query is expensive (fresh COM objects
        // each call); only fall back to it when no direct capture is available.
        let endpoint_peak = if program.shared_peak_bits.is_none() && capture_monitor.is_none() {
            match input_peak_value() {
                Ok(peak) => peak,
                Err(error) => {
                    if !self.meter_error_logged {
                        log_event(
                            "warn",
                            "lighting.vu.meter.error",
                            &[("message", error.to_string())],
                        );
                        self.meter_error_logged = true;
                    }
                    0.0
                }
            }
        } else {
            0.0
        };
        let target = vu_target_level(direct_peak.max(endpoint_peak));
        self.level = smooth_vu_level(self.level, target);
        self.tick = self.tick.wrapping_add(1);
        if self.tick % 50 == 0 {
            log_event(
                "info",
                "lighting.vu.level",
                &[
                    ("direct", format!("{direct_peak:.4}")),
                    ("endpoint", format!("{endpoint_peak:.4}")),
                    ("target", format!("{target:.3}")),
                    ("level", format!("{:.3}", self.level)),
                ],
            );
        }
        build_vu_frame(self.level, program.brightness, self.tick)
    }
}

fn build_display_header_packet() -> [u8; 64] {
    let mut packet = [0u8; 64];
    packet[0] = 0x04;
    packet[1] = 0xf2;
    packet[8] = 0x01;
    packet
}

fn build_frame_packet(frame: LightingFrame) -> [u8; 64] {
    let mut packet = [0u8; 64];
    for (index, color) in frame.iter().enumerate() {
        let offset = index * 4;
        packet[offset] = 0x81;
        packet[offset + 1] = color[0];
        packet[offset + 2] = color[1];
        packet[offset + 3] = color[2];
    }
    packet
}

fn apply_light_target(mut frame: LightingFrame, target: LightTarget) -> LightingFrame {
    match target {
        LightTarget::All => frame,
        LightTarget::Top => {
            frame[1] = [0, 0, 0];
            for color in frame.iter_mut().skip(2) {
                *color = [0, 0, 0];
            }
            frame
        }
        LightTarget::Bottom => {
            frame[0] = [0, 0, 0];
            for color in frame.iter_mut().skip(2) {
                *color = [0, 0, 0];
            }
            frame
        }
    }
}

fn combine_zone_frames(top: LightingFrame, bottom: LightingFrame) -> LightingFrame {
    let mut frame = [[0, 0, 0]; LIGHTING_CELL_COUNT];
    frame[0] = top[0];
    frame[1] = bottom[0];
    frame
}

fn build_save_prepare_packet() -> [u8; 64] {
    let mut packet = [0u8; 64];
    packet[0] = 0x04;
    packet[1] = 0x53;
    packet[8] = 0x01;
    packet
}

fn build_save_state_packet() -> [u8; 64] {
    let mut packet = [0u8; 64];
    packet[0] = 0x04;
    packet[1] = 0x02;
    packet
}

fn build_save_commit_packet() -> [u8; 64] {
    let mut packet = [0u8; 64];
    packet[0] = 0x04;
    packet[1] = 0x23;
    packet[8] = 0x01;
    packet
}

fn build_save_sentinel_packet() -> [u8; 64] {
    let mut packet = [0u8; 64];
    packet[0] = 0x08;
    packet[59] = 0x28;
    packet[60] = 0x01;
    packet[61] = 0x00;
    packet[62] = 0xaa;
    packet[63] = 0x55;
    packet
}

fn build_effect_frames(effect: Effect, program: &LightingProgram) -> Vec<LightingFrame> {
    let colors = normalized_colors(program);
    match effect {
        Effect::Solid => vec![solid_frame(colors[0])],
        Effect::Cycle => cycle_frames(&colors, program.speed, false),
        Effect::Wave => cycle_frames(&colors, program.speed, true),
        Effect::Pulse => pulse_frames(&colors, program.speed),
        Effect::Blink => blink_frames(&colors, program.speed),
        Effect::Lightning => lightning_frames(&colors, program.speed),
        Effect::VuMeter => vec![solid_frame([0, 0, 0])],
    }
}

fn normalized_colors(program: &LightingProgram) -> Vec<[u8; 3]> {
    let colors = if program.colors.is_empty() {
        vec![[0, 255, 0]]
    } else {
        program.colors.clone()
    };
    colors
        .into_iter()
        .map(|color| scale_color(color, program.brightness))
        .collect()
}

fn effect_frame_delay(speed: u8) -> Duration {
    let millis = 140u64.saturating_sub(speed as u64);
    Duration::from_millis(millis.clamp(24, 140))
}

fn transition_steps(speed: u8, min: usize, max: usize) -> usize {
    let span = max.saturating_sub(min);
    max.saturating_sub((span * speed as usize) / 100).max(min)
}

fn solid_frame(color: [u8; 3]) -> LightingFrame {
    [color; LIGHTING_CELL_COUNT]
}

fn cycle_frames(colors: &[[u8; 3]], speed: u8, wave: bool) -> Vec<LightingFrame> {
    let steps = transition_steps(speed, 8, 48);
    let mut sequence = Vec::new();
    for index in 0..colors.len() {
        let start = colors[index];
        let end = colors[(index + 1) % colors.len()];
        for step in 0..steps {
            sequence.push(lerp_color(start, end, step, steps));
        }
    }

    if sequence.is_empty() {
        return vec![solid_frame([0, 0, 0])];
    }

    let wave_span = (sequence.len() / 2).max(1);
    sequence
        .iter()
        .enumerate()
        .map(|(index, color)| {
            let mut frame = solid_frame(*color);
            if wave {
                for (cell, slot) in frame.iter_mut().enumerate() {
                    let offset = (cell * wave_span) / LIGHTING_CELL_COUNT;
                    *slot = sequence[(index + offset) % sequence.len()];
                }
            }
            frame
        })
        .collect()
}

fn pulse_frames(colors: &[[u8; 3]], speed: u8) -> Vec<LightingFrame> {
    let steps = transition_steps(speed, 6, 36);
    let mut frames = Vec::new();
    for &color in colors {
        for step in 0..steps {
            let value = lerp_color([0, 0, 0], color, step, steps);
            frames.push(solid_frame(value));
        }
        for step in 0..steps {
            let value = lerp_color(color, [0, 0, 0], step, steps);
            frames.push(solid_frame(value));
        }
    }
    frames
}

fn blink_frames(colors: &[[u8; 3]], speed: u8) -> Vec<LightingFrame> {
    let lit = transition_steps(speed, 2, 16);
    let dark = transition_steps(speed, 2, 24);
    let mut frames = Vec::new();
    for &color in colors {
        for _ in 0..lit {
            frames.push(solid_frame(color));
        }
        for _ in 0..dark {
            frames.push(solid_frame([0, 0, 0]));
        }
    }
    frames
}

fn lightning_frames(colors: &[[u8; 3]], speed: u8) -> Vec<LightingFrame> {
    let flash = transition_steps(speed, 1, 6);
    let fade = transition_steps(speed, 8, 42);
    let mut frames = Vec::new();
    for &color in colors {
        for _ in 0..flash {
            let mut frame = solid_frame([0, 0, 0]);
            for cell in (0..LIGHTING_CELL_COUNT).step_by(3) {
                frame[cell] = color;
            }
            frames.push(frame);
        }
        for step in 0..fade {
            let value = lerp_color(color, [0, 0, 0], step, fade);
            frames.push(solid_frame(value));
        }
        frames.push(solid_frame([0, 0, 0]));
    }
    frames
}

pub(crate) fn smooth_vu_level(current: f32, target: f32) -> f32 {
    let coefficient = if target > current { 0.42 } else { 0.14 };
    current + (target - current) * coefficient
}

pub(crate) fn vu_target_level(raw_peak: f32) -> f32 {
    let normalized = ((raw_peak - 0.00002).max(0.0) * 1200.0).clamp(0.0, 1.0);
    normalized.powf(0.48)
}

pub(crate) fn build_vu_frame(level: f32, brightness: u8, tick: u32) -> LightingFrame {
    let level = level.clamp(0.0, 1.0);
    let visible_level = level.powf(0.55).max(0.04);
    let lit_cells = ((visible_level * LIGHTING_CELL_COUNT as f32).ceil() as usize)
        .clamp(1, LIGHTING_CELL_COUNT);
    let mut frame = solid_frame(scale_color([10, 0, 0], brightness.max(70)));

    for cell in 0..LIGHTING_CELL_COUNT {
        let active = cell < lit_cells;
        let physical_cell = LIGHTING_CELL_COUNT - 1 - cell;
        if active {
            let position = if lit_cells <= 1 {
                0.0
            } else {
                cell as f32 / (lit_cells - 1) as f32
            };
            let shimmer = ((flame_wave(cell, tick) + 1.0) * 0.5).clamp(0.0, 1.0);
            frame[physical_cell] = vu_flame_color(position, shimmer, brightness.max(95));
        } else {
            let distance = cell.saturating_sub(lit_cells) as f32;
            if distance < 2.0 {
                let afterglow = (2.0 - distance) / 2.0;
                frame[physical_cell] = scale_color([80, 4, 0], (afterglow * 35.0) as u8);
            }
        }
    }
    frame
}

fn flame_wave(cell: usize, tick: u32) -> f32 {
    let phase_a = tick as f32 * 0.18 + cell as f32 * 0.72;
    let phase_b = tick as f32 * 0.11 + cell as f32 * 1.37;
    (phase_a.sin() * 0.65 + phase_b.sin() * 0.35).clamp(-1.0, 1.0)
}

fn vu_flame_color(position: f32, shimmer: f32, brightness: u8) -> [u8; 3] {
    let base = if position < 0.45 {
        lerp_color_float([255, 245, 120], [255, 170, 0], position / 0.45)
    } else if position < 0.78 {
        lerp_color_float([255, 170, 0], [255, 38, 0], (position - 0.45) / 0.33)
    } else {
        lerp_color_float([255, 38, 0], [160, 0, 0], (position - 0.78) / 0.22)
    };
    let effective = ((0.78 + shimmer * 0.22) * brightness as f32).round() as u8;
    scale_color(base, effective)
}

fn lerp_color(start: [u8; 3], end: [u8; 3], step: usize, steps: usize) -> [u8; 3] {
    let denominator = steps.saturating_sub(1).max(1) as f32;
    let t = step as f32 / denominator;
    [
        (start[0] as f32 + (end[0] as f32 - start[0] as f32) * t).round() as u8,
        (start[1] as f32 + (end[1] as f32 - start[1] as f32) * t).round() as u8,
        (start[2] as f32 + (end[2] as f32 - start[2] as f32) * t).round() as u8,
    ]
}

fn lerp_color_float(start: [u8; 3], end: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (start[0] as f32 + (end[0] as f32 - start[0] as f32) * t).round() as u8,
        (start[1] as f32 + (end[1] as f32 - start[1] as f32) * t).round() as u8,
        (start[2] as f32 + (end[2] as f32 - start[2] as f32) * t).round() as u8,
    ]
}

fn scale_color(color: [u8; 3], percent: u8) -> [u8; 3] {
    [
        ((color[0] as u16 * percent as u16) / 100) as u8,
        ((color[1] as u16 * percent as u16) / 100) as u8,
        ((color[2] as u16 * percent as u16) / 100) as u8,
    ]
}

fn send_feature_packet(
    device: &hidapi::HidDevice,
    packet: &[u8; 64],
    packet_log: bool,
) -> Result<(), LightingError> {
    let mut with_report_id = [0u8; 65];
    with_report_id[1..].copy_from_slice(packet);

    let send_feature_id = || device.send_feature_report(&with_report_id);
    let send_feature = || device.send_feature_report(packet);
    let send_output_id = || device.write(&with_report_id).map(|_| ());
    let send_output = || device.write(packet).map(|_| ());
    let attempts: [(&str, &dyn Fn() -> hidapi::HidResult<()>); 4] = [
        ("feature+id", &send_feature_id),
        ("feature", &send_feature),
        ("output+id", &send_output_id),
        ("output", &send_output),
    ];

    let mut errors = Vec::new();
    for (name, attempt) in attempts {
        match attempt() {
            Ok(()) => {
                log_hid_packet_attempt(packet_log, name, true, packet, None);
                return Ok(());
            }
            Err(error) => {
                log_hid_packet_attempt(packet_log, name, false, packet, Some(error.to_string()));
                errors.push(format!("{name}: {error}"));
            }
        }
    }

    Err(LightingError::Hid(errors.join("; ")))
}

fn read_feature_packet(device: &hidapi::HidDevice, packet_log: bool) -> Result<[u8; 64], String> {
    let mut with_report_id = [0u8; 65];
    let mut errors = Vec::new();
    match device.get_feature_report(&mut with_report_id) {
        Ok(length) if length >= 65 => {
            let mut packet = [0u8; 64];
            packet.copy_from_slice(&with_report_id[1..65]);
            log_hid_packet_attempt(packet_log, "feature-read+id", true, &packet, None);
            return Ok(packet);
        }
        Ok(length) => errors.push(format!("feature-read+id: short report length {length}")),
        Err(error) => errors.push(format!("feature-read+id: {error}")),
    }

    let mut packet = [0u8; 64];
    match device.get_feature_report(&mut packet) {
        Ok(length) if length >= 64 => {
            log_hid_packet_attempt(packet_log, "feature-read", true, &packet, None);
            Ok(packet)
        }
        Ok(length) => Err(format!(
            "Unable to read feature report. Short report length {length}. Previous errors: {}",
            errors.join("; ")
        )),
        Err(error) => {
            errors.push(format!("feature-read: {error}"));
            Err(format!(
                "Unable to read feature report: {}",
                errors.join("; ")
            ))
        }
    }
}

pub(crate) fn save_lighting_to_microphone(packet_log: bool) -> Result<(), LightingError> {
    let api = hidapi::HidApi::new().map_err(|error| LightingError::Hid(error.to_string()))?;
    let info = api
        .device_list()
        .filter(|device| is_supported_lighting_device(device))
        .max_by_key(|device| lighting_device_score(device))
        .ok_or(LightingError::NoDevice)?;

    let device = api
        .open_path(info.path())
        .map_err(|error| LightingError::Hid(error.to_string()))?;

    send_feature_packet(&device, &build_save_prepare_packet(), packet_log)?;
    send_feature_packet(&device, &build_save_state_packet(), packet_log)?;
    if let Err(error) = read_feature_packet(&device, packet_log) {
        log_event(
            "warn",
            "lighting.save.readback.error",
            &[("message", error)],
        );
    }
    send_feature_packet(&device, &build_save_commit_packet(), packet_log)?;
    if let Err(error) = read_feature_packet(&device, packet_log) {
        log_event(
            "warn",
            "lighting.save.readback.error",
            &[("message", error)],
        );
    }
    send_feature_packet(&device, &build_save_sentinel_packet(), packet_log)?;
    send_feature_packet(&device, &build_save_state_packet(), packet_log)?;
    if let Err(error) = read_feature_packet(&device, packet_log) {
        log_event(
            "warn",
            "lighting.save.readback.error",
            &[("message", error)],
        );
    }
    Ok(())
}

pub(crate) fn write_solid_lighting_once(
    color: [u8; 3],
    brightness: u8,
    packet_log: bool,
) -> Result<(), LightingError> {
    let color = scale_color(color, brightness);
    write_lighting_frame_once(solid_frame(color), packet_log)
}

pub(crate) fn write_lighting_frame_once(
    frame: LightingFrame,
    packet_log: bool,
) -> Result<(), LightingError> {
    let api = hidapi::HidApi::new().map_err(|error| LightingError::Hid(error.to_string()))?;
    let info = api
        .device_list()
        .filter(|device| is_supported_lighting_device(device))
        .max_by_key(|device| lighting_device_score(device))
        .ok_or(LightingError::NoDevice)?;
    let device = api
        .open_path(info.path())
        .map_err(|error| LightingError::Hid(error.to_string()))?;
    send_feature_packet(&device, &build_display_header_packet(), packet_log)?;
    send_feature_packet(&device, &build_frame_packet(frame), packet_log)
}

pub(crate) fn live_mute_lighting_color(is_live: bool) -> [u8; 3] {
    if is_live { [0, 255, 70] } else { [255, 0, 28] }
}

fn log_hid_packet_attempt(
    enabled: bool,
    transport: &str,
    success: bool,
    packet: &[u8; 64],
    error: Option<String>,
) {
    if !enabled {
        return;
    }

    let mut fields = vec![
        ("transport", transport.to_string()),
        ("success", success.to_string()),
        ("packet", hex_bytes(packet)),
    ];
    if let Some(error) = error {
        fields.push(("error", error));
    }
    log_event("info", "hid.packet.write", &fields);
}
