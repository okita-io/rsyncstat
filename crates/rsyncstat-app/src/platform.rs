//! Platform glue.

/// Run `work` after the current window event has returned.
///
/// On macOS the folder panel is a nested modal loop. Starting it from inside
/// winit's event handler makes a resize of that panel re-enter the handler,
/// which aborts. `dispatch_async` to the main queue runs only after this
/// event returns. Other platforms show the dialog immediately.
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
    #[cfg(not(target_os = "macos"))]
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
