//! Reacquires everything that is bound to the physical mic when it is plugged or unplugged.
//!
//! The capture stream, the lighting HID handles and the reported status are all tied to a
//! device that can vanish mid-session. `DeviceWatcher` decides *when* that happened; this
//! module decides what the GUI does about it.

use std::sync::atomic::Ordering;

use crate::{
    audio::start_audio_peak_monitor_with,
    devices::{DeviceEvent, DeviceSnapshot},
    logging::log_event,
};

use super::{MicLiteApp, NoticeSeverity, UiNotice};

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

    fn restart_input_monitor(&mut self) {
        self.stop_input_monitor();
        match start_audio_peak_monitor_with(self.input_peak_bits.clone()) {
            Ok(monitor) => {
                self.input_monitor = Some(monitor);
                log_event("info", "gui.audio.capture.restart", &[]);
            }
            Err(error) => log_event(
                "warn",
                "gui.audio.capture.restart.error",
                &[("message", error)],
            ),
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
