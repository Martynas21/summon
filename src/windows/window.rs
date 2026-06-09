use std::ffi::c_void;
use windows_sys::Win32::Foundation::{BOOL, FALSE, LPARAM, TRUE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetForegroundWindow, GetWindow, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, GW_OWNER, GWL_EXSTYLE,
    SW_FORCEMINIMIZE, SW_RESTORE, SW_SHOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

// Manual declaration — AttachThreadInput lives in user32 but isn't always
// accessible via the Win32_System_Threading feature path in windows-sys 0.59.
#[link(name = "user32")]
unsafe extern "system" {
    fn AttachThreadInput(idattach: u32, idattachto: u32, fattach: BOOL) -> BOOL;
}

/// An opaque window handle. Stores HWND as *mut c_void (the true Win32 type).
/// HWND is not Send by default; we assert it's safe to pass across threads
/// because Win32 window handles are process-global identifiers.
#[derive(Debug, Clone, Copy)]
pub struct WindowHandle(pub *mut c_void);

unsafe impl Send for WindowHandle {}
unsafe impl Sync for WindowHandle {}

impl PartialEq for WindowHandle {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

/// Screen rectangle from GetWindowRect.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Enumerate top-level, visible, unowned windows with a title belonging to `pid`.
pub fn windows_for_pid(pid: u32) -> Vec<WindowHandle> {
    enum_windows_for_pid(pid, true)
}

/// Like `windows_for_pid` but includes hidden windows. Used as a fallback for
/// apps like Discord that collapse to the system tray via `SW_HIDE` rather than
/// minimising — `raise` already calls `SW_SHOW`, so these windows restore fine.
pub fn tray_windows_for_pid(pid: u32) -> Vec<WindowHandle> {
    enum_windows_for_pid(pid, false)
}

fn enum_windows_for_pid(pid: u32, visible_only: bool) -> Vec<WindowHandle> {
    let mut result: Vec<WindowHandle> = Vec::new();
    let data: (*mut Vec<WindowHandle>, u32, bool) = (&mut result, pid, visible_only);
    unsafe {
        EnumWindows(Some(enum_windows_cb), &data as *const _ as LPARAM);
    }
    result
}

extern "system" fn enum_windows_cb(hwnd: *mut c_void, lparam: LPARAM) -> BOOL {
    unsafe {
        let data = &*(lparam as *const (*mut Vec<WindowHandle>, u32, bool));
        let list = &mut *data.0;
        let target_pid = data.1;
        let visible_only = data.2;

        let mut window_pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut window_pid);
        if window_pid != target_pid {
            return TRUE;
        }
        if visible_only && IsWindowVisible(hwnd) == FALSE {
            return TRUE;
        }
        if !GetWindow(hwnd, GW_OWNER).is_null() {
            return TRUE;
        }
        if GetWindowTextLengthW(hwnd) == 0 {
            return TRUE;
        }
        list.push(WindowHandle(hwnd));
        TRUE
    }
}

pub fn is_minimized(win: &WindowHandle) -> bool {
    unsafe { IsIconic(win.0) != FALSE }
}

pub fn minimize(win: &WindowHandle) {
    unsafe { ShowWindow(win.0, SW_FORCEMINIMIZE) };
}

pub fn unminimize(win: &WindowHandle) {
    unsafe { ShowWindow(win.0, SW_RESTORE) };
}

/// Raise and focus the window, working around Windows foreground-lock.
pub fn raise(win: &WindowHandle) {
    unsafe {
        let fg = GetForegroundWindow();
        let current_tid = GetCurrentThreadId();
        let mut target_tid = 0u32;
        let mut fg_tid = 0u32;
        GetWindowThreadProcessId(win.0, &mut target_tid);
        GetWindowThreadProcessId(fg, &mut fg_tid);

        let attach_fg = fg_tid != 0 && fg_tid != current_tid && fg_tid != target_tid;
        // Also attach to the target thread so Electron apps (Discord, VS Code)
        // receive a proper WM_ACTIVATE and route input correctly after restore.
        let attach_target = target_tid != 0 && target_tid != current_tid;
        if attach_fg {
            AttachThreadInput(current_tid, fg_tid, TRUE);
        }
        if attach_target {
            AttachThreadInput(current_tid, target_tid, TRUE);
        }
        // Tray apps set WS_EX_TOOLWINDOW (hides from taskbar/Alt+Tab) and
        // sometimes WS_EX_NOACTIVATE when hiding to tray. Both prevent proper
        // activation after restore — strip them before showing.
        if IsWindowVisible(win.0) == FALSE {
            let ex = GetWindowLongPtrW(win.0, GWL_EXSTYLE);
            let strip = (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) as isize;
            if ex & strip != 0 {
                SetWindowLongPtrW(win.0, GWL_EXSTYLE, ex & !strip);
                // Frame-style changes are cached — SWP_FRAMECHANGED flushes
                // them to the shell so the window becomes taskbar/Alt+Tab
                // eligible and SetForegroundWindow can activate it.
                SetWindowPos(
                    win.0,
                    std::ptr::null_mut(),
                    0, 0, 0, 0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
                );
            }
        }
        let show_cmd = if IsWindowVisible(win.0) == FALSE { SW_RESTORE } else { SW_SHOW };
        ShowWindow(win.0, show_cmd);
        BringWindowToTop(win.0);
        SetForegroundWindow(win.0);
        if attach_target {
            AttachThreadInput(current_tid, target_tid, FALSE);
        }
        if attach_fg {
            AttachThreadInput(current_tid, fg_tid, FALSE);
        }
    }
}

pub fn focus(win: &WindowHandle) {
    raise(win);
}

pub fn title(win: &WindowHandle) -> Option<String> {
    unsafe {
        let len = GetWindowTextLengthW(win.0);
        if len == 0 {
            return None;
        }
        let mut buf: Vec<u16> = vec![0u16; (len + 1) as usize];
        let copied = GetWindowTextW(win.0, buf.as_mut_ptr(), len + 1);
        if copied == 0 {
            return None;
        }
        buf.truncate(copied as usize);
        Some(String::from_utf16_lossy(&buf))
    }
}

pub fn frame(win: &WindowHandle) -> Option<Rect> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;
    unsafe {
        let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        if GetWindowRect(win.0, &mut r) == FALSE {
            return None;
        }
        Some(Rect {
            x: r.left,
            y: r.top,
            width: r.right - r.left,
            height: r.bottom - r.top,
        })
    }
}

/// Lower 32 bits of the HWND pointer value, used as a cycle-state key.
/// HWNDs fit in 32 bits in practice on current Windows.
pub fn window_id(win: &WindowHandle) -> Option<u32> {
    Some(win.0 as usize as u32)
}

/// The currently focused foreground window belonging to `pid`, if any.
pub fn focused_window_for_pid(pid: u32) -> Option<WindowHandle> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_null() {
            return None;
        }
        let mut fg_pid = 0u32;
        GetWindowThreadProcessId(fg, &mut fg_pid);
        if fg_pid == pid { Some(WindowHandle(fg)) } else { None }
    }
}
