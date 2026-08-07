//! Reacquires everything that is bound to the physical mic when it is plugged or unplugged.
//!
//! The capture stream, the lighting HID handles and the reported status are all tied to a
//! device that can vanish mid-session. `DeviceWatcher` decides *when* that happened; this
//! module decides what the GUI does about it.

use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use crate::{
    audio::start_audio_peak_monitor_with,
    devices::{DeviceEvent, DeviceSnapshot},
    logging::log_event,
};

use super::{MicLiteApp, NoticeSeverity, UiNotice};

/// How long to wait before another attempt at opening the capture stream.
///
/// Windows publishes the capture endpoint slightly before it is ready to be activated, so the
/// attempt made the instant the mic reappears can legitimately fail. Retrying costs one WASAPI
/// activation every few seconds and only happens while the meter is actually down.
const INPUT_MONITOR_RETRY_INTERVAL: Duration = Duration::from_secs(3);

/// Whether the capture meter should be (re)opened on this tick.
///
/// Split out from the GUI so the policy is testable: the bug this exists to prevent is a
/// single failed attempt latching "Input meter unavailable" for the rest of the session.
fn should_start_input_monitor(
    mic_connected: bool,
    monitor_alive: bool,
    since_last_attempt: Duration,
) -> bool {
    mic_connected && !monitor_alive && since_last_attempt >= INPUT_MONITOR_RETRY_INTERVAL
}

impl MicLiteApp {
    pub(super) fn drain_device_events(&mut self) {
        while let Some(event) = self.device_watcher.try_recv() {
            self.apply_device_snapshot(event.snapshot().clone());
            match event {
                DeviceEvent::Connected(_) => self.on_device_connected(),
                DeviceEvent::Disconnected(_) => self.on_device_disconnected(),
                DeviceEvent::Changed(_) => self.refresh_status(),
            }
        }
    }

    fn apply_device_snapshot(&mut self, snapshot: DeviceSnapshot) {
        self.lighting_device = snapshot.lighting.clone();
        self.devices = snapshot;
        self.device_generation = self.device_generation.wrapping_add(1);
    }

    fn on_device_connected(&mut self) {
        log_event(
            "info",
            "gui.device.connected",
            &[(
                "device",
                self.devices
                    .capture
                    .as_ref()
                    .map(|info| info.name.clone())
                    .unwrap_or_else(|| "unknown".to_string()),
            )],
        );
        // Pull the device's real state rather than pushing the saved sliders at it: the
        // hardware keeps its own volume and mute across a replug, and the UI should agree
        // with the hardware.
        self.refresh_status();
        self.restart_input_monitor();
        self.resume_lighting_stream();
        self.lighting_notice = Some(UiNotice {
            severity: NoticeSeverity::Good,
            message: "QuadCast S reconnected.".to_string(),
        });
    }

    fn on_device_disconnected(&mut self) {
        log_event("warn", "gui.device.disconnected", &[]);
        // The HID handles behind the stream are dead; the worker would fail its next write
        // anyway, so cancel it deliberately and keep `lighting_stream_wanted` set so it can
        // come back on reconnect.
        if let Some(cancel) = self.lighting_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.stop_input_monitor();
        // The physical mute button's state is unknowable while the mic is gone; leaving it
        // latched would show a "Muted" the user cannot clear.
        self.hid_muted = false;
        self.refresh_status();
        self.lighting_notice = Some(UiNotice {
            severity: NoticeSeverity::Error,
            message: "QuadCast S disconnected.".to_string(),
        });
    }

    fn stop_input_monitor(&mut self) {
        // Dropping the monitor stops the cpal stream. Zero the shared cell too, otherwise a
        // VU Meter lighting stream would hold the last sample forever.
        self.input_monitor = None;
        self.input_peak_bits
            .store(0.0f32.to_bits(), Ordering::Relaxed);
        self.input_peak = 0.0;
    }

    /// Whether a capture stream is open and still delivering samples.
    pub(super) fn input_monitor_alive(&self) -> bool {
        self.input_monitor
            .as_ref()
            .is_some_and(|monitor| !monitor.failed())
    }

    /// Reopen the capture stream if it is down while the mic is present.
    ///
    /// Called every frame. `restart_input_monitor` used to run once per connect event, so a
    /// failure at that exact moment — the endpoint not yet activatable, or the stream dying
    /// later to a WASAPI error — left the meter dead with no path back.
    pub(super) fn ensure_input_monitor(&mut self) {
        if !should_start_input_monitor(
            self.devices.connected(),
            self.input_monitor_alive(),
            self.last_input_monitor_attempt.elapsed(),
        ) {
            return;
        }
        self.restart_input_monitor();
    }

    fn restart_input_monitor(&mut self) {
        self.stop_input_monitor();
        self.last_input_monitor_attempt = Instant::now();
        match start_audio_peak_monitor_with(self.input_peak_bits.clone()) {
            Ok(monitor) => {
                self.input_monitor = Some(monitor);
                self.input_monitor_error = None;
                log_event("info", "gui.audio.capture.restart", &[]);
            }
            Err(error) => {
                // Log only when the reason changes: the retry runs until it succeeds, and
                // repeating an identical line every few seconds buries everything else.
                if self.input_monitor_error.as_deref() != Some(error.as_str()) {
                    log_event(
                        "warn",
                        "gui.audio.capture.restart.error",
                        &[("message", error.clone())],
                    );
                }
                self.input_monitor_error = Some(error);
            }
        }
    }

    fn resume_lighting_stream(&mut self) {
        if !self.lighting_stream_wanted || self.lighting_device.is_none() {
            return;
        }
        log_event("info", "lighting.apply.resume", &[]);
        self.apply_lighting_to_microphone();
    }
}

#[cfg(test)]
mod tests {
    use super::{INPUT_MONITOR_RETRY_INTERVAL, should_start_input_monitor};
    use std::time::Duration;

    const LONG_AGO: Duration = Duration::from_secs(60);

    #[test]
    fn a_dead_monitor_is_reopened_while_the_mic_is_connected() {
        // The regression: the meter was opened once per connect event, so a failure during
        // the replug settle window reported "Input meter unavailable" until the app restarted.
        assert!(should_start_input_monitor(true, false, LONG_AGO));
    }

    #[test]
    fn a_live_monitor_is_left_alone() {
        assert!(!should_start_input_monitor(true, true, LONG_AGO));
    }

    #[test]
    fn nothing_is_opened_while_the_mic_is_unplugged() {
        assert!(!should_start_input_monitor(false, false, LONG_AGO));
    }

    #[test]
    fn retries_are_spaced_out() {
        assert!(!should_start_input_monitor(true, false, Duration::ZERO));
        assert!(!should_start_input_monitor(
            true,
            false,
            INPUT_MONITOR_RETRY_INTERVAL - Duration::from_millis(1)
        ));
        assert!(should_start_input_monitor(
            true,
            false,
            INPUT_MONITOR_RETRY_INTERVAL
        ));
    }
}
