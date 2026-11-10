//! QuadCast S audio controls, independent of the platform audio API.
//!
//! The command-line surface, the capture-based peak meter and the JSON output live here;
//! everything that talks to the OS mixer is in the platform backend.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    process,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

#[cfg(target_os = "macos")]
mod core_audio;
#[cfg(windows)]
mod wasapi;

#[cfg(target_os = "macos")]
use core_audio as backend;
#[cfg(windows)]
use wasapi as backend;

pub(crate) use backend::{
    AudioResult, hyperx_endpoint_info, input_peak_value, list_capture_devices, mic_status,
};

use crate::{
    logging::{json_string, log_event},
    model::{DeviceInfo, MicStatus},
};

pub(crate) struct AudioPeakMonitor {
    peak_bits: Arc<AtomicU32>,
    /// Raised by the cpal error callback. A dead stream keeps returning its last sample
    /// forever, so the owner needs a way to tell "silent" from "broken" and rebuild.
    failed: Arc<AtomicBool>,
    _stream: cpal::Stream,
}

impl AudioPeakMonitor {
    pub(crate) fn peak(&self) -> f32 {
        peak_from_bits(&self.peak_bits)
    }

    /// Whether the capture stream has reported an unrecoverable error.
    pub(crate) fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

pub(crate) fn peak_from_bits(peak_bits: &AtomicU32) -> f32 {
    f32::from_bits(peak_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0)
}

/// Which of the QuadCast S audio endpoints an operation targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndpointFlow {
    /// The microphone input.
    Capture,
    /// The headphone output on the back of the mic.
    Render,
}

pub(crate) fn set_volume(args: &[String]) -> AudioResult<()> {
    if args.len() != 1 {
        eprintln!("Usage: hyperx-mic-lite volume 75");
        process::exit(2);
    }

    let percent = args[0].parse::<u8>().unwrap_or_else(|_| {
        eprintln!("Volume must be a number from 0 to 100.");
        process::exit(2);
    });

    if percent > 100 {
        eprintln!("Volume must be a number from 0 to 100.");
        process::exit(2);
    }

    set_mic_volume_percent(percent)?;
    print_status_json(&mic_status()?);
    Ok(())
}

pub(crate) fn run_audio_command(args: &[String]) -> AudioResult<()> {
    if args.is_empty() {
        audio_usage();
        process::exit(2);
    }

    match args[0].as_str() {
        "volume" => {
            if args.len() != 3 {
                audio_usage();
                process::exit(2);
            }
            let control = AudioClassControl::parse(&args[1]).unwrap_or_else(|| {
                eprintln!("Unknown audio control '{}'.", args[1]);
                audio_usage();
                process::exit(2);
            });
            let percent = parse_percent_arg(&args[2]);
            set_audio_control_volume(control, percent)?;
            println!(
                "{{\"control\":{},\"volume\":{}}}",
                json_string(control.label()),
                percent
            );
            Ok(())
        }
        "mute" => {
            if args.len() != 3 {
                audio_usage();
                process::exit(2);
            }
            let control = AudioClassControl::parse(&args[1]).unwrap_or_else(|| {
                eprintln!("Unknown audio control '{}'.", args[1]);
                audio_usage();
                process::exit(2);
            });
            let muted = parse_on_off_arg(&args[2]);
            set_audio_control_mute(control, muted)?;
            println!(
                "{{\"control\":{},\"muted\":{}}}",
                json_string(control.label()),
                muted
            );
            Ok(())
        }
        "topology" => {
            if args.len() != 2 {
                audio_usage();
                process::exit(2);
            }
            let flow = match args[1].as_str() {
                "capture" | "mic" | "input" => EndpointFlow::Capture,
                "render" | "headphone" | "output" => EndpointFlow::Render,
                _ => {
                    audio_usage();
                    process::exit(2);
                }
            };
            backend::print_audio_topology(flow)?;
            Ok(())
        }
        _ => {
            audio_usage();
            process::exit(2);
        }
    }
}

