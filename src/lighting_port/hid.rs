//! Lighting reports through hidapi, which reaches the controller's HID collection directly.

use super::{REPORT_LEN, log_packet_attempt};
use crate::lighting::{LightingError, is_supported_lighting_device, lighting_device_score};

pub(crate) struct LightingPort {
    device: hidapi::HidDevice,
}

impl LightingPort {
    /// Open the best-scoring lighting interface of the connected controller.
    pub(crate) fn open() -> Result<Self, LightingError> {
        let api = hidapi::HidApi::new().map_err(|error| LightingError::Hid(error.to_string()))?;
        let info = api
            .device_list()
            .filter(|device| is_supported_lighting_device(device))
            .max_by_key(|device| lighting_device_score(device))
            .ok_or(LightingError::NoDevice)?;
        let device = api
            .open_path(info.path())
            .map_err(|error| LightingError::Hid(error.to_string()))?;
        Ok(Self { device })
    }

    /// Send one report, trying each transport the controller has been seen to accept.
    pub(crate) fn send_feature(
        &self,
        packet: &[u8; REPORT_LEN],
        packet_log: bool,
    ) -> Result<(), LightingError> {
        let device = &self.device;
        let mut with_report_id = [0u8; REPORT_LEN + 1];
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
                    log_packet_attempt(packet_log, name, true, packet, None);
                    return Ok(());
                }
                Err(error) => {
                    log_packet_attempt(packet_log, name, false, packet, Some(error.to_string()));
                    errors.push(format!("{name}: {error}"));
                }
            }
        }

        Err(LightingError::Hid(errors.join("; ")))
    }

    pub(crate) fn read_feature(&self, packet_log: bool) -> Result<[u8; REPORT_LEN], String> {
        let device = &self.device;
        let mut with_report_id = [0u8; REPORT_LEN + 1];
        let mut errors = Vec::new();
        match device.get_feature_report(&mut with_report_id) {
            Ok(length) if length > REPORT_LEN => {
                let mut packet = [0u8; REPORT_LEN];
                packet.copy_from_slice(&with_report_id[1..]);
                log_packet_attempt(packet_log, "feature-read+id", true, &packet, None);
                return Ok(packet);
            }
            Ok(length) => errors.push(format!("feature-read+id: short report length {length}")),
            Err(error) => errors.push(format!("feature-read+id: {error}")),
        }

        let mut packet = [0u8; REPORT_LEN];
        match device.get_feature_report(&mut packet) {
            Ok(length) if length >= REPORT_LEN => {
                log_packet_attempt(packet_log, "feature-read", true, &packet, None);
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
}
