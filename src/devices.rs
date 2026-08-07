//! Tracks whether the QuadCast S is plugged into USB.
//!
//! Presence used to be sampled once at GUI startup, so connecting or disconnecting the mic
//! while the app was running left every consumer (status, lighting HID, capture meter, MQTT)
//! pointing at a device that was no longer there. This module owns presence instead and
//! publishes transitions to the GUI.
//!
//! Detection is event-driven where Windows offers events: `IMMNotificationClient` fires as
//! soon as the audio service adds, removes, or restates an endpoint. It is backed by a slow
//! re-enumeration timer for two reasons — the HID lighting interface has no equivalent
//! notification, and a missed or unregistered callback must not leave the UI stuck.

use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::Duration,
};

use windows::{
    Win32::{
        Foundation::PROPERTYKEY,
        Media::Audio::{
            DEVICE_STATE, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
            IMMNotificationClient_Impl, MMDeviceEnumerator, eCapture, eRender,
        },
        System::Com::{CLSCTX_ALL, CoCreateInstance},
    },
    core::{PCWSTR, Result as WinResult, implement},
};

use crate::{
    audio::{default_capture_info, hyperx_endpoint_info, is_quadcast_device_name},
    com::ComApartment,
    lighting::detect_lighting_device,
    logging::log_event,
    model::{DeviceInfo, LightingDevice},
};

/// Backstop re-enumeration cadence when no endpoint notification arrives.
const POLL_INTERVAL: Duration = Duration::from_millis(1500);

/// Grace period after a notification before re-enumerating.
///
/// A single plug event produces a burst of callbacks (capture endpoint, render endpoint,
/// default-device change) and the HID interface is published slightly after the audio
/// endpoints. Waiting briefly collapses the burst into one probe that sees the whole device.
const SETTLE_DELAY: Duration = Duration::from_millis(250);

/// Everything the app needs to know about QuadCast S presence at one instant.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct DeviceSnapshot {
    /// The QuadCast S capture endpoint, present only while Windows reports it active.
    pub(crate) capture: Option<DeviceInfo>,
    /// The QuadCast S headphone output endpoint.
    pub(crate) render: Option<DeviceInfo>,
    /// The lighting HID interface, absent when unplugged or claimed by NGENUITY.
    pub(crate) lighting: Option<LightingDevice>,
    /// Whichever endpoint the app's mute/volume controls currently act on.
    pub(crate) default_capture: Option<DeviceInfo>,
}

impl DeviceSnapshot {
    /// Whether the microphone itself is plugged in.
    ///
    /// Keyed on the capture endpoint rather than the HID interface: NGENUITY can hold the
    /// HID exclusively while the mic is perfectly connected and usable.
    pub(crate) fn connected(&self) -> bool {
        self.capture.is_some()
    }

    /// Whether Windows currently routes capture to the QuadCast S.
    pub(crate) fn default_is_quadcast(&self) -> bool {
        self.default_capture
            .as_ref()
            .is_some_and(|info| is_quadcast_device_name(&info.name))
    }

    /// Name of the endpoint the controls act on, for display.
    pub(crate) fn default_capture_name(&self) -> Option<&str> {
        self.default_capture.as_ref().map(|info| info.name.as_str())
    }

    fn log_fields(&self) -> Vec<(&'static str, String)> {
        vec![
            ("connected", self.connected().to_string()),
            (
                "capture",
                self.capture
                    .as_ref()
                    .map(|info| info.name.clone())
                    .unwrap_or_else(|| "none".to_string()),
            ),
            ("render", self.render.is_some().to_string()),
            ("lighting", self.lighting.is_some().to_string()),
            (
                "default_capture",
                self.default_capture_name().unwrap_or("none").to_string(),
            ),
        ]
    }
}

/// A change in QuadCast S presence.
#[derive(Clone, PartialEq)]
pub(crate) enum DeviceEvent {
    /// The microphone appeared (or was already present at startup).
    Connected(DeviceSnapshot),
    /// The microphone went away (or was absent at startup).
    Disconnected(DeviceSnapshot),
    /// Still present, but something about the device set changed — the default endpoint
    /// moved, the lighting interface appeared or was claimed, or an endpoint restated.
    Changed(DeviceSnapshot),
}

impl DeviceEvent {
    pub(crate) fn snapshot(&self) -> &DeviceSnapshot {
        match self {
            Self::Connected(snapshot) | Self::Disconnected(snapshot) | Self::Changed(snapshot) => {
                snapshot
            }
        }
    }
}

/// Classify one probe against the previous one.
///
/// `previous` is `None` only for the first probe, which always reports so the GUI starts
/// from observed hardware rather than assuming the mic is there.
fn classify_transition(
    previous: Option<&DeviceSnapshot>,
    current: &DeviceSnapshot,
) -> Option<DeviceEvent> {
    let Some(previous) = previous else {
        return Some(if current.connected() {
            DeviceEvent::Connected(current.clone())
        } else {
            DeviceEvent::Disconnected(current.clone())
        });
    };
    if previous == current {
        return None;
    }
    Some(match (previous.connected(), current.connected()) {
        (false, true) => DeviceEvent::Connected(current.clone()),
        (true, false) => DeviceEvent::Disconnected(current.clone()),
        _ => DeviceEvent::Changed(current.clone()),
    })
}