fn audio_usage() {
    eprintln!(
        "Usage:\n\
  hyperx-mic-lite audio volume <mic|monitoring|headphone> <0-100>\n\
  hyperx-mic-lite audio mute <mic|monitoring|headphone> <on|off>\n\
  hyperx-mic-lite audio topology <capture|render>"
    );
}

fn parse_percent_arg(value: &str) -> u8 {
    let percent = value.parse::<u8>().unwrap_or_else(|_| {
        eprintln!("Percent must be a number from 0 to 100.");
        process::exit(2);
    });
    if percent > 100 {
        eprintln!("Percent must be a number from 0 to 100.");
        process::exit(2);
    }
    percent
}

fn parse_on_off_arg(value: &str) -> bool {
    match value.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "muted" => true,
        "off" | "unmuted" | "live" | "false" | "0" => false,
        _ => {
            eprintln!("Mute value must be on/off or true/false.");
            process::exit(2);
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum AudioClassControl {
    Mic,
    Monitoring,
    Headphone,
}

impl AudioClassControl {
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "mic" | "microphone" | "input" => Some(Self::Mic),
            "monitoring" | "monitor" | "sidetone" => Some(Self::Monitoring),
            "headphone" | "headphones" | "output" => Some(Self::Headphone),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Mic => "mic",
            Self::Monitoring => "monitoring",
            Self::Headphone => "headphone",
        }
    }

    /// The gain range the 0-100 sliders span, in dB, as captured from NGENUITY.
    ///
    /// These are also the exact ranges the QuadCast S feature units report, so a percentage
    /// lands on the same hardware gain whichever platform wrote it.
    fn db_range(self) -> (f32, f32) {
        match self {
            Self::Mic => (-8.0, 7.0),
            Self::Monitoring => (-30.0, 6.0),
            Self::Headphone => (-40.0, -9.0),
        }
    }

    /// The endpoint that owns the control. Monitoring is the mic signal routed to the
    /// headphones, but both platforms expose it on the capture side.
    fn endpoint_flow(self) -> EndpointFlow {
        match self {
            Self::Headphone => EndpointFlow::Render,
            Self::Mic | Self::Monitoring => EndpointFlow::Capture,
        }
    }
}

/// Map a slider percentage linearly onto a control's dB range.
fn percent_to_db((min, max): (f32, f32), percent: u8) -> f32 {
    min + (max - min) * f32::from(percent.min(100)) / 100.0
}

/// Inverse of [`percent_to_db`], clamped so a gain set outside the slider range (for
/// example from the OS sound settings) still reads back as a valid percentage.
#[cfg(any(target_os = "macos", test))]
fn db_to_percent((min, max): (f32, f32), db: f32) -> u8 {
    if max <= min || !db.is_finite() {
        return 0;
    }
    ((db - min) / (max - min) * 100.0).round().clamp(0.0, 100.0) as u8
}

pub(crate) fn set_mic_mute(muted: bool) -> AudioResult<()> {
    let result = backend::write_mic_mute(muted);
    if result.is_ok() {
        log_event("info", "audio.mute.set", &[("muted", muted.to_string())]);
    }
    result
}

pub(crate) fn toggle_mic_mute() -> AudioResult<()> {
    backend::toggle_mic_mute()
}

pub(crate) fn set_mic_volume_percent(percent: u8) -> AudioResult<()> {
    let result = backend::write_mic_volume(percent);
    if result.is_ok() {
        log_event(
            "info",
            "audio.volume.set",
            &[("percent", percent.to_string())],
        );
    }
    result
}

