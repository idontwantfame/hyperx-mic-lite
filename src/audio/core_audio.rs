//! CoreAudio backend.
//!
//! macOS publishes the QuadCast S as two audio devices that share a name: one carries only
//! input streams, the other only output streams. The USB Audio Class feature units that
//! Windows reaches through `IDeviceTopology` show up here as plain device properties, with
//! the same dB ranges NGENUITY uses:
//!
//! | control    | device  | property scope | mute         | volume (dB)        |
//! |------------|---------|----------------|--------------|--------------------|
//! | mic        | capture | input          | main element | channels 1 and 2   |
//! | monitoring | capture | play-through   | main element | channels 1 and 2   |
//! | headphone  | render  | output         | main element | channels 1 and 2   |
//!
//! Levels are written in dB rather than as the 0-1 scalar the system slider uses, so a
//! percentage lands on exactly the gain the Windows topology write produces.

use std::{error::Error, fmt, mem, ptr::NonNull};

use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectHasProperty,
    AudioObjectID, AudioObjectIsPropertySettable, AudioObjectPropertyAddress,
    AudioObjectPropertyElement, AudioObjectPropertyScope, AudioObjectPropertySelector,
    AudioObjectSetPropertyData, kAudioDevicePropertyDeviceUID, kAudioDevicePropertyMute,
    kAudioDevicePropertyStreams, kAudioDevicePropertyVolumeDecibels,
    kAudioDevicePropertyVolumeRangeDecibels, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain, kAudioObjectPropertyName,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
    kAudioObjectPropertyScopeOutput, kAudioObjectPropertyScopePlayThrough,
    kAudioObjectSystemObject,
};
use objc2_core_foundation::{CFRetained, CFString};

use super::{
    AudioClassControl, EndpointFlow, db_to_percent, is_quadcast_device_name, percent_to_db,
};
use crate::model::{DeviceInfo, MicStatus};

pub(crate) type AudioResult<T> = Result<T, AudioError>;

/// Highest channel element probed for a per-channel volume control. The QuadCast S has two;
/// the bound only stops a misbehaving driver from being probed forever.
const MAX_VOLUME_CHANNELS: AudioObjectPropertyElement = 8;

const SYSTEM_OBJECT: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

#[derive(Debug)]
pub(crate) struct AudioError(String);

impl AudioError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    fn status(operation: &str, status: i32) -> Self {
        Self(format!("{operation} failed: {}", describe_status(status)))
    }
}

impl fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for AudioError {}

/// CoreAudio errors are four-character codes packed into an `OSStatus`; show the code when
/// it is printable, since that is what Apple's headers and forums refer to.
fn describe_status(status: i32) -> String {
    let bytes = status.to_be_bytes();
    if bytes
        .iter()
        .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
    {
        format!("'{}' ({status})", String::from_utf8_lossy(&bytes))
    } else {
        format!("OSStatus {status}")
    }
}

fn check(status: i32, operation: &str) -> AudioResult<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(AudioError::status(operation, status))
    }
}

/// Error returned when the QuadCast S is not plugged in.
///
/// Every control here acts on the QuadCast S specifically; when it is absent the answer is
/// a failure rather than a silent redirect to whichever device macOS considers default.
fn quadcast_not_connected() -> AudioError {
    AudioError::new("HyperX QuadCast S is not connected")
}

/// Property values CoreAudio fills in place. Every bit pattern of these is a valid value,
/// so reading one cannot produce an invalid Rust value whatever the HAL writes.
trait PlainValue: Copy + Default {}

impl PlainValue for u32 {}
impl PlainValue for f32 {}
/// `AudioValueRange`: minimum and maximum as two `Float64`s.
impl PlainValue for [f64; 2] {}

fn address(
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    element: AudioObjectPropertyElement,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: element,
    }
}

