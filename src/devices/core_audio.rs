//! Device-list notifications from the CoreAudio HAL.

use std::{ffi::c_void, ptr::NonNull, sync::mpsc::Sender};

use objc2_core_audio::{
    AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectRemovePropertyListener, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
};

use crate::logging::log_event;

const SYSTEM_OBJECT: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

/// The system object's device list: changes whenever any audio device appears or vanishes.
const DEVICE_LIST: AudioObjectPropertyAddress = AudioObjectPropertyAddress {
    mSelector: kAudioHardwarePropertyDevices,
    mScope: kAudioObjectPropertyScopeGlobal,
    mElement: kAudioObjectPropertyElementMain,
};

/// CoreAudio needs no per-thread setup before the watcher enumerates devices.
pub(super) fn prepare_watcher_thread() -> Option<()> {
    Some(())
}

/// Keeps the device-list listener registered for as long as it is held.
pub(super) struct EndpointNotifications {
    wake: &'static Sender<()>,
}

impl EndpointNotifications {
    pub(super) fn register(wake: Sender<()>) -> Result<Self, String> {
        // Leaked on purpose. CoreAudio does not promise that a callback already running on
        // its notification thread has returned when the listener is removed, so the sender
        // the callback dereferences must stay valid for the rest of the process. The GUI
        // starts one watcher, so this is a single small allocation.
        let wake: &'static Sender<()> = Box::leak(Box::new(wake));
        // SAFETY: DEVICE_LIST is a valid address borrowed for the call, and the client data
        // points at a 'static Sender, which is what `device_list_changed` expects.
        let status = unsafe {
            AudioObjectAddPropertyListener(
                SYSTEM_OBJECT,
                NonNull::from(&DEVICE_LIST),
                Some(device_list_changed),
                client_data(wake),
            )
        };
        if status != 0 {
            return Err(format!(
                "AudioObjectAddPropertyListener failed: OSStatus {status}"
            ));
        }
        Ok(Self { wake })
    }
}

impl Drop for EndpointNotifications {
    fn drop(&mut self) {
        // SAFETY: removes exactly the (address, proc, client data) triple added in
        // `register`; the client data stays valid regardless, see there.
        let status = unsafe {
            AudioObjectRemovePropertyListener(
                SYSTEM_OBJECT,
                NonNull::from(&DEVICE_LIST),
                Some(device_list_changed),
                client_data(self.wake),
            )
        };
        if status != 0 {
            log_event(
                "warn",
                "devices.watch.notifications.unregister.error",
                &[("message", format!("OSStatus {status}"))],
            );
        }
    }
}

fn client_data(wake: &'static Sender<()>) -> *mut c_void {
    (wake as *const Sender<()>).cast_mut().cast()
}

/// Nudges the watcher whenever the HAL reports a device-list change.
///
/// Runs on a HAL notification thread, so it does no work beyond signalling; all
/// enumeration happens back on the watcher thread.
unsafe extern "C-unwind" fn device_list_changed(
    _object: AudioObjectID,
    _address_count: u32,
    _addresses: NonNull<AudioObjectPropertyAddress>,
    client_data: *mut c_void,
) -> i32 {
    // SAFETY: the only client data ever registered with this proc is a leaked
    // `&'static Sender<()>` (see `register`), and `Sender` is `Sync`.
    let wake = unsafe { &*client_data.cast::<Sender<()>>() };
    // A dropped watcher is not worth reporting: it is about to unregister.
    let _ = wake.send(());
    0
}
