//! Lighting reports as raw USB control transfers through IOUSBHost.
//!
//! The controller's interface 0 is HID class, but macOS's HID driver does not stay attached
//! to it, which leaves the interface unowned. A user-space client may open an unowned
//! interface, so this sends NGENUITY's `SET_REPORT` / `GET_REPORT` requests to it directly.

use std::{
    ptr::NonNull,
    sync::{Arc, Mutex, MutexGuard, PoisonError, Weak},
};

use objc2::{AllocAnyThread, rc::Retained};
use objc2_core_foundation::{CFDictionary, CFRetained};
use objc2_foundation::{NSMutableData, NSNumber};
use objc2_io_kit::{
    IOObjectRelease, IOServiceGetMatchingService, IOUSBDeviceRequest, kIOMainPortDefault,
};
use objc2_io_usb_host::{IOUSBHostInterface, IOUSBHostObjectInitOptions};

use super::{REPORT_LEN, log_packet_attempt};
use crate::lighting::{LightingError, SUPPORTED_LIGHTING_IDS};

/// The interface NGENUITY addresses (`wIndex = 0` in the captures).
const LIGHTING_INTERFACE: u8 = 0;
/// The controller has a single configuration. IOUSBHost interface matching needs it
/// alongside vendor, product and interface number.
const CONFIGURATION: u8 = 1;
/// Class request to an interface, host-to-device (`0x21`) and device-to-host (`0xa1`).
const REQUEST_TYPE_CLASS_INTERFACE_OUT: u8 = 0x21;
const REQUEST_TYPE_CLASS_INTERFACE_IN: u8 = 0xa1;
const HID_GET_REPORT: u8 = 0x01;
const HID_SET_REPORT: u8 = 0x09;
/// Report type 3 (feature) in the high byte, report ID 0 in the low byte.
const FEATURE_REPORT_ID_0: u16 = 0x0300;
/// Longer than any healthy transfer takes; bounds the wait when the mic is unplugged
/// mid-request.
const TRANSFER_TIMEOUT_SECONDS: f64 = 1.0;

/// The opened interface, shared by every port in the process.
///
/// IOUSBHost grants one client per interface, while the GUI can briefly hold two ports: a
/// new lighting stream starts before the old one has noticed its cancel flag, and Save to
/// Mic runs alongside a live stream. hidapi on Windows simply opens the device twice;
/// sharing one handle is the macOS equivalent.
static SHARED_INTERFACE: Mutex<Weak<SharedInterface>> = Mutex::new(Weak::new());

struct SharedInterface {
    /// The lock also serializes transfers from concurrent writers.
    interface: Mutex<Retained<IOUSBHostInterface>>,
}

// SAFETY: IOUSBHost objects are not tied to the thread that created them (they run their
// own dispatch queue), every use of the interface goes through the mutex, and `destroy`
// runs once, in Drop, after the last reference is gone.
unsafe impl Send for SharedInterface {}
// SAFETY: see above; `&SharedInterface` only exposes the mutex.
unsafe impl Sync for SharedInterface {}

impl Drop for SharedInterface {
    fn drop(&mut self) {
        let interface = self
            .interface
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        // SAFETY: releases the user client opened in `open_interface`; nothing can use the
        // interface afterwards because this is its last owner.
        unsafe { interface.destroy() };
    }
}

pub(crate) struct LightingPort {
    /// Always `Some` until `drop` takes it.
    shared: Option<Arc<SharedInterface>>,
}

impl Drop for LightingPort {
    fn drop(&mut self) {
        // Let go under the registry lock. If this is the last port, the interface is
        // destroyed before any `open` can look for it again; otherwise an `open` landing
        // between the last release and `destroy` would find no shared interface yet be
        // refused a new one, because the old client is still closing.
        let _slot = shared_interface_slot();
        drop(self.shared.take());
    }
}

fn shared_interface_slot() -> MutexGuard<'static, Weak<SharedInterface>> {
    SHARED_INTERFACE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

impl LightingPort {
    /// Open the connected controller's lighting interface, or join the open one.
    pub(crate) fn open() -> Result<Self, LightingError> {
        let mut slot = shared_interface_slot();
        let shared = match slot.upgrade() {
            Some(shared) => shared,
            None => {
                let shared = Arc::new(SharedInterface {
                    interface: Mutex::new(open_controller_interface()?),
                });
                *slot = Arc::downgrade(&shared);
                shared
            }
        };
        Ok(Self {
            shared: Some(shared),
        })
    }

    pub(crate) fn send_feature(
        &self,
        packet: &[u8; REPORT_LEN],
        packet_log: bool,
    ) -> Result<(), LightingError> {
        let request = feature_request(REQUEST_TYPE_CLASS_INTERFACE_OUT, HID_SET_REPORT);
        let data = NSMutableData::with_bytes(packet);
        let result = self.transfer(request, &data).map(|_| ());
        log_packet_attempt(
            packet_log,
            "usb-set-report",
            result.is_ok(),
            packet,
            result.as_ref().err().cloned(),
        );
        result.map_err(LightingError::Hid)
    }