fn has_property(object: AudioObjectID, address: AudioObjectPropertyAddress) -> bool {
    // SAFETY: address is an initialized struct borrowed for the duration of the call.
    unsafe { AudioObjectHasProperty(object, NonNull::from(&address)) }
}

fn is_settable(object: AudioObjectID, address: AudioObjectPropertyAddress) -> bool {
    let mut settable = 0u8;
    // SAFETY: address is borrowed for the call and settable is a live Boolean out-param.
    let status = unsafe {
        AudioObjectIsPropertySettable(
            object,
            NonNull::from(&address),
            NonNull::from(&mut settable),
        )
    };
    status == 0 && settable != 0
}

fn data_size(
    object: AudioObjectID,
    address: AudioObjectPropertyAddress,
    operation: &str,
) -> AudioResult<u32> {
    let mut size = 0u32;
    // SAFETY: address is borrowed for the call, no qualifier is passed (size 0, null), and
    // size is a live u32 out-param.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    check(status, operation)?;
    Ok(size)
}

fn read<T: PlainValue>(
    object: AudioObjectID,
    address: AudioObjectPropertyAddress,
    operation: &str,
) -> AudioResult<T> {
    let mut value = T::default();
    let mut size = mem::size_of::<T>() as u32;
    // SAFETY: value is a T-sized buffer and size reports exactly that capacity, so the HAL
    // cannot write past it; T: PlainValue keeps any bytes it writes a valid T.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    check(status, operation)?;
    if size as usize != mem::size_of::<T>() {
        return Err(AudioError::new(format!(
            "{operation} returned {size} bytes, expected {}",
            mem::size_of::<T>()
        )));
    }
    Ok(value)
}

fn write<T: PlainValue>(
    object: AudioObjectID,
    address: AudioObjectPropertyAddress,
    value: T,
    operation: &str,
) -> AudioResult<()> {
    // SAFETY: value is a live T and the size passed is exactly its size; the HAL copies the
    // data before returning.
    let status = unsafe {
        AudioObjectSetPropertyData(
            object,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            mem::size_of::<T>() as u32,
            NonNull::from(&value).cast(),
        )
    };
    check(status, operation)
}

fn read_object_ids(
    object: AudioObjectID,
    address: AudioObjectPropertyAddress,
    operation: &str,
) -> AudioResult<Vec<AudioObjectID>> {
    let capacity =
        data_size(object, address, operation)? as usize / mem::size_of::<AudioObjectID>();
    let mut ids = vec![0 as AudioObjectID; capacity];
    if ids.is_empty() {
        return Ok(ids);
    }
    let mut size = (ids.len() * mem::size_of::<AudioObjectID>()) as u32;
    // SAFETY: ids owns exactly `size` bytes of AudioObjectID slots, and AudioObjectID is a
    // plain u32, so any value the HAL writes is valid.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(ids.as_mut_slice()).cast(),
        )
    };
    check(status, operation)?;
    // A device can disappear between the size query and the read.
    ids.truncate(size as usize / mem::size_of::<AudioObjectID>());
    Ok(ids)
}

/// Read a property whose value is a `CFStringRef` the caller owns.
fn read_string(
    object: AudioObjectID,
    address: AudioObjectPropertyAddress,
    operation: &str,
) -> AudioResult<String> {
    let mut raw: *const CFString = std::ptr::null();
    let mut size = mem::size_of::<*const CFString>() as u32;
    // SAFETY: only called with selectors documented to return a CFStringRef; raw is a
    // pointer-sized slot and size reports exactly that.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut raw).cast(),
        )
    };
    check(status, operation)?;
    let string = NonNull::new(raw.cast_mut())
        .ok_or_else(|| AudioError::new(format!("{operation} returned no string")))?;
    // SAFETY: the HAL transfers ownership of the returned CFString (the "Copy" rule), so
    // wrapping it releases it exactly once when the CFRetained drops.
    Ok(unsafe { CFRetained::from_raw(string) }.to_string())
}