/// Handle on the background watcher. Dropping it stops the watcher thread.
pub(crate) struct DeviceWatcher {
    events: Receiver<DeviceEvent>,
}

impl DeviceWatcher {
    pub(crate) fn start() -> Self {
        let (sender, events) = mpsc::channel();
        thread::spawn(move || watch_devices(sender));
        Self { events }
    }

    /// Take the next observed transition, if any. Never blocks.
    pub(crate) fn try_recv(&self) -> Option<DeviceEvent> {
        self.events.try_recv().ok()
    }
}

fn watch_devices(sender: Sender<DeviceEvent>) {
    // The watcher owns COM interfaces on this thread only; see `init_multithreaded` for why
    // it must not be an STA.
    let _com = match ComApartment::init_multithreaded() {
        Ok(com) => com,
        Err(error) => {
            log_event(
                "error",
                "devices.watch.com_init.error",
                &[("message", error.to_string())],
            );
            return;
        }
    };

    let (wake_sender, wake_receiver) = mpsc::channel();
    // Kept alive for the life of the loop so `recv_timeout` reports Timeout rather than
    // Disconnected when registration failed and nothing else holds a sender.
    let _wake_sender = wake_sender.clone();
    let _registration = match EndpointNotifications::register(wake_sender) {
        Ok(registration) => {
            log_event("info", "devices.watch.notifications.registered", &[]);
            Some(registration)
        }
        Err(error) => {
            // Not fatal: the poll below still detects hotplug, just up to POLL_INTERVAL later.
            log_event(
                "warn",
                "devices.watch.notifications.error",
                &[("message", error.to_string())],
            );
            None
        }
    };

    let mut previous: Option<DeviceSnapshot> = None;
    loop {
        let current = probe_devices();
        if let Some(event) = classify_transition(previous.as_ref(), &current) {
            log_event(
                match event {
                    DeviceEvent::Connected(_) => "info",
                    DeviceEvent::Disconnected(_) => "warn",
                    DeviceEvent::Changed(_) => "info",
                },
                match event {
                    DeviceEvent::Connected(_) => "devices.connected",
                    DeviceEvent::Disconnected(_) => "devices.disconnected",
                    DeviceEvent::Changed(_) => "devices.changed",
                },
                &current.log_fields(),
            );
            if sender.send(event).is_err() {
                log_event("info", "devices.watch.stop", &[]);
                return;
            }
        }
        previous = Some(current);

        match wake_receiver.recv_timeout(POLL_INTERVAL) {
            Ok(()) => {
                thread::sleep(SETTLE_DELAY);
                // Drain the rest of the burst so one plug does not cause several probes.
                while wake_receiver.try_recv().is_ok() {}
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => thread::sleep(POLL_INTERVAL),
        }
    }
}

/// Read current presence on the calling thread.
///
/// The GUI uses this once at startup so its first frame already reflects real hardware
/// instead of flashing "disconnected" until the watcher's first event lands.
pub(crate) fn probe() -> DeviceSnapshot {
    probe_devices()
}

fn probe_devices() -> DeviceSnapshot {
    DeviceSnapshot {
        capture: hyperx_endpoint_info(eCapture).unwrap_or_default(),
        render: hyperx_endpoint_info(eRender).unwrap_or_default(),
        lighting: detect_lighting_device(),
        default_capture: default_capture_info(),
    }
}

/// Keeps the endpoint notification callback registered for as long as it is held.
struct EndpointNotifications {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl EndpointNotifications {
    fn register(wake: Sender<()>) -> WinResult<Self> {
        // SAFETY: MMDeviceEnumerator is the well-known CLSID for this coclass, COM is
        // initialized on this thread by the caller, and no aggregation is used (None).
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let client: IMMNotificationClient = EndpointNotifier { wake }.into();
        // SAFETY: enumerator is the live interface created above; `client` is an owned
        // reference held in the returned struct, so it outlives the registration, which is
        // undone in Drop before the reference is released.
        unsafe { enumerator.RegisterEndpointNotificationCallback(&client)? };
        Ok(Self { enumerator, client })
    }
}

impl Drop for EndpointNotifications {
    fn drop(&mut self) {
        // SAFETY: unregisters exactly the client registered in `register` on the same live
        // enumerator; both are still alive at this point.
        let result = unsafe {
            self.enumerator
                .UnregisterEndpointNotificationCallback(&self.client)
        };
        if let Err(error) = result {
            log_event(
                "warn",
                "devices.watch.notifications.unregister.error",
                &[("message", error.to_string())],
            );
        }
    }
}

/// Nudges the watcher whenever the audio service reports an endpoint change.
///
/// The callbacks run on a thread owned by the audio service, so they do no work beyond
/// signalling; all enumeration happens back on the watcher thread.
#[implement(IMMNotificationClient)]
struct EndpointNotifier {
    wake: Sender<()>,
}

// COM invokes the callbacks through `&self` from an audio-service thread while the watcher
// thread owns the other end of the channel, and the `IMMNotificationClient_Impl` trait does
// not require thread safety. Assert it here so a future field cannot silently remove it.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<EndpointNotifier>();
};

impl EndpointNotifier {
    fn poke(&self) {
        // A full channel or a dropped watcher is not worth reporting: the watcher is either
        // already about to re-probe or gone.
        let _ = self.wake.send(());
    }
}

impl IMMNotificationClient_Impl for EndpointNotifier_Impl {
    fn OnDeviceStateChanged(&self, _device_id: &PCWSTR, _new_state: DEVICE_STATE) -> WinResult<()> {
        self.poke();
        Ok(())
    }

    fn OnDeviceAdded(&self, _device_id: &PCWSTR) -> WinResult<()> {
        self.poke();
        Ok(())
    }

    fn OnDeviceRemoved(&self, _device_id: &PCWSTR) -> WinResult<()> {
        self.poke();
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        _flow: EDataFlow,
        _role: ERole,
        _default_device_id: &PCWSTR,
    ) -> WinResult<()> {
        self.poke();
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _device_id: &PCWSTR, _key: &PROPERTYKEY) -> WinResult<()> {
        // Deliberately ignored. Property churn (volume, format, icon) is frequent and never
        // signals arrival or removal; waking on it would re-enumerate constantly.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(name: &str) -> DeviceInfo {
        DeviceInfo {
            id: format!("id-{name}"),
            name: name.to_string(),
            state: "active".to_string(),
            is_default: false,
        }
    }

    fn connected_snapshot() -> DeviceSnapshot {
        DeviceSnapshot {
            capture: Some(endpoint("Microphone (HyperX QuadCast S)")),
            render: Some(endpoint("Headphones (HyperX QuadCast S)")),
            lighting: None,
            default_capture: Some(endpoint("Microphone (HyperX QuadCast S)")),
        }
    }

    #[test]
    fn first_probe_reports_connected_when_mic_is_present() {
        let event = classify_transition(None, &connected_snapshot());
        assert!(matches!(event, Some(DeviceEvent::Connected(_))));
    }

    #[test]
    fn first_probe_reports_disconnected_when_mic_is_absent() {
        let event = classify_transition(None, &DeviceSnapshot::default());
        assert!(matches!(event, Some(DeviceEvent::Disconnected(_))));
    }

    #[test]
    fn unchanged_snapshot_reports_nothing() {
        let snapshot = connected_snapshot();
        assert!(classify_transition(Some(&snapshot), &snapshot).is_none());
    }

    #[test]
    fn plugging_in_reports_connected() {
        let event = classify_transition(Some(&DeviceSnapshot::default()), &connected_snapshot());
        assert!(matches!(event, Some(DeviceEvent::Connected(_))));
    }

    #[test]
    fn unplugging_reports_disconnected() {
        let event = classify_transition(Some(&connected_snapshot()), &DeviceSnapshot::default());
        assert!(matches!(event, Some(DeviceEvent::Disconnected(_))));
    }

    #[test]
    fn default_endpoint_moving_away_reports_changed_not_disconnected() {
        let mut current = connected_snapshot();
        current.default_capture = Some(endpoint("Microphone (Webcam)"));
        let event = classify_transition(Some(&connected_snapshot()), &current);
        assert!(matches!(event, Some(DeviceEvent::Changed(_))));
    }

    #[test]
    fn lighting_interface_appearing_reports_changed() {
        let mut current = connected_snapshot();
        current.lighting = Some(LightingDevice {
            vendor_id: 0x0951,
            product_id: 0x171f,
            interface_number: 0,
            usage_page: 0xff90,
            usage: 0xff00,
            manufacturer: "HyperX".to_string(),
            product: "QuadCast S".to_string(),
        });
        let event = classify_transition(Some(&connected_snapshot()), &current);
        assert!(matches!(event, Some(DeviceEvent::Changed(_))));
    }

    #[test]
    fn unplugged_endpoint_left_as_default_is_not_treated_as_connected() {
        // Windows keeps a stale default pointing at the mic for a moment after removal; only
        // an active capture endpoint counts as connected.
        let snapshot = DeviceSnapshot {
            capture: None,
            render: None,
            lighting: None,
            default_capture: Some(endpoint("Microphone (HyperX QuadCast S)")),
        };
        assert!(!snapshot.connected());
        assert!(matches!(
            classify_transition(Some(&connected_snapshot()), &snapshot),
            Some(DeviceEvent::Disconnected(_))
        ));
    }
}
