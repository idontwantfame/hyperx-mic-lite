use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    process,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Media::Audio::{
            DEVICE_STATE, DEVICE_STATE_ACTIVE, DEVICE_STATE_DISABLED, DEVICE_STATE_NOTPRESENT,
            DEVICE_STATE_UNPLUGGED, DEVICE_STATEMASK_ALL,
            Endpoints::{IAudioEndpointVolume, IAudioMeterInformation},
            IAudioMute, IAudioVolumeLevel, IDeviceTopology, IMMDevice, IMMDeviceEnumerator,
            MMDeviceEnumerator, eCapture, eCommunications, eRender,
        },
        System::{
            Com::StructuredStorage::PropVariantClear,
            Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ},
            Variant::VT_LPWSTR,
        },
    },
    core::{Error, HRESULT, Interface, Result as WinResult, Type},
};

use crate::{
    logging::{json_string, log_event},
    model::{DeviceInfo, MicStatus},
};
pub(crate) struct AudioPeakMonitor {
    peak_bits: Arc<AtomicU32>,
    _stream: cpal::Stream,
}

impl AudioPeakMonitor {
    pub(crate) fn peak(&self) -> f32 {
        peak_from_bits(&self.peak_bits)
    }

    pub(crate) fn peak_bits(&self) -> Arc<AtomicU32> {
        self.peak_bits.clone()
    }
}

pub(crate) fn peak_from_bits(peak_bits: &AtomicU32) -> f32 {
    f32::from_bits(peak_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0)
}

pub(crate) fn set_volume(args: &[String]) -> WinResult<()> {
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

pub(crate) fn run_audio_command(args: &[String]) -> WinResult<()> {
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
                "capture" | "mic" | "input" => eCapture,
                "render" | "headphone" | "output" => eRender,
                _ => {
                    audio_usage();
                    process::exit(2);
                }
            };
            print_audio_topology(flow)?;
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

    fn volume_part_id(self) -> u32 {
        match self {
            Self::Mic => 0x20008,
            Self::Monitoring => 0x2000a,
            Self::Headphone => 0x20006,
        }
    }

    fn mute_part_id(self) -> u32 {
        match self {
            Self::Mic => 0x20007,
            Self::Monitoring => 0x20009,
            Self::Headphone => 0x20005,
        }
    }

    fn db_range(self) -> (f32, f32) {
        match self {
            Self::Mic => (-8.0, 7.0),
            Self::Monitoring => (-30.0, 6.0),
            Self::Headphone => (-40.0, -9.0),
        }
    }

    fn endpoint_flow(self) -> windows::Win32::Media::Audio::EDataFlow {
        match self {
            Self::Headphone => eRender,
            Self::Mic | Self::Monitoring => eCapture,
        }
    }
}

pub(crate) fn set_mic_mute(muted: bool) -> WinResult<()> {
    // SAFETY: the IAudioEndpointVolume comes from Activate on a live device; SetMute
    // documents the event-context GUID pointer as optional, so null is allowed.
    let result =
        unsafe { endpoint_volume(&default_capture_device()?)?.SetMute(muted, std::ptr::null()) };
    if result.is_ok() {
        log_event("info", "audio.mute.set", &[("muted", muted.to_string())]);
    }
    result
}

pub(crate) fn toggle_mic_mute() -> WinResult<()> {
    let volume = endpoint_volume(&default_capture_device()?)?;
    // SAFETY: volume is a live IAudioEndpointVolume obtained above; GetMute only writes
    // its BOOL out-param.
    let muted = unsafe { volume.GetMute()?.as_bool() };
    // SAFETY: same live interface pointer; SetMute allows a null event-context GUID.
    unsafe { volume.SetMute(!muted, std::ptr::null())? };
    Ok(())
}

