//! The channel lighting packets travel over.
//!
//! NGENUITY drives the lighting controller with 64-byte HID feature reports sent as
//! `SET_REPORT` control transfers to interface 0 (see `captures/1-color-analysis.md`).
//! Windows exposes that interface as a HID collection, so hidapi reaches it. macOS does
//! not: its HID driver declines interface 0 and only publishes interface 1 (the button
//! events), so the macOS port sends the same control transfers through IOUSBHost instead.

#[cfg(windows)]
mod hid;
#[cfg(target_os = "macos")]
mod usb_host;

#[cfg(windows)]
pub(crate) use hid::LightingPort;
#[cfg(target_os = "macos")]
pub(crate) use usb_host::LightingPort;

use crate::{lighting::hex_bytes, logging::log_event};

/// Size of every lighting report, without a report ID.
pub(crate) const REPORT_LEN: usize = 64;

/// Record one packet transfer when `--packet-log` is on.
fn log_packet_attempt(
    enabled: bool,
    transport: &str,
    success: bool,
    packet: &[u8; REPORT_LEN],
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
