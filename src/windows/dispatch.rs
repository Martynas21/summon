/// Main-thread work dispatch via a hidden HWND_MESSAGE window.
/// Mirrors the macOS libdispatch interface used in daemon.rs.
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, KillTimer,
    PostMessageW, PostQuitMessage, RegisterClassW, SetTimer, TranslateMessage,
    WM_TIMER, WM_USER, WNDCLASSW,
};

pub const WM_SUMMON_DISPATCH: u32 = WM_USER + 1;
pub const WM_SUMMON_RELOAD: u32 = WM_USER + 2;
pub const WM_SUMMON_STOP: u32 = WM_USER + 3;

// HWND stored as usize (Send-safe; HWND = *mut c_void is the Win32 type).
static MSG_HWND: OnceLock<usize> = OnceLock::new();

// Maps timer IDs (ctx as usize) → (ctx as usize, work_fn).
// Store ctx as usize so the HashMap is Send (raw pointers aren't Send).
static PENDING_TIMERS: Mutex<Option<HashMap<usize, (usize, extern "C" fn(*mut c_void))>>> =
    Mutex::new(None);

/// Create the hidden message window. Call once on the main thread before
/// `run_message_pump()`.
pub fn create_message_window() -> Result<()> {
    let hwnd = unsafe { create_hwnd() }.context("creating summon message window")?;
    MSG_HWND.set(hwnd).ok();
    *PENDING_TIMERS.lock().unwrap() = Some(HashMap::new());
    Ok(())
}

/// Run the Win32 message pump until WM_QUIT.
pub fn run_message_pump() {
    use windows_sys::Win32::UI::WindowsAndMessaging::MSG;
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        loop {
            let ret = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if ret == 0 || ret == -1 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

pub unsafe fn async_to_main(ctx: *mut c_void, work: extern "C" fn(*mut c_void)) {
    let hwnd = msg_hwnd();
    unsafe { PostMessageW(hwnd, WM_SUMMON_DISPATCH, work as usize, ctx as isize) };
}

pub unsafe fn after_main_ms(ms: u64, ctx: *mut c_void, work: extern "C" fn(*mut c_void)) {
    let hwnd = msg_hwnd();
    let timer_id = ctx as usize;
    {
        let mut guard = PENDING_TIMERS.lock().unwrap();
        if let Some(map) = guard.as_mut() {
            map.insert(timer_id, (ctx as usize, work));
        }
    }
    unsafe { SetTimer(hwnd, timer_id, ms as u32, None) };
}

pub unsafe fn cancel_timer(ctx: *mut c_void) {
    let hwnd = msg_hwnd();
    let timer_id = ctx as usize;
    unsafe { KillTimer(hwnd, timer_id) };
    let mut guard = PENDING_TIMERS.lock().unwrap();
    if let Some(map) = guard.as_mut() {
        map.remove(&timer_id);
    }
}

pub fn post_reload() {
    unsafe { PostMessageW(msg_hwnd(), WM_SUMMON_RELOAD, 0, 0) };
}

pub fn post_shutdown() {
    unsafe { PostQuitMessage(0) };
}

fn msg_hwnd() -> HWND {
    *MSG_HWND.get().expect("message window not created") as HWND
}

unsafe fn create_hwnd() -> Result<usize> {
    let class_name: Vec<u16> = OsStr::new("SummonMsgWnd")
        .encode_wide()
        .chain(Some(0))
        .collect();

    let hinstance = GetModuleHandleW(std::ptr::null());

    let wc = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(msg_wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name.as_ptr(),
    };
    RegisterClassW(&wc);

    let window_name: Vec<u16> = OsStr::new("SummonMsgWnd")
        .encode_wide()
        .chain(Some(0))
        .collect();

    // HWND_MESSAGE = (HWND)(-3)
    let hwnd_message: HWND = (-3isize) as HWND;

    let hwnd = CreateWindowExW(
        0,
        class_name.as_ptr(),
        window_name.as_ptr(),
        0,
        0, 0, 0, 0,
        hwnd_message,
        std::ptr::null_mut(),
        hinstance,
        std::ptr::null(),
    );

    if hwnd.is_null() {
        anyhow::bail!("CreateWindowExW returned NULL");
    }
    Ok(hwnd as usize)
}

unsafe extern "system" fn msg_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_SUMMON_DISPATCH => {
            let work: extern "C" fn(*mut c_void) = std::mem::transmute(wparam);
            let ctx = lparam as *mut c_void;
            work(ctx);
            0
        }
        WM_TIMER => {
            let timer_id = wparam;
            KillTimer(hwnd, timer_id);
            let entry = {
                let mut guard = PENDING_TIMERS.lock().unwrap();
                guard.as_mut().and_then(|m| m.remove(&timer_id))
            };
            if let Some((ctx_usize, work)) = entry {
                work(ctx_usize as *mut c_void);
            }
            0
        }
        WM_SUMMON_RELOAD => {
            crate::daemon::on_reload_main();
            0
        }
        WM_SUMMON_STOP => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