fn stream_scope(flow: EndpointFlow) -> AudioObjectPropertyScope {
    match flow {
        EndpointFlow::Capture => kAudioObjectPropertyScopeInput,
        EndpointFlow::Render => kAudioObjectPropertyScopeOutput,
    }
}

/// The property scope that holds a control on its device.
fn control_scope(control: AudioClassControl) -> AudioObjectPropertyScope {
    match control {
        AudioClassControl::Mic => kAudioObjectPropertyScopeInput,
        AudioClassControl::Monitoring => kAudioObjectPropertyScopePlayThrough,
        AudioClassControl::Headphone => kAudioObjectPropertyScopeOutput,
    }
}

fn all_devices() -> AudioResult<Vec<AudioObjectID>> {
    read_object_ids(
        SYSTEM_OBJECT,
        address(
            kAudioHardwarePropertyDevices,
            kAudioObjectPropertyScopeGlobal,
            kAudioObjectPropertyElementMain,
        ),
        "list audio devices",
    )
}

fn has_streams(device: AudioObjectID, scope: AudioObjectPropertyScope) -> bool {
    data_size(
        device,
        address(
            kAudioDevicePropertyStreams,
            scope,
            kAudioObjectPropertyElementMain,
        ),
        "read device streams",
    )
    .is_ok_and(|size| size > 0)
}

fn device_name(device: AudioObjectID) -> String {
    read_string(
        device,
        address(
            kAudioObjectPropertyName,
            kAudioObjectPropertyScopeGlobal,
            kAudioObjectPropertyElementMain,
        ),
        "read device name",
    )
    .ok()
    .filter(|name| !name.trim().is_empty())
    .unwrap_or_else(|| "Unknown microphone".to_string())
}

fn device_uid(device: AudioObjectID) -> AudioResult<String> {
    read_string(
        device,
        address(
            kAudioDevicePropertyDeviceUID,
            kAudioObjectPropertyScopeGlobal,
            kAudioObjectPropertyElementMain,
        ),
        "read device UID",
    )
}

fn default_input_device() -> AudioResult<AudioObjectID> {
    read(
        SYSTEM_OBJECT,
        address(
            kAudioHardwarePropertyDefaultInputDevice,
            kAudioObjectPropertyScopeGlobal,
            kAudioObjectPropertyElementMain,
        ),
        "read default input device",
    )
}

/// Locate the QuadCast S half that carries `flow`.
///
/// Matched on name and stream direction together: both halves share the name, and only the
/// direction tells the microphone apart from the headphone output.
fn find_quadcast(flow: EndpointFlow) -> AudioResult<Option<AudioObjectID>> {
    let scope = stream_scope(flow);
    Ok(all_devices()?.into_iter().find(|&device| {
        has_streams(device, scope) && is_quadcast_device_name(&device_name(device))
    }))
}

fn quadcast_device(flow: EndpointFlow) -> AudioResult<AudioObjectID> {
    find_quadcast(flow)?.ok_or_else(quadcast_not_connected)
}

/// CoreAudio only lists devices that are present, so every device it reports is active.
fn describe_device(device: AudioObjectID) -> AudioResult<DeviceInfo> {
    Ok(DeviceInfo {
        id: device_uid(device)?,
        name: device_name(device),
        state: "active".to_string(),
        is_default: false,
    })
}

/// Describe the QuadCast S device for `flow`, or `None` when it is not connected.
pub(crate) fn hyperx_endpoint_info(flow: EndpointFlow) -> AudioResult<Option<DeviceInfo>> {
    find_quadcast(flow)?.map(describe_device).transpose()
}

