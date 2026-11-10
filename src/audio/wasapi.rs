//! WASAPI backend: Core Audio endpoints plus the QuadCast S USB Audio Class topology.

use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Media::Audio::{
            DEVICE_STATE, DEVICE_STATE_ACTIVE, DEVICE_STATE_DISABLED, DEVICE_STATE_NOTPRESENT,
            DEVICE_STATE_UNPLUGGED, DEVICE_STATEMASK_ALL, EDataFlow,
            Endpoints::{IAudioEndpointVolume, IAudioMeterInformation},
            IAudioMute, IAudioVolumeLevel, IDeviceTopology, IMMDevice, IMMDeviceEnumerator, IPart,
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

use super::{AudioClassControl, EndpointFlow, is_quadcast_device_name, percent_to_db};
use crate::{
    logging::log_event,
    model::{DeviceInfo, MicStatus},
};

pub(crate) type AudioResult<T> = WinResult<T>;

impl AudioClassControl {
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
}

fn data_flow(flow: EndpointFlow) -> EDataFlow {
    match flow {
        EndpointFlow::Capture => eCapture,
        EndpointFlow::Render => eRender,
    }
}

pub(super) fn write_mic_mute(muted: bool) -> WinResult<()> {
    // SAFETY: the IAudioEndpointVolume comes from Activate on a live device; SetMute
    // documents the event-context GUID pointer as optional, so null is allowed.
    unsafe { endpoint_volume(&hyperx_capture_device()?)?.SetMute(muted, std::ptr::null()) }
}

pub(super) fn toggle_mic_mute() -> WinResult<()> {
    let volume = endpoint_volume(&hyperx_capture_device()?)?;
    // SAFETY: volume is a live IAudioEndpointVolume obtained above; GetMute only writes
    // its BOOL out-param.
    let muted = unsafe { volume.GetMute()?.as_bool() };
    // SAFETY: same live interface pointer; SetMute allows a null event-context GUID.
    unsafe { volume.SetMute(!muted, std::ptr::null())? };
    Ok(())
}

pub(super) fn write_mic_volume(percent: u8) -> WinResult<()> {
    // SAFETY: the IAudioEndpointVolume comes from Activate on a live device;
    // SetMasterVolumeLevelScalar allows a null event-context GUID.
    unsafe {
        endpoint_volume(&hyperx_capture_device()?)?
            .SetMasterVolumeLevelScalar(percent as f32 / 100.0, std::ptr::null())?
    };
    if let Err(error) = set_topology_control_volume(AudioClassControl::Mic, percent) {
        log_event(
            "warn",
            "audio.usb_class.volume.mic.error",
            &[("message", error.to_string())],
        );
    }
    Ok(())
}

/// Set the monitoring or headphone level. The mic level goes through [`write_mic_volume`].
pub(super) fn write_control_volume(control: AudioClassControl, percent: u8) -> WinResult<()> {
    if let AudioClassControl::Headphone = control {
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
    }
    set_topology_control_volume(control, percent)
}

/// Mute the monitoring or headphone path. The mic mute goes through [`write_mic_mute`].
pub(super) fn write_control_mute(control: AudioClassControl, muted: bool) -> WinResult<()> {
    set_topology_control_mute(control, muted)
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
    let mut target = percent_to_db(control.db_range(), percent);
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

fn activate_part_interface<T: Interface>(part: &IPart) -> WinResult<T> {
    let mut raw = std::ptr::null_mut();
    // SAFETY: part is a valid IPart; on success Activate writes an owned pointer implementing
    // T::IID into raw, and Type::from_abi takes ownership of exactly that pointer.
    unsafe {
        part.Activate(CLSCTX_ALL.0 as u32, &T::IID, Some(&mut raw))?;
        Type::from_abi(raw)
    }
}

/// Error returned when the QuadCast S is not plugged in.
///
/// Every control in this module acts on the QuadCast S specifically. When it is absent the
/// honest answer is a failure, not a silent redirect: falling back to whatever endpoint
/// Windows promoted to default meant the mute button, the volume slider and the reported
/// status all quietly started driving an unrelated microphone.
fn quadcast_not_connected() -> Error {
    Error::new(
        HRESULT(0x80070490u32 as i32),
        "HyperX QuadCast S is not connected",
    )
}

fn hyperx_render_device() -> WinResult<IMMDevice> {
    hyperx_audio_device(EndpointFlow::Render)
}

/// The QuadCast S capture endpoint.
fn hyperx_capture_device() -> WinResult<IMMDevice> {
    hyperx_audio_device(EndpointFlow::Capture)
}

fn hyperx_audio_device(flow: EndpointFlow) -> WinResult<IMMDevice> {
    let enumerator = device_enumerator()?;
    find_hyperx_endpoint(&enumerator, flow)?
        .map(|(device, _)| device)
        .ok_or_else(quadcast_not_connected)
}

/// Locate the QuadCast S endpoint for `flow`, considering only endpoints Windows reports
/// as active.
///
/// Unplugged endpoints linger in the enumerator with `DEVICE_STATE_UNPLUGGED` for as long
/// as the driver stays installed, so matching over `DEVICE_STATEMASK_ALL` would keep
/// returning a dead endpoint after the USB cable is pulled and every control write against
/// it would fail.
fn find_hyperx_endpoint(
    enumerator: &IMMDeviceEnumerator,
    flow: EndpointFlow,
) -> WinResult<Option<(IMMDevice, DeviceInfo)>> {
    // SAFETY: enumerator is a valid IMMDeviceEnumerator borrowed from the caller; the
    // returned collection is an owned IMMDeviceCollection.
    let collection =
        unsafe { enumerator.EnumAudioEndpoints(data_flow(flow), DEVICE_STATE_ACTIVE)? };
    // SAFETY: collection is the live IMMDeviceCollection obtained above.
    let count = unsafe { collection.GetCount()? };
    for index in 0..count {
        // SAFETY: index < count reported by GetCount on this same collection.
        let device = unsafe { collection.Item(index)? };
        let name = device_name(&device).unwrap_or_default();
        if is_quadcast_device_name(&name) {
            let info = describe_device(&device)?;
            return Ok(Some((device, info)));
        }
    }
    Ok(None)
}

/// Describe the active QuadCast S endpoint for `flow`, or `None` when it is not connected.
pub(crate) fn hyperx_endpoint_info(flow: EndpointFlow) -> WinResult<Option<DeviceInfo>> {
    let enumerator = device_enumerator()?;
    Ok(find_hyperx_endpoint(&enumerator, flow)?.map(|(_, info)| info))
}

pub(super) fn print_audio_topology(flow: EndpointFlow) -> WinResult<()> {
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

fn find_topology_part(topology: &IDeviceTopology, id: u32) -> WinResult<Option<IPart>> {
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
    part: &IPart,
    id: u32,
    visited: &mut Vec<u32>,
) -> WinResult<Option<IPart>> {
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

fn print_topology_part(part: &IPart, depth: usize, visited: &mut Vec<u32>) -> WinResult<()> {
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
    let device = hyperx_capture_device()?;
    let meter = endpoint_meter(&device)?;
    // SAFETY: meter is a live IAudioMeterInformation activated for the QuadCast S capture
    // endpoint; GetPeakValue only writes its f32 out-param.
    unsafe { meter.GetPeakValue() }
}

pub(crate) fn mic_status() -> WinResult<MicStatus> {
    let device = hyperx_capture_device()?;
    let enumerator = device_enumerator()?;
    let mut info = describe_device(&device)?;
    info.is_default = default_capture_device_with(&enumerator)
        .and_then(|default| device_id_string(&default))
        .is_ok_and(|default_id| default_id == info.id);

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
