use super::window::WindowHandle;
use std::ffi::c_void;
use windows_sys::Win32::Foundation::{LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, MonitorFromPoint, MonitorFromRect, HDC, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

/// HMONITOR stored as isize for PartialEq + Send + Copy.
pub type DisplayId = isize;

fn hmon(m: HMONITOR) -> DisplayId {
    m as isize
}

pub fn active_display() -> DisplayId {
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut pt) != 0 {
            return hmon(MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY));
        }
        hmon(MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY))
    }
}

pub fn window_display(win: &WindowHandle) -> Option<DisplayId> {
    let r = super::window::frame(win)?;
    unsafe {
        let rect = RECT {
            left: r.x,
            top: r.y,
            right: r.x + r.width,
            bottom: r.y + r.height,
        };
        Some(hmon(MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST)))
    }
}

#[allow(dead_code)]
pub fn all_displays() -> Vec<DisplayId> {
    let mut monitors: Vec<DisplayId> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            0 as HDC,
            std::ptr::null(),
            Some(enum_monitors_cb),
            &mut monitors as *mut Vec<DisplayId> as LPARAM,
        );
    }
    monitors
}

extern "system" fn enum_monitors_cb(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> i32 {
    unsafe {
        let list = &mut *(lparam as *mut Vec<DisplayId>);
        list.push(hmon(hmonitor));
    }
    1
}