/// Elements that carry a scope's volume: the main element when the device has a master
/// control, otherwise one element per channel.
fn volume_elements(
    device: AudioObjectID,
    scope: AudioObjectPropertyScope,
) -> Vec<AudioObjectPropertyElement> {
    let volume = |element| address(kAudioDevicePropertyVolumeDecibels, scope, element);
    if has_property(device, volume(kAudioObjectPropertyElementMain)) {
        return vec![kAudioObjectPropertyElementMain];
    }
    (1..=MAX_VOLUME_CHANNELS)
        .take_while(|&element| has_property(device, volume(element)))
        .collect()
}

fn volume_range(
    device: AudioObjectID,
    scope: AudioObjectPropertyScope,
    element: AudioObjectPropertyElement,
) -> Option<(f32, f32)> {
    read::<[f64; 2]>(
        device,
        address(kAudioDevicePropertyVolumeRangeDecibels, scope, element),
        "read volume range",
    )
    .ok()
    .filter(|[min, max]| min <= max)
    .map(|[min, max]| (min as f32, max as f32))
}

fn write_volume(device: AudioObjectID, control: AudioClassControl, percent: u8) -> AudioResult<()> {
    let scope = control_scope(control);
    let elements = volume_elements(device, scope);
    if elements.is_empty() {
        return Err(AudioError::new(format!(
            "The QuadCast S has no {} volume control",
            control.label()
        )));
    }
    let target = percent_to_db(control.db_range(), percent);
    for element in elements {
        let level = volume_range(device, scope, element)
            .map_or(target, |(min, max)| target.clamp(min, max));
        write(
            device,
            address(kAudioDevicePropertyVolumeDecibels, scope, element),
            level,
            "set volume",
        )?;
    }
    Ok(())
}

fn read_volume_percent(device: AudioObjectID, control: AudioClassControl) -> AudioResult<u8> {
    let scope = control_scope(control);
    let element = volume_elements(device, scope)
        .first()
        .copied()
        .ok_or_else(|| {
            AudioError::new(format!(
                "The QuadCast S has no {} volume control",
                control.label()
            ))
        })?;
    let db: f32 = read(
        device,
        address(kAudioDevicePropertyVolumeDecibels, scope, element),
        "read volume",
    )?;
    Ok(db_to_percent(control.db_range(), db))
}

fn mute_address(control: AudioClassControl) -> AudioObjectPropertyAddress {
    address(
        kAudioDevicePropertyMute,
        control_scope(control),
        kAudioObjectPropertyElementMain,
    )
}

fn write_mute(device: AudioObjectID, control: AudioClassControl, muted: bool) -> AudioResult<()> {
    write(device, mute_address(control), u32::from(muted), "set mute")
}

fn read_mute(device: AudioObjectID, control: AudioClassControl) -> AudioResult<bool> {
    read::<u32>(device, mute_address(control), "read mute").map(|muted| muted != 0)
}

fn control_device(control: AudioClassControl) -> AudioResult<AudioObjectID> {
    quadcast_device(control.endpoint_flow())
}

pub(super) fn write_mic_mute(muted: bool) -> AudioResult<()> {
    write_mute(
        control_device(AudioClassControl::Mic)?,
        AudioClassControl::Mic,
        muted,
    )
}

pub(super) fn toggle_mic_mute() -> AudioResult<()> {
    let device = control_device(AudioClassControl::Mic)?;
    let muted = read_mute(device, AudioClassControl::Mic)?;
    write_mute(device, AudioClassControl::Mic, !muted)
}

pub(super) fn write_mic_volume(percent: u8) -> AudioResult<()> {
    write_volume(
        control_device(AudioClassControl::Mic)?,
        AudioClassControl::Mic,
        percent,
    )
}

/// Set the monitoring or headphone level. The mic level goes through [`write_mic_volume`].
pub(super) fn write_control_volume(control: AudioClassControl, percent: u8) -> AudioResult<()> {
    write_volume(control_device(control)?, control, percent)
}

/// Mute the monitoring or headphone path. The mic mute goes through [`write_mic_mute`].
pub(super) fn write_control_mute(control: AudioClassControl, muted: bool) -> AudioResult<()> {
    write_mute(control_device(control)?, control, muted)
}

