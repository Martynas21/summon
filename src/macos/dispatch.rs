use std::ffi::c_void;
use std::os::raw::c_int;

unsafe extern "C" {
    #[link_name = "_dispatch_main_q"]
    static DISPATCH_MAIN_Q: c_void;

    #[link_name = "_dispatch_source_type_signal"]
    static DISPATCH_SOURCE_TYPE_SIGNAL: c_void;

    fn dispatch_async_f(
        queue: *mut c_void,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );

    fn dispatch_time(when: u64, delta: i64) -> u64;

    fn dispatch_after_f(
        when: u64,
        queue: *mut c_void,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );

    fn dispatch_source_create(
        ty: *const c_void,
        handle: usize,
        mask: usize,
        queue: *mut c_void,
    ) -> *mut c_void;

    fn dispatch_source_set_event_handler_f(
        source: *mut c_void,
        handler: extern "C" fn(*mut c_void),
    );

    fn dispatch_resume(object: *mut c_void);
}

const DISPATCH_TIME_NOW: u64 = 0;

fn main_queue() -> *mut c_void {
    (&raw const DISPATCH_MAIN_Q) as *mut c_void
}

/// Schedule `work` on the main thread. `ctx` is passed verbatim; caller
/// owns its lifecycle (typically `Box::into_raw` + `Box::from_raw` inside
/// the handler).
pub unsafe fn async_to_main(ctx: *mut c_void, work: extern "C" fn(*mut c_void)) {
    unsafe { dispatch_async_f(main_queue(), ctx, work) };
}

/// Schedule `work` on the main queue after `ms` milliseconds. Same context
/// ownership rules as `async_to_main`.
pub unsafe fn after_main_ms(ms: u64, ctx: *mut c_void, work: extern "C" fn(*mut c_void)) {
    let when = unsafe { dispatch_time(DISPATCH_TIME_NOW, (ms as i64) * 1_000_000) };
    unsafe { dispatch_after_f(when, main_queue(), ctx, work) };
}

/// Install a libdispatch signal source on the main queue. The default
/// signal disposition must be set to SIG_IGN beforehand so the kernel
/// doesn't terminate (or interrupt) the process before the source fires —
/// signal sources observe via kqueue, independent of the disposition.
/// The created source is intentionally leaked (lives for process lifetime).
pub fn install_signal_handler(signum: c_int, handler: extern "C" fn(*mut c_void)) {
    unsafe {
        let source = dispatch_source_create(
            (&raw const DISPATCH_SOURCE_TYPE_SIGNAL) as *const c_void,
            signum as usize,
            0,
            main_queue(),
        );
        assert!(!source.is_null(), "dispatch_source_create returned NULL");
        dispatch_source_set_event_handler_f(source, handler);
        dispatch_resume(source);
    }
}
