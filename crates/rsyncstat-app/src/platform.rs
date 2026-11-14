//! Platform glue.

/// Run `work` without blocking the window's event loop.
///
/// On macOS the folder panel is a nested modal loop. Starting it from inside
/// winit's event handler makes a resize of that panel re-enter the handler,
/// which aborts. `dispatch_async` to the main queue runs only after this
/// event returns.
///
/// On Linux, `rfd` waits on the XDG file-chooser portal until the dialog
/// closes. Doing that on the UI thread stops Wayland ping replies, and
/// Hyprland then opens its own dialog (`CANRManager` in `/usr/bin/Hyprland`,
/// `misc:enable_anr_dialog`): "An application {title} - {class} is not
/// responding." The portal call itself is D-Bus and does not need the UI
/// thread, so it runs in the background. The result is delivered on the
/// channel the caller already polls.
pub(crate) fn run_after_event(work: impl FnOnce() + Send + 'static) {
    #[cfg(target_os = "macos")]
    {
        let work: Box<dyn FnOnce() + Send> = Box::new(work);
        let ctx = Box::into_raw(Box::new(work));
        // SAFETY: `ctx` is a Box we own. The main queue calls `call_after_event`
        // exactly once, and that function frees it. The queue pointer is the
        // process-wide main queue (`_dispatch_main_q`).
        unsafe {
            dispatch_async_f(main_queue(), ctx.cast(), call_after_event);
        }
    }
    #[cfg(target_os = "linux")]
    {
        std::thread::Builder::new()
            .name("folder-dialog".into())
            .spawn(work)
            .expect("spawn folder dialog thread");
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        work();
    }
}

#[cfg(target_os = "macos")]
extern "C" fn call_after_event(ctx: *mut std::ffi::c_void) {
    // SAFETY: `dispatch_async_f` passes back the Box pointer we gave it, once.
    let work = unsafe { Box::from_raw(ctx.cast::<Box<dyn FnOnce() + Send>>()) };
    work();
}

#[cfg(target_os = "macos")]
fn main_queue() -> *mut std::ffi::c_void {
    // Recent SDKs define `dispatch_get_main_queue()` as an inline that returns
    // `&_dispatch_main_q`, so that function is not a linkable symbol.
    std::ptr::addr_of!(_dispatch_main_q).cast_mut().cast()
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    static _dispatch_main_q: u8;
    fn dispatch_async_f(
        queue: *mut std::ffi::c_void,
        context: *mut std::ffi::c_void,
        work: extern "C" fn(*mut std::ffi::c_void),
    );
}