pub(crate) fn set_audio_control_volume(control: AudioClassControl, percent: u8) -> AudioResult<()> {
    match control {
        AudioClassControl::Mic => set_mic_volume_percent(percent),
        AudioClassControl::Monitoring | AudioClassControl::Headphone => {
            backend::write_control_volume(control, percent)
        }
    }?;
    log_event(
        "info",
        "audio.usb_class.volume.set",
        &[
            ("control", control.label().to_string()),
            ("percent", percent.to_string()),
        ],
    );
    Ok(())
}

fn set_audio_control_mute(control: AudioClassControl, muted: bool) -> AudioResult<()> {
    match control {
        AudioClassControl::Mic => set_mic_mute(muted),
        AudioClassControl::Monitoring | AudioClassControl::Headphone => {
            backend::write_control_mute(control, muted)
        }
    }?;
    log_event(
        "info",
        "audio.usb_class.mute.set",
        &[
            ("control", control.label().to_string()),
            ("muted", muted.to_string()),
        ],
    );
    Ok(())
}

/// Whether an endpoint's friendly name identifies the QuadCast S.
///
/// Windows exposes the mic under several vendor spellings depending on the driver
/// package, so match on any of them rather than one exact string.
pub(crate) fn is_quadcast_device_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("hyperx") || name.contains("quadcast") || name.contains("kingston")
}

pub(crate) fn start_audio_peak_monitor() -> Result<AudioPeakMonitor, String> {
    start_audio_peak_monitor_with(Arc::new(AtomicU32::new(0.0f32.to_bits())))
}

/// Locate the QuadCast S among cpal's capture devices.
///
/// Deliberately not `default_input_device()`, for two observed reasons. That handle follows
/// whatever Windows has promoted to default, so the meter read a foreign microphone whenever
/// the QuadCast S was not the default, and cpal tore the stream down with "Default audio
/// device changed" on every replug. It also activates through `ActivateAudioInterfaceAsync`
/// rather than `IMMDevice::Activate`, and reopening it in the moments after a replug failed
/// intermittently with RPC_E_CHANGED_MODE; the exact trigger was never pinned down, but a
/// concrete device does not use that code path at all.
fn quadcast_input_device() -> Result<cpal::Device, String> {
    cpal::default_host()
        .input_devices()
        .map_err(|error| error.to_string())?
        // cpal exposes the endpoint's friendly name through `Display`, the same string the
        // platform enumeration in this module matches on.
        .find(|device| is_quadcast_device_name(&device.to_string()))
        .ok_or_else(|| "HyperX QuadCast S is not available as a capture device.".to_string())
}

/// Build a peak monitor that publishes into an existing `peak_bits` cell.
///
/// The GUI rebuilds its monitor whenever the capture device changes, but a VU Meter
/// lighting stream started earlier holds a clone of the cell. Reusing it keeps that stream
/// fed across a replug instead of freezing it on the last sample of the old device.
pub(crate) fn start_audio_peak_monitor_with(
    peak_bits: Arc<AtomicU32>,
) -> Result<AudioPeakMonitor, String> {
    let device = quadcast_input_device()?;
    let config = device
        .default_input_config()
        .map_err(|error| error.to_string())?;
    let channels = config.channels() as usize;
    let stream_config: cpal::StreamConfig = config.into();
    let sample_rate = stream_config.sample_rate;
    peak_bits.store(0.0f32.to_bits(), Ordering::Relaxed);
    let failed = Arc::new(AtomicBool::new(false));

    let build = |sample_format| {
        let args = (
            stream_config,
            channels,
            sample_rate,
            peak_bits.clone(),
            failed.clone(),
        );
        match sample_format {
            cpal::SampleFormat::F32 => build_peak_stream::<f32>(&device, args),
            cpal::SampleFormat::F64 => build_peak_stream::<f64>(&device, args),
            cpal::SampleFormat::I8 => build_peak_stream::<i8>(&device, args),
            cpal::SampleFormat::I16 => build_peak_stream::<i16>(&device, args),
            cpal::SampleFormat::I32 => build_peak_stream::<i32>(&device, args),
            cpal::SampleFormat::I64 => build_peak_stream::<i64>(&device, args),
            cpal::SampleFormat::U8 => build_peak_stream::<u8>(&device, args),
            cpal::SampleFormat::U16 => build_peak_stream::<u16>(&device, args),
            cpal::SampleFormat::U32 => build_peak_stream::<u32>(&device, args),
            cpal::SampleFormat::U64 => build_peak_stream::<u64>(&device, args),
            other => Err(format!("Unsupported input sample format: {other:?}")),
        }
    };
    let stream = build(config.sample_format())?;
    stream.play().map_err(|error| error.to_string())?;
    Ok(AudioPeakMonitor {
        peak_bits,
        failed,
        _stream: stream,
    })
}

