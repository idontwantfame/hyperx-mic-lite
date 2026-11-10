//! HID report capabilities of a lighting interface, for the dump and diagnostics commands.

use std::ffi::CStr;

/// Report sizes and capability counts of one HID top-level collection.
pub(crate) struct HidReportCaps {
    pub(crate) input_report_bytes: u16,
    pub(crate) output_report_bytes: u16,
    pub(crate) feature_report_bytes: u16,
    pub(crate) input_button_caps: u16,
    pub(crate) input_value_caps: u16,
    pub(crate) output_button_caps: u16,
    pub(crate) output_value_caps: u16,
    pub(crate) feature_button_caps: u16,
    pub(crate) feature_value_caps: u16,
}

#[cfg(windows)]
pub(crate) fn hid_caps_for_path(path: &CStr) -> Result<HidReportCaps, String> {
    use windows::{
        Win32::{
            Devices::HumanInterfaceDevice::{
                HIDP_CAPS, HIDP_STATUS_SUCCESS, HidD_FreePreparsedData, HidD_GetPreparsedData,
                HidP_GetCaps, PHIDP_PREPARSED_DATA,
            },
            Foundation::CloseHandle,
            Storage::FileSystem::{
                CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, FILE_SHARE_READ,
                FILE_SHARE_WRITE, OPEN_EXISTING,
            },
        },
        core::PCWSTR,
    };

    let path = path.to_string_lossy();
    let wide_path = path.encode_utf16().chain([0]).collect::<Vec<_>>();
    // SAFETY: wide_path is a null-terminated UTF-16 buffer that outlives the call; access mask 0
    // with OPEN_EXISTING is valid for opening a HID device just to query its capabilities.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide_path.as_ptr()),
            0,
            FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0),
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|error| error.to_string())?;

    let mut preparsed: PHIDP_PREPARSED_DATA = PHIDP_PREPARSED_DATA::default();
    // SAFETY: handle is the valid HID device handle opened above; preparsed is only used after
    // HidD_GetPreparsedData returns TRUE and is freed exactly once, HidP_GetCaps writes only to
    // the local HIDP_CAPS out-param, and handle is closed on both the error and success paths.
    unsafe {
        if !HidD_GetPreparsedData(handle, &mut preparsed) {
            let _ = CloseHandle(handle);
            return Err("HidD_GetPreparsedData failed".to_string());
        }

        let mut caps = HIDP_CAPS::default();
        let status = HidP_GetCaps(preparsed, &mut caps);
        HidD_FreePreparsedData(preparsed);
        let _ = CloseHandle(handle);

        if status == HIDP_STATUS_SUCCESS {
            Ok(HidReportCaps {
                input_report_bytes: caps.InputReportByteLength,
                output_report_bytes: caps.OutputReportByteLength,
                feature_report_bytes: caps.FeatureReportByteLength,
                input_button_caps: caps.NumberInputButtonCaps,
                input_value_caps: caps.NumberInputValueCaps,
                output_button_caps: caps.NumberOutputButtonCaps,
                output_value_caps: caps.NumberOutputValueCaps,
                feature_button_caps: caps.NumberFeatureButtonCaps,
                feature_value_caps: caps.NumberFeatureValueCaps,
            })
        } else {
            Err(format!(
                "HidP_GetCaps failed with NTSTATUS 0x{:08x}",
                status.0
            ))
        }
    }
}

/// macOS has no preparsed-data API; the report sizes would have to be parsed out of the
/// report descriptor, which nothing here needs beyond the Windows dump output.
#[cfg(not(windows))]
pub(crate) fn hid_caps_for_path(_path: &CStr) -> Result<HidReportCaps, String> {
    Err("HID capability queries are only available on Windows".to_string())
}