/// macOS has no per-device input meter comparable to `IAudioMeterInformation`, so the
/// capture stream in the parent module is the only level source.
pub(crate) fn input_peak_value() -> AudioResult<f32> {
    Err(AudioError::new(
        "macOS has no endpoint peak meter; input levels come from the capture stream",
    ))
}

pub(crate) fn mic_status() -> AudioResult<MicStatus> {
    let device = quadcast_device(EndpointFlow::Capture)?;
    let mut info = describe_device(device)?;
    info.is_default = default_input_device().is_ok_and(|default| default == device);
    Ok(MicStatus {
        device: info,
        volume: read_volume_percent(device, AudioClassControl::Mic)?,
        muted: read_mute(device, AudioClassControl::Mic)?,
    })
}

pub(crate) fn list_capture_devices() -> AudioResult<Vec<DeviceInfo>> {
    let default = default_input_device().ok();
    all_devices()?
        .into_iter()
        .filter(|&device| has_streams(device, kAudioObjectPropertyScopeInput))
        .map(|device| {
            describe_device(device).map(|info| DeviceInfo {
                is_default: Some(device) == default,
                ..info
            })
        })
        .collect()
}

/// Print every mute and volume control the device exposes, the CoreAudio counterpart of
/// the Windows topology walk.
pub(super) fn print_audio_topology(flow: EndpointFlow) -> AudioResult<()> {
    let device = quadcast_device(flow)?;
    println!(
        "Controls for {} ({})",
        device_name(device),
        device_uid(device).unwrap_or_default()
    );
    for (label, scope) in [
        ("input", kAudioObjectPropertyScopeInput),
        ("output", kAudioObjectPropertyScopeOutput),
        ("play-through", kAudioObjectPropertyScopePlayThrough),
    ] {
        print_scope_controls(device, label, scope);
    }
    Ok(())
}

fn print_scope_controls(device: AudioObjectID, label: &str, scope: AudioObjectPropertyScope) {
    let settable = |address| {
        if is_settable(device, address) {
            "settable"
        } else {
            "read-only"
        }
    };
    let mute = address(
        kAudioDevicePropertyMute,
        scope,
        kAudioObjectPropertyElementMain,
    );
    if has_property(device, mute) {
        let value = read::<u32>(device, mute, "read mute")
            .map(|muted| if muted != 0 { "on" } else { "off" }.to_string())
            .unwrap_or_else(|error| error.to_string());
        println!("  {label} mute={value} {}", settable(mute));
    }
    for element in volume_elements(device, scope) {
        let volume = address(kAudioDevicePropertyVolumeDecibels, scope, element);
        let value = read::<f32>(device, volume, "read volume")
            .map(|db| format!("{db:.1} dB"))
            .unwrap_or_else(|error| error.to_string());
        let range = volume_range(device, scope, element)
            .map(|(min, max)| format!("{min:.1}..{max:.1} dB"))
            .unwrap_or_else(|| "unknown".to_string());
        println!(
            "  {label} channel {element} volume={value} range={range} {}",
            settable(volume)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_character_status_codes_are_spelled_out() {
        // kAudioHardwareBadObjectError
        assert_eq!(describe_status(0x216f626a), "'!obj' (560947818)");
    }

    #[test]
    fn numeric_status_codes_are_left_as_numbers() {
        assert_eq!(describe_status(-50), "OSStatus -50");
    }

    #[test]
    fn each_control_reads_from_its_own_scope() {
        assert_eq!(
            control_scope(AudioClassControl::Mic),
            kAudioObjectPropertyScopeInput
        );
        assert_eq!(
            control_scope(AudioClassControl::Monitoring),
            kAudioObjectPropertyScopePlayThrough
        );
        assert_eq!(
            control_scope(AudioClassControl::Headphone),
            kAudioObjectPropertyScopeOutput
        );
    }
}
