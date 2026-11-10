//! Tracks whether the QuadCast S is plugged into USB.
//!
//! Presence used to be sampled once at GUI startup, so connecting or disconnecting the mic
//! while the app was running left every consumer (status, lighting HID, capture meter, MQTT)
//! pointing at a device that was no longer there. This module owns presence instead and
//! publishes transitions to the GUI.
//!
//! Detection is event-driven where the OS offers events: Windows fires
//! `IMMNotificationClient` and macOS a CoreAudio property listener as soon as an audio
//! endpoint is added, removed, or restated. Both are backed by a slow re-enumeration timer
//! for two reasons — the HID lighting interface has no equivalent notification, and a
//! missed or unregistered callback must not leave the UI stuck.

use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
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

use crate::{
    audio::{EndpointFlow, hyperx_endpoint_info},
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
}

impl DeviceSnapshot {
    /// Whether the microphone itself is plugged in.
    ///
    /// Keyed on the capture endpoint rather than the HID interface: NGENUITY can hold the
    /// HID exclusively while the mic is perfectly connected and usable.
    pub(crate) fn connected(&self) -> bool {
        self.capture.is_some()
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
    let Some(_thread) = backend::prepare_watcher_thread() else {
        return;
    };

    let (wake_sender, wake_receiver) = mpsc::channel();
    // Kept alive for the life of the loop so `recv_timeout` reports Timeout rather than
    // Disconnected when registration failed and nothing else holds a sender.
    let _wake_sender = wake_sender.clone();
    let _registration = match backend::EndpointNotifications::register(wake_sender) {
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

/// Sample QuadCast S presence.
///
/// Every field is scoped to the QuadCast S. Nothing here describes any other microphone, so
/// plugging a webcam or a second USB mic in or out leaves the snapshot byte-identical and
/// raises no event — the app has no business reacting to hardware it does not control.
fn probe_devices() -> DeviceSnapshot {
    DeviceSnapshot {
        capture: hyperx_endpoint_info(EndpointFlow::Capture).unwrap_or_default(),
        render: hyperx_endpoint_info(EndpointFlow::Render).unwrap_or_default(),
        lighting: detect_lighting_device(),
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
        }
    }

    fn lighting_device() -> LightingDevice {
        LightingDevice {
            vendor_id: 0x0951,
            product_id: 0x171f,
            interface_number: 0,
            usage_page: 0xff90,
            usage: 0xff00,
            manufacturer: "HyperX".to_string(),
            product: "QuadCast S".to_string(),
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
    fn lighting_interface_being_claimed_reports_changed() {
        let mut current = connected_snapshot();
        current.lighting = None;
        let mut previous = connected_snapshot();
        previous.lighting = Some(lighting_device());
        let event = classify_transition(Some(&previous), &current);
        assert!(matches!(event, Some(DeviceEvent::Changed(_))));
    }

    #[test]
    fn lighting_interface_appearing_reports_changed() {
        let mut current = connected_snapshot();
        current.lighting = Some(lighting_device());
        let event = classify_transition(Some(&connected_snapshot()), &current);
        assert!(matches!(event, Some(DeviceEvent::Changed(_))));
    }

    #[test]
    fn presence_is_keyed_on_the_capture_endpoint_alone() {
        // NGENUITY can hold the HID exclusively while the mic works fine, and the HID can
        // linger a moment after the cable is pulled; only an active capture endpoint counts.
        let claimed_hid = DeviceSnapshot {
            capture: Some(endpoint("Microphone (HyperX QuadCast S)")),
            render: Some(endpoint("Headphones (HyperX QuadCast S)")),
            lighting: None,
        };
        assert!(claimed_hid.connected());

        let lingering_hid = DeviceSnapshot {
            capture: None,
            render: None,
            lighting: Some(lighting_device()),
        };
        assert!(!lingering_hid.connected());
        assert!(matches!(
            classify_transition(Some(&connected_snapshot()), &lingering_hid),
            Some(DeviceEvent::Disconnected(_))
        ));
    }
}