pub(crate) fn set_mic_volume_percent(percent: u8) -> WinResult<()> {
    // SAFETY: the IAudioEndpointVolume comes from Activate on a live device;
    // SetMasterVolumeLevelScalar allows a null event-context GUID.
    let result = unsafe {
        endpoint_volume(&default_capture_device()?)?
            .SetMasterVolumeLevelScalar(percent as f32 / 100.0, std::ptr::null())
    };
    if result.is_ok() {
        if let Err(error) = set_topology_control_volume(AudioClassControl::Mic, percent) {
            log_event(
                "warn",
                "audio.usb_class.volume.mic.error",
                &[("message", error.to_string())],
            );
        }
    }
    if result.is_ok() {
        log_event(
            "info",
            "audio.volume.set",
            &[("percent", percent.to_string())],
        );
    }
    result
}

pub(crate) fn set_audio_control_volume(control: AudioClassControl, percent: u8) -> WinResult<()> {
    match control {
        AudioClassControl::Mic => set_mic_volume_percent(percent),
        AudioClassControl::Monitoring => set_topology_control_volume(control, percent),
        AudioClassControl::Headphone => {
            // Best effort: an endpoint failure must not skip the topology write below.
            if let Ok(device) = hyperx_render_device() {
                // SAFETY: device is a valid IMMDevice; the activated IAudioEndpointVolume is
                // used within the closure, and a null event-context GUID is allowed.
                let endpoint_result = unsafe {
                    endpoint_volume(&device).and_then(|volume| {
                        volume.SetMasterVolumeLevelScalar(percent as f32 / 100.0, std::ptr::null())
                    })
                };
                if let Err(error) = endpoint_result {
                    log_event(
                        "warn",
                        "audio.usb_class.volume.endpoint_error",
                        &[("message", error.to_string())],
                    );
                }
            }
            set_topology_control_volume(control, percent)
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

fn set_audio_control_mute(control: AudioClassControl, muted: bool) -> WinResult<()> {
    match control {
        AudioClassControl::Mic => set_mic_mute(muted),
        AudioClassControl::Monitoring | AudioClassControl::Headphone => {
            set_topology_control_mute(control, muted)
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

fn set_topology_control_volume(control: AudioClassControl, percent: u8) -> WinResult<()> {
    let device = hyperx_audio_device(control.endpoint_flow())?;
    // SAFETY: device is a valid IMMDevice; Activate with no activation params is valid for
    // IDeviceTopology and returns an owned interface on success.
    let topology: IDeviceTopology = unsafe { device.Activate(CLSCTX_ALL, None)? };
    let part = find_topology_part(&topology, control.volume_part_id())?
        // SAFETY: topology is a live IDeviceTopology; GetPartById returns an owned IPart.
        .or_else(|| unsafe { topology.GetPartById(control.volume_part_id()).ok() })
        .ok_or_else(|| {
            Error::new(
                HRESULT(0x80070490u32 as i32),
                "Topology volume part not found",
            )
        })?;
    let volume = activate_part_interface::<IAudioVolumeLevel>(&part)?;
    let (captured_min, captured_max) = control.db_range();
    let mut target = captured_min + (captured_max - captured_min) * percent as f32 / 100.0;
    // SAFETY: volume is the IAudioVolumeLevel activated above for this part; GetLevelRange
    // writes only to the three local f32 out-params, and SetLevel is called with channel
    // indices below the count reported by GetChannelCount on the same interface.
    unsafe {
        let channels = volume.GetChannelCount().unwrap_or(2).max(1);
        let mut min = 0.0f32;
        let mut max = 0.0f32;
        let mut stepping = 0.0f32;
        if volume
            .GetLevelRange(0, &mut min, &mut max, &mut stepping)
            .is_ok()
        {
            target = target.clamp(min, max);
        }
        for channel in 0..channels {
            volume.SetLevel(channel, target, None)?;
        }
    }
    Ok(())
}

fn set_topology_control_mute(control: AudioClassControl, muted: bool) -> WinResult<()> {
    let device = hyperx_audio_device(control.endpoint_flow())?;
    // SAFETY: device is a valid IMMDevice; Activate with no activation params is valid for
    // IDeviceTopology and returns an owned interface on success.
    let topology: IDeviceTopology = unsafe { device.Activate(CLSCTX_ALL, None)? };
    let part = find_topology_part(&topology, control.mute_part_id())?
        // SAFETY: topology is a live IDeviceTopology; GetPartById returns an owned IPart.
        .or_else(|| unsafe { topology.GetPartById(control.mute_part_id()).ok() })
        .ok_or_else(|| {
            Error::new(
                HRESULT(0x80070490u32 as i32),
                "Topology mute part not found",
            )
        })?;
    let mute = activate_part_interface::<IAudioMute>(&part)?;
    // SAFETY: mute is the IAudioMute activated above for this part; the event-context
    // argument is optional and None is allowed.
    unsafe { mute.SetMute(muted, None) }
}

fn activate_part_interface<T: Interface>(
    part: &windows::Win32::Media::Audio::IPart,
) -> WinResult<T> {
    let mut raw = std::ptr::null_mut();
    // SAFETY: part is a valid IPart; on success Activate writes an owned pointer implementing
    // T::IID into raw, and Type::from_abi takes ownership of exactly that pointer.
    unsafe {
        part.Activate(CLSCTX_ALL.0 as u32, &T::IID, Some(&mut raw))?;
        Type::from_abi(raw)
    }
}

fn hyperx_render_device() -> WinResult<IMMDevice> {
    hyperx_audio_device(eRender)
}

fn hyperx_audio_device(flow: windows::Win32::Media::Audio::EDataFlow) -> WinResult<IMMDevice> {
    let enumerator = device_enumerator()?;
    // SAFETY: enumerator is a valid IMMDeviceEnumerator created just above; the returned
    // collection is an owned IMMDeviceCollection.
    let collection =
        unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE(DEVICE_STATEMASK_ALL))? };
    // SAFETY: collection is the live IMMDeviceCollection obtained above.
    let count = unsafe { collection.GetCount()? };
    for index in 0..count {
        // SAFETY: index < count reported by GetCount on this same collection.
        let device = unsafe { collection.Item(index)? };
        let name = device_name(&device)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.contains("hyperx") || name.contains("quadcast") {
            return Ok(device);
        }
    }
    // SAFETY: enumerator is still the valid IMMDeviceEnumerator created above; the method
    // returns an owned IMMDevice on success.
    unsafe { enumerator.GetDefaultAudioEndpoint(flow, eCommunications) }
}

fn print_audio_topology(flow: windows::Win32::Media::Audio::EDataFlow) -> WinResult<()> {
    let device = hyperx_audio_device(flow)?;
    // SAFETY: device is a valid IMMDevice; Activate with no activation params is valid for
    // IDeviceTopology and returns an owned interface on success.
    let topology: IDeviceTopology = unsafe { device.Activate(CLSCTX_ALL, None)? };
    let device_name = device_name(&device).unwrap_or_else(|_| "Unknown".to_string());
    println!("Topology for {device_name}");
    let mut visited = Vec::new();
    // SAFETY: topology is a live IDeviceTopology; every subunit, connector, and part used in
    // this block is an owned COM interface it returned, and indices stay below the counts
    // reported by GetSubunitCount/GetConnectorCount on the same object.
    unsafe {
        let subunit_count = topology.GetSubunitCount()?;
        for index in 0..subunit_count {
            let subunit = topology.GetSubunit(index)?;
            if let Ok(part) = subunit.cast() {
                print_topology_part(&part, 0, &mut visited)?;
            }
        }
        let connector_count = topology.GetConnectorCount()?;
        for index in 0..connector_count {
            let connector = topology.GetConnector(index)?;
            if let Ok(connected) = connector.GetConnectedTo() {
                if let Ok(part) = connected.cast() {
                    print_topology_part(&part, 0, &mut visited)?;
                }
            }
        }
    }
    Ok(())
}

fn find_topology_part(
    topology: &IDeviceTopology,
    id: u32,
) -> WinResult<Option<windows::Win32::Media::Audio::IPart>> {
    let mut visited = Vec::new();
    // SAFETY: topology is a live IDeviceTopology borrowed from the caller; every subunit,
    // connector, and part used here is an owned COM interface it returned, and indices stay
    // below the counts reported by GetSubunitCount/GetConnectorCount on the same object.
    unsafe {
        let subunit_count = topology.GetSubunitCount()?;
        for index in 0..subunit_count {
            let subunit = topology.GetSubunit(index)?;
            if let Ok(part) = subunit.cast() {
                if let Some(found) = find_topology_part_from(&part, id, &mut visited)? {
                    return Ok(Some(found));
                }
            }
        }
        let connector_count = topology.GetConnectorCount()?;
        for index in 0..connector_count {
            let connector = topology.GetConnector(index)?;
            if let Ok(connected) = connector.GetConnectedTo() {
                if let Ok(part) = connected.cast() {
                    if let Some(found) = find_topology_part_from(&part, id, &mut visited)? {
                        return Ok(Some(found));
                    }
                }
            }
        }
    }
    Ok(None)
}

fn find_topology_part_from(
    part: &windows::Win32::Media::Audio::IPart,
    id: u32,
    visited: &mut Vec<u32>,
) -> WinResult<Option<windows::Win32::Media::Audio::IPart>> {
    // SAFETY: part is a valid IPart borrowed from the caller; the parts lists and children it
    // returns are owned COM interfaces, and GetPart is only called with index < GetCount().
    unsafe {
        let local_id = part.GetLocalId()?;
        if local_id == id {
            return Ok(Some(part.clone()));
        }
        if visited.contains(&local_id) {
            return Ok(None);
        }
        visited.push(local_id);
        if let Ok(parts) = part.EnumPartsIncoming() {
            let count = parts.GetCount().unwrap_or(0);
            for index in 0..count {
                if let Ok(child) = parts.GetPart(index) {
                    if let Some(found) = find_topology_part_from(&child, id, visited)? {
                        return Ok(Some(found));
                    }
                }
            }
        }
        if let Ok(parts) = part.EnumPartsOutgoing() {
            let count = parts.GetCount().unwrap_or(0);
            for index in 0..count {
                if let Ok(child) = parts.GetPart(index) {
                    if let Some(found) = find_topology_part_from(&child, id, visited)? {
                        return Ok(Some(found));
                    }
                }
            }
        }
    }
    Ok(None)
}

fn print_topology_part(
    part: &windows::Win32::Media::Audio::IPart,
    depth: usize,
    visited: &mut Vec<u32>,
) -> WinResult<()> {
    // SAFETY: part is a valid IPart borrowed from the caller; controls, part lists, and
    // children are owned COM interfaces it returned, and every index stays below the
    // corresponding GetControlInterfaceCount/GetCount value from the same object.
    unsafe {
        let id = part.GetLocalId()?;
        if visited.contains(&id) {
            return Ok(());
        }
        visited.push(id);
        let indent = "  ".repeat(depth);
        let name = part
            .GetName()
            .ok()
            .and_then(|value| value.to_string().ok())
            .unwrap_or_default();
        let subtype = part.GetSubType().ok();
        println!("{indent}part id=0x{id:02x} name={name} subtype={subtype:?}");
        let control_count = part.GetControlInterfaceCount().unwrap_or(0);
        for index in 0..control_count {
            if let Ok(control) = part.GetControlInterface(index) {
                let control_name = control
                    .GetName()
                    .ok()
                    .and_then(|value| value.to_string().ok())
                    .unwrap_or_default();
                let iid = control.GetIID().ok();
                println!("{indent}  control name={control_name} iid={iid:?}");
            }
        }
        if let Ok(parts) = part.EnumPartsIncoming() {
            let count = parts.GetCount().unwrap_or(0);
            for index in 0..count {
                if let Ok(child) = parts.GetPart(index) {
                    print_topology_part(&child, depth + 1, visited)?;
                }
            }
        }
        if let Ok(parts) = part.EnumPartsOutgoing() {
            let count = parts.GetCount().unwrap_or(0);
            for index in 0..count {
                if let Ok(child) = parts.GetPart(index) {
                    print_topology_part(&child, depth + 1, visited)?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn input_peak_value() -> WinResult<f32> {
    let device = default_capture_device()?;
    let meter = endpoint_meter(&device)?;
    // SAFETY: meter is a live IAudioMeterInformation activated for the default capture
    // device; GetPeakValue only writes its f32 out-param.
    unsafe { meter.GetPeakValue() }
}

pub(crate) fn start_audio_peak_monitor() -> Result<AudioPeakMonitor, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "No default input device is available.".to_string())?;
    let config = device
        .default_input_config()
        .map_err(|error| error.to_string())?;
    let channels = config.channels() as usize;
    let stream_config: cpal::StreamConfig = config.clone().into();
    let peak_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            build_peak_stream::<f32>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::F64 => {
            build_peak_stream::<f64>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::I8 => {
            build_peak_stream::<i8>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::I16 => {
            build_peak_stream::<i16>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::I32 => {
            build_peak_stream::<i32>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::I64 => {
            build_peak_stream::<i64>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::U8 => {
            build_peak_stream::<u8>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::U16 => {
            build_peak_stream::<u16>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::U32 => {
            build_peak_stream::<u32>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        cpal::SampleFormat::U64 => {
            build_peak_stream::<u64>(&device, stream_config.clone(), channels, peak_bits.clone())
        }
        other => Err(format!("Unsupported input sample format: {other:?}")),
    }?;
    stream.play().map_err(|error| error.to_string())?;
    Ok(AudioPeakMonitor {
        peak_bits,
        _stream: stream,
    })
}

fn build_peak_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    peak_bits: Arc<AtomicU32>,
) -> Result<cpal::Stream, String>
where
    T: cpal::Sample + cpal::SizedSample + ToPeakSample + Send + 'static,
{
    device
        .build_input_stream(
            config,
            move |data: &[T], _| update_peak_from_samples(data, channels, &peak_bits),
            |error| {
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

fn update_peak_from_samples<T>(data: &[T], channels: usize, peak_bits: &AtomicU32)
where
    T: ToPeakSample,
{
    let step = channels.max(1);
    let peak = data
        .chunks(step)
        .flat_map(|frame| frame.iter())
        .map(|sample| sample.to_peak_sample().abs())
        .fold(0.0f32, f32::max)
        .clamp(0.0, 1.0);
    peak_bits.store(peak.to_bits(), Ordering::Relaxed);
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

pub(crate) fn mic_status() -> WinResult<MicStatus> {
    let device = default_capture_device()?;
    let mut info = describe_device(&device)?;
    info.is_default = true;

    let volume = endpoint_volume(&device)?;
    // SAFETY: volume is a live IAudioEndpointVolume obtained above; the getter only writes
    // its f32 out-param.
    let scalar = unsafe { volume.GetMasterVolumeLevelScalar()? };
    // SAFETY: same live interface pointer; GetMute only writes its BOOL out-param.
    let muted = unsafe { volume.GetMute()?.as_bool() };

    Ok(MicStatus {
        device: info,
        volume: (scalar * 100.0).round().clamp(0.0, 100.0) as u8,
        muted,
    })
}

pub(crate) fn list_capture_devices() -> WinResult<Vec<DeviceInfo>> {
    let enumerator = device_enumerator()?;
    let default_id = default_capture_device_with(&enumerator)
        .and_then(|device| device_id_string(&device))
        .unwrap_or_default();

    // SAFETY: enumerator is a valid IMMDeviceEnumerator created above; the returned
    // collection is an owned IMMDeviceCollection.
    let collection =
        unsafe { enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE(DEVICE_STATEMASK_ALL))? };

    // SAFETY: collection is the live IMMDeviceCollection obtained above.
    let count = unsafe { collection.GetCount()? };
    let mut devices = Vec::with_capacity(count as usize);

    for index in 0..count {
        // SAFETY: index < count reported by GetCount on this same collection.
        let device = unsafe { collection.Item(index)? };
        let mut info = describe_device(&device)?;
        info.is_default = info.id == default_id;
        devices.push(info);
    }

    Ok(devices)
}

fn default_capture_device() -> WinResult<IMMDevice> {
    default_capture_device_with(&device_enumerator()?)
}

fn default_capture_device_with(enumerator: &IMMDeviceEnumerator) -> WinResult<IMMDevice> {
    // SAFETY: enumerator is a valid IMMDeviceEnumerator borrowed from the caller; the method
    // returns an owned IMMDevice on success.
    unsafe { enumerator.GetDefaultAudioEndpoint(eCapture, eCommunications) }
}

fn device_enumerator() -> WinResult<IMMDeviceEnumerator> {
    // SAFETY: MMDeviceEnumerator is the well-known CLSID for this coclass; callers initialize
    // COM on this thread first (via ComApartment), and no outer aggregation is used (None).
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

fn endpoint_volume(device: &IMMDevice) -> WinResult<IAudioEndpointVolume> {
    // SAFETY: device is a valid IMMDevice; Activate with no activation params is valid for
    // IAudioEndpointVolume and returns an owned interface on success.
    unsafe { device.Activate(CLSCTX_ALL, None) }
}

fn endpoint_meter(device: &IMMDevice) -> WinResult<IAudioMeterInformation> {
    // SAFETY: device is a valid IMMDevice; Activate with no activation params is valid for
    // IAudioMeterInformation and returns an owned interface on success.
    unsafe { device.Activate(CLSCTX_ALL, None) }
}

fn device_id_string(device: &IMMDevice) -> WinResult<String> {
    // SAFETY: device is a valid IMMDevice; GetId returns an owned, null-terminated PWSTR
    // allocated with CoTaskMemAlloc, owned by the caller.
    let id = unsafe { device.GetId()? };
    // SAFETY: id is the null-terminated PWSTR returned above, read before it is freed.
    let text = unsafe { id.to_string().unwrap_or_default() };
    // SAFETY: id was allocated by GetId with CoTaskMemAlloc and is not used after this free.
    unsafe { CoTaskMemFree(Some(id.as_ptr() as *const _)) };
    Ok(text)
}

fn describe_device(device: &IMMDevice) -> WinResult<DeviceInfo> {
    let id = device_id_string(device)?;
    // SAFETY: same valid IMMDevice; GetState only writes its out-param.
    let state = unsafe { device.GetState()? };

    Ok(DeviceInfo {
        id,
        name: device_name(device)?,
        state: state_name(state.0),
        is_default: false,
    })
}

fn device_name(device: &IMMDevice) -> WinResult<String> {
    // SAFETY: device is a valid IMMDevice; STGM_READ is a supported access mode and the
    // returned store is an owned IPropertyStore.
    let store = unsafe { device.OpenPropertyStore(STGM_READ)? };
    // SAFETY: store is the live IPropertyStore opened above; GetValue writes an initialized
    // PROPVARIANT that we own (cleared below with PropVariantClear).
    let mut value = unsafe { store.GetValue(&PKEY_Device_FriendlyName)? };
    // PROPVARIANT is a tagged union; pwszVal is only valid when vt is VT_LPWSTR
    // (a device may report the property as VT_EMPTY or another type).
    // SAFETY: the vt tag is checked to be VT_LPWSTR before the pwszVal union field is read,
    // and the string is owned by value, which is still alive here.
    let name = unsafe {
        if value.Anonymous.Anonymous.vt == VT_LPWSTR {
            value
                .Anonymous
                .Anonymous
                .Anonymous
                .pwszVal
                .to_string()
                .unwrap_or_default()
        } else {
            String::new()
        }
    };
    // SAFETY: value is the PROPVARIANT initialized by GetValue above and is cleared exactly
    // once; the borrowed pwszVal string is no longer used after this point.
    unsafe { PropVariantClear(&mut value)? };

    if name.trim().is_empty() {
        Ok("Unknown microphone".to_string())
    } else {
        Ok(name)
    }
}

fn state_name(state: u32) -> String {
    match state {
        value if value == DEVICE_STATE_ACTIVE.0 => "active",
        value if value == DEVICE_STATE_DISABLED.0 => "disabled",
        value if value == DEVICE_STATE_NOTPRESENT.0 => "not_present",
        value if value == DEVICE_STATE_UNPLUGGED.0 => "unplugged",
        other => return format!("unknown_{other}"),
    }
    .to_string()
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