    pub(crate) fn read_feature(&self, packet_log: bool) -> Result<[u8; REPORT_LEN], String> {
        let request = feature_request(REQUEST_TYPE_CLASS_INTERFACE_IN, HID_GET_REPORT);
        let data = NSMutableData::with_bytes(&[0u8; REPORT_LEN]);
        let transferred = self.transfer(request, &data)?;
        let packet: [u8; REPORT_LEN] = data
            .to_vec()
            .get(..REPORT_LEN)
            .filter(|_| transferred >= REPORT_LEN)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| format!("Short feature report: {transferred} bytes"))?;
        log_packet_attempt(packet_log, "usb-get-report", true, &packet, None);
        Ok(packet)
    }

    /// Run one control transfer and return the number of bytes moved.
    fn transfer(&self, request: IOUSBDeviceRequest, data: &NSMutableData) -> Result<usize, String> {
        let interface = self.lock();
        let mut transferred = 0usize;
        // SAFETY: the interface is open (destroy only runs in Drop), `data` is wLength bytes
        // long, and `transferred` is a live out-param for the synchronous call.
        unsafe {
            interface.sendDeviceRequest_data_bytesTransferred_completionTimeout_error(
                request,
                Some(data),
                &mut transferred,
                TRANSFER_TIMEOUT_SECONDS,
            )
        }
        .map(|()| transferred)
        .map_err(|error| format!("USB control transfer failed: {error}"))
    }

    fn lock(&self) -> MutexGuard<'_, Retained<IOUSBHostInterface>> {
        self.shared
            .as_ref()
            .expect("a port holds its interface until it is dropped")
            .interface
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

fn feature_request(request_type: u8, request: u8) -> IOUSBDeviceRequest {
    IOUSBDeviceRequest {
        bmRequestType: request_type,
        bRequest: request,
        wValue: FEATURE_REPORT_ID_0,
        wIndex: u16::from(LIGHTING_INTERFACE),
        wLength: REPORT_LEN as u16,
    }
}

/// Open the lighting interface of the first supported controller that is plugged in.
fn open_controller_interface() -> Result<Retained<IOUSBHostInterface>, LightingError> {
    let mut last_error = None;
    for &(vendor_id, product_id) in &SUPPORTED_LIGHTING_IDS {
        let Some(service) = find_interface_service(vendor_id, product_id) else {
            continue;
        };
        let mut error = None;
        // SAFETY: `service` is a live io_service_t from IOServiceGetMatchingService; the
        // object retains what it needs, so the reference is released right after. `error`
        // is a live out-param, and no dispatch queue or interest handler is passed, both of
        // which are optional.
        let interface = unsafe {
            IOUSBHostInterface::initWithIOService_options_queue_error_interestHandler(
                IOUSBHostInterface::alloc(),
                service,
                IOUSBHostObjectInitOptions::None,
                None,
                Some(&mut error),
                std::ptr::null_mut(),
            )
        };
        IOObjectRelease(service);
        match interface {
            Some(interface) => return Ok(interface),
            None => {
                // The usual cause is another client holding the interface.
                let reason = error.map_or_else(|| "unknown error".to_string(), |e| e.to_string());
                last_error = Some(format!(
                    "Cannot open the {vendor_id:04x}:{product_id:04x} lighting interface \
                     ({reason}). Is NGENUITY or a second copy of this app running?"
                ))
            }
        }
    }
    Err(last_error.map_or(LightingError::NoDevice, LightingError::Hid))
}

/// Look up the controller's lighting interface in the IORegistry.
fn find_interface_service(vendor_id: u16, product_id: u16) -> Option<u32> {
    // SAFETY: every argument is either None or a live NSNumber, which is what the matching
    // dictionary builder accepts.
    let matching = unsafe {
        IOUSBHostInterface::createMatchingDictionaryWithVendorID_productID_bcdDevice_interfaceNumber_configurationValue_interfaceClass_interfaceSubclass_interfaceProtocol_speed_productIDArray(
            Some(&NSNumber::new_u16(vendor_id)),
            Some(&NSNumber::new_u16(product_id)),
            None,
            Some(&NSNumber::new_u8(LIGHTING_INTERFACE)),
            Some(&NSNumber::new_u8(CONFIGURATION)),
            None,
            None,
            None,
            None,
            None,
        )
    };
    // Two references are ours here: the builder follows the "create" rule and returns +1,
    // but its binding declares no method family, so objc2 retains the result once more
    // (measured: retain count 2 while the `Retained` is held). IOServiceGetMatchingService
    // consumes one; the other is released before the call.
    let matching = NonNull::new(Retained::into_raw(matching).cast::<CFDictionary>())?;
    // SAFETY: each `from_raw` takes over exactly one of the two owned references above.
    let (consumed, surplus) = unsafe {
        (
            CFRetained::from_raw(matching),
            CFRetained::from_raw(matching),
        )
    };
    drop(surplus);
    // SAFETY: kIOMainPortDefault is the documented default main port.
    let service = unsafe { IOServiceGetMatchingService(kIOMainPortDefault, Some(consumed)) };
    (service != 0).then_some(service)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_match_the_ngenuity_capture() {
        // Setup bytes from captures/1-color-analysis.md: 21 09 00 03 00 00 40 00.
        let set = feature_request(REQUEST_TYPE_CLASS_INTERFACE_OUT, HID_SET_REPORT);
        assert_eq!(
            (
                set.bmRequestType,
                set.bRequest,
                set.wValue,
                set.wIndex,
                set.wLength
            ),
            (0x21, 0x09, 0x0300, 0, 64)
        );
    }

    #[test]
    fn readback_is_the_matching_get_report() {
        let get = feature_request(REQUEST_TYPE_CLASS_INTERFACE_IN, HID_GET_REPORT);
        assert_eq!(
            (
                get.bmRequestType,
                get.bRequest,
                get.wValue,
                get.wIndex,
                get.wLength
            ),
            (0xa1, 0x01, 0x0300, 0, 64)
        );
    }
}
