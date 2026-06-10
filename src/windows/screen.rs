use super::window::WindowHandle;
use windows_sys::Win32::Foundation::{POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    MonitorFromPoint, MonitorFromRect, HMONITOR, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTOPRIMARY,
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