type PeakStreamArgs = (
    cpal::StreamConfig,
    usize,
    cpal::SampleRate,
    Arc<AtomicU32>,
    Arc<AtomicBool>,
);

fn build_peak_stream<T>(
    device: &cpal::Device,
    (config, channels, sample_rate, peak_bits, failed): PeakStreamArgs,
) -> Result<cpal::Stream, String>
where
    T: cpal::Sample + cpal::SizedSample + ToPeakSample + Send + 'static,
{
    device
        .build_input_stream(
            config,
            move |data: &[T], _| update_peak_from_samples(data, channels, sample_rate, &peak_bits),
            move |error| {
                // cpal never revives a stream it has errored, so record the death: the owner
                // polls this to rebuild instead of showing a meter frozen at its last sample.
                failed.store(true, Ordering::Relaxed);
                log_event(
                    "warn",
                    "audio.capture.stream.error",
                    &[("message", error.to_string())],
                );
            },
            None,
        )
        .map_err(|error| error.to_string())
}

/// How fast a captured peak fades from the shared cell: it halves every this long.
///
/// The capture callback runs about every 10 ms, but the GUI meter and the VU Meter stream
/// read the cell only every 50-80 ms. Publishing just the latest buffer made them land
/// between syllables most of the time, so the macOS meter barely moved with speech.
/// Windows keeps the instantaneous value its VU mapping was tuned against.
#[cfg(target_os = "macos")]
const PEAK_HOLD_HALF_LIFE: Option<Duration> = Some(Duration::from_millis(120));
#[cfg(windows)]
const PEAK_HOLD_HALF_LIFE: Option<Duration> = None;

fn update_peak_from_samples<T>(
    data: &[T],
    channels: usize,
    sample_rate: cpal::SampleRate,
    peak_bits: &AtomicU32,
) where
    T: ToPeakSample,
{
    let step = channels.max(1);
    let buffer_peak = data
        .chunks(step)
        .flat_map(|frame| frame.iter())
        .map(|sample| sample.to_peak_sample().abs())
        .fold(0.0f32, f32::max)
        .clamp(0.0, 1.0);
    let decay = peak_hold_decay(data.len() / step, sample_rate, PEAK_HOLD_HALF_LIFE);
    let previous = f32::from_bits(peak_bits.load(Ordering::Relaxed));
    let peak = held_peak(previous, buffer_peak, decay);
    peak_bits.store(peak.to_bits(), Ordering::Relaxed);
}

/// The share of a held peak that survives `frames` sample frames; 0 means no hold.
fn peak_hold_decay(
    frames: usize,
    sample_rate: cpal::SampleRate,
    half_life: Option<Duration>,
) -> f32 {
    match half_life {
        Some(half_life) if !half_life.is_zero() && sample_rate > 0 => {
            let elapsed = frames as f32 / sample_rate as f32;
            0.5f32.powf(elapsed / half_life.as_secs_f32())
        }
        _ => 0.0,
    }
}

/// The peak to publish: this buffer's, or the held one fading out, whichever is louder.
fn held_peak(previous: f32, buffer_peak: f32, decay: f32) -> f32 {
    buffer_peak.max(previous * decay).clamp(0.0, 1.0)
}

