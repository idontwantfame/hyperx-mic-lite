use windows::{
    Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
    core::Result as WinResult,
};

pub(crate) struct ComApartment {
    // CoUninitialize must run on the thread that called CoInitializeEx; the raw-pointer
    // PhantomData makes this guard !Send/!Sync so it cannot cross threads.
    _not_send: std::marker::PhantomData<*mut ()>,
}

impl ComApartment {
    pub(crate) fn init() -> WinResult<Self> {
        // SAFETY: CoInitializeEx takes no pointers (reserved arg must be None); the guard is
        // only constructed when init succeeded, so every ComApartment matches one successful init.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
        Ok(Self {
            _not_send: std::marker::PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: balances the successful CoInitializeEx in init(); the guard is created at most
        // once per init, so this uninitialize cannot underflow the COM init count.
        unsafe { CoUninitialize() };
    }
}
