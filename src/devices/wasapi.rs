//! Endpoint notifications from the Windows audio service.

use std::sync::mpsc::Sender;

use windows::{
    Win32::{
        Foundation::PROPERTYKEY,
        Media::Audio::{
            DEVICE_STATE, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
            IMMNotificationClient_Impl, MMDeviceEnumerator,
        },
        System::Com::{CLSCTX_ALL, CoCreateInstance},
    },
    core::{PCWSTR, Result as WinResult, implement},
};

use crate::{com::ComApartment, logging::log_event};

/// Join the multithreaded apartment for the watcher thread.
///
/// The watcher owns COM interfaces on this thread only; see `init_multithreaded` for why
/// it must not be an STA. `None` means COM is unusable here and the watcher must stop.
pub(super) fn prepare_watcher_thread() -> Option<ComApartment> {
    match ComApartment::init_multithreaded() {
        Ok(com) => Some(com),
        Err(error) => {
            log_event(
                "error",
                "devices.watch.com_init.error",
                &[("message", error.to_string())],
            );
            None
        }
    }
}

/// Keeps the endpoint notification callback registered for as long as it is held.
pub(super) struct EndpointNotifications {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl EndpointNotifications {
    pub(super) fn register(wake: Sender<()>) -> WinResult<Self> {
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