trait ToPeakSample {
    fn to_peak_sample(&self) -> f32;
}

impl ToPeakSample for f32 {
    fn to_peak_sample(&self) -> f32 {
        *self
    }
}

impl ToPeakSample for f64 {
    fn to_peak_sample(&self) -> f32 {
        *self as f32
    }
}

impl ToPeakSample for i8 {
    fn to_peak_sample(&self) -> f32 {
        *self as f32 / i8::MAX as f32
    }
}

impl ToPeakSample for i16 {
    fn to_peak_sample(&self) -> f32 {
        *self as f32 / i16::MAX as f32
    }
}

impl ToPeakSample for i32 {
    fn to_peak_sample(&self) -> f32 {
        *self as f32 / i32::MAX as f32
    }
}

impl ToPeakSample for i64 {
    fn to_peak_sample(&self) -> f32 {
        *self as f32 / i64::MAX as f32
    }
}

impl ToPeakSample for u8 {
    fn to_peak_sample(&self) -> f32 {
        (*self as f32 - 128.0) / 128.0
    }
}

impl ToPeakSample for u16 {
    fn to_peak_sample(&self) -> f32 {
        (*self as f32 - 32768.0) / 32768.0
    }
}

impl ToPeakSample for u32 {
    fn to_peak_sample(&self) -> f32 {
        (*self as f32 - 2147483648.0) / 2147483648.0
    }
}

impl ToPeakSample for u64 {
    fn to_peak_sample(&self) -> f32 {
        (*self as f64 - 9223372036854775808.0) as f32 / 9223372036854775808.0_f32
    }
}

pub(crate) fn print_devices_json(devices: &[DeviceInfo]) {
    println!("[");
    for (index, device) in devices.iter().enumerate() {
        let comma = if index + 1 == devices.len() { "" } else { "," };
        println!(
            "  {{\n    \"id\": \"{}\",\n    \"name\": \"{}\",\n    \"state\": \"{}\",\n    \"isDefault\": {}\n  }}{}",
            json_escape(&device.id),
            json_escape(&device.name),
            json_escape(&device.state),
            device.is_default,
            comma
        );
    }
    println!("]");
}

pub(crate) fn print_status_json(status: &MicStatus) {
    println!(
        "{{\n  \"device\": {{\n    \"id\": \"{}\",\n    \"name\": \"{}\",\n    \"state\": \"{}\",\n    \"isDefault\": {}\n  }},\n  \"volume\": {},\n  \"muted\": {}\n}}",
        json_escape(&status.device.id),
        json_escape(&status.device.name),
        json_escape(&status.device.state),
        status.device.is_default,
        status.volume,
        status.muted
    );
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if c.is_control() => escaped.push_str(&format!("\\u{:04x}", c as u32)),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quadcast_vendor_spellings_are_matched() {
        for name in [
            "Microphone (HyperX QuadCast S)",
            "Headphones (HyperX QuadCast S)",
            "Microphone (Kingston HyperX QuadCast S)",
            "microphone (quadcast s)",
            // macOS names the device without the endpoint prefix.
            "HyperX QuadCast S",
        ] {
            assert!(is_quadcast_device_name(name), "expected {name} to match");
        }
    }

    /// Hardware check: the capture meter must open against the QuadCast S itself, repeatedly.
    ///
    /// Ignored by default because it needs the mic plugged in. Run with
    /// `cargo test -- --ignored quadcast_capture_stream_opens_repeatedly --nocapture`.
    #[test]
    #[ignore = "requires a connected HyperX QuadCast S"]
    fn quadcast_capture_stream_opens_repeatedly() {
        #[cfg(windows)]
        let _com = crate::com::ComApartment::init().expect("COM init");
        let device = quadcast_input_device().unwrap_or_else(|error| {
            // Worth naming what was there instead: inside an RDP session Windows replaces the
            // physical endpoints with "Remote Audio" and the mic is genuinely unreachable.
            let seen = cpal::default_host()
                .input_devices()
                .map(|devices| devices.map(|d| d.to_string()).collect::<Vec<_>>())
                .unwrap_or_default();
            panic!("{error} capture devices visible to this session: {seen:?}")
        });
        println!("resolved capture device: {device}");
        assert!(is_quadcast_device_name(&device.to_string()));

        // Open, drop, reopen: a replug drives exactly this cycle, and the previous
        // default-device handle is what failed partway through it.
        for attempt in 1..=3 {
            let monitor = start_audio_peak_monitor().unwrap_or_else(|error| {
                panic!("attempt {attempt} failed to open the capture stream: {error}")
            });
            std::thread::sleep(std::time::Duration::from_millis(250));
            assert!(!monitor.failed(), "attempt {attempt} errored the stream");
            println!("attempt {attempt}: peak={:.4}", monitor.peak());
        }
    }

    #[test]
    fn foreign_microphones_are_not_matched() {
        // Every one of these has been the Windows default at some point on the dev machine;
        // the app must not treat any of them as the QuadCast S.
        for name in [
            "Microphone (Creative Pebble X Plus)",
            "Microphone (Realtek(R) Audio)",
            "Microphone (USB Webcam)",
            "Line In (High Definition Audio Device)",
            "Unknown microphone",
            "MacBook Pro Microphone",
            "Razer Kiyo",
        ] {
            assert!(
                !is_quadcast_device_name(name),
                "expected {name} not to match"
            );
        }
    }

    #[test]
    fn percent_maps_onto_the_ends_of_the_db_range() {
        let range = AudioClassControl::Mic.db_range();
        assert_eq!(percent_to_db(range, 0), -8.0);
        assert_eq!(percent_to_db(range, 100), 7.0);
        assert_eq!(percent_to_db(range, 50), -0.5);
    }

    #[test]
    fn percent_above_one_hundred_is_clamped_to_the_top_of_the_range() {
        assert_eq!(
            percent_to_db(AudioClassControl::Headphone.db_range(), 200),
            -9.0
        );
    }

    #[test]
    fn every_slider_position_round_trips_through_db() {
        for control in [
            AudioClassControl::Mic,
            AudioClassControl::Monitoring,
            AudioClassControl::Headphone,
        ] {
            let range = control.db_range();
            for percent in 0..=100u8 {
                assert_eq!(db_to_percent(range, percent_to_db(range, percent)), percent);
            }
        }
    }

    #[test]
    fn gain_outside_the_slider_range_reads_back_clamped() {
        let range = AudioClassControl::Monitoring.db_range();
        assert_eq!(db_to_percent(range, -96.0), 0);
        assert_eq!(db_to_percent(range, 12.0), 100);
    }

    #[test]
    fn without_a_hold_the_latest_buffer_peak_is_published() {
        let decay = peak_hold_decay(480, 48_000, None);

        assert_eq!(decay, 0.0);
        assert_eq!(held_peak(0.5, 0.1, decay), 0.1);
    }

    #[test]
    fn held_peak_halves_once_per_half_life() {
        // 4800 frames at 48 kHz is 100 ms, one half-life.
        let decay = peak_hold_decay(4800, 48_000, Some(Duration::from_millis(100)));

        assert!((decay - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_louder_earlier_peak_fades_instead_of_vanishing() {
        assert_eq!(held_peak(0.8, 0.1, 0.5), 0.4);
    }

    #[test]
    fn a_louder_new_buffer_replaces_the_held_peak() {
        assert_eq!(held_peak(0.2, 0.6, 0.9), 0.6);
    }

    #[test]
    fn degenerate_db_input_reads_back_as_zero() {
        assert_eq!(db_to_percent((0.0, 0.0), 0.0), 0);
        assert_eq!(db_to_percent((-8.0, 7.0), f32::NAN), 0);
    }
}
