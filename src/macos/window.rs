use accessibility_sys::{
    kAXErrorSuccess, kAXFocusedAttribute, kAXFocusedWindowAttribute, kAXHiddenAttribute,
    kAXMainAttribute, kAXMinimizedAttribute, kAXPositionAttribute, kAXRaiseAction,
    kAXSizeAttribute, kAXStandardWindowSubrole, kAXSubroleAttribute, kAXTitleAttribute,
    kAXWindowsAttribute, kAXValueTypeCGPoint, kAXValueTypeCGSize, AXError,
    AXUIElementCopyAttributeValue, AXUIElementCreateApplication, AXUIElementGetTypeID,
    AXUIElementPerformAction, AXUIElementRef, AXUIElementSetAttributeValue, AXValueGetValue,
    AXValueRef,
};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use std::ffi::c_void;

// Private but stable since macOS 10.x — used by yabai, Hammerspoon, Rectangle,
// skhd. Maps an AXUIElement to its CGWindowID, which is the only identifier
// that is stable across AX queries (AXUIElementRef pointers are not).
unsafe extern "C" {
    fn _AXUIElementGetWindow(element: AXUIElementRef, window_id: *mut u32) -> AXError;
}
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation_sys::base::{CFGetTypeID, CFRelease, CFRetain, CFTypeRef};
use core_foundation_sys::number::{kCFBooleanFalse, kCFBooleanTrue, CFBooleanGetValue};
use core_foundation_sys::string::{CFStringGetTypeID, CFStringRef};
use std::ptr;

/// AXUIElement for an application PID. Drops via CFRelease.
pub struct AppEl(AXUIElementRef);

impl AppEl {
    pub fn for_pid(pid: i32) -> Option<Self> {
        let raw = unsafe { AXUIElementCreateApplication(pid) };
        if raw.is_null() {
            None
        } else {
            Some(Self(raw))
        }
    }
}

impl Drop for AppEl {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0 as _) }
    }
}

/// AXUIElement for a single window. Drops via CFRelease.
pub struct WindowEl(AXUIElementRef);

impl WindowEl {
    /// Stable cross-press identifier. Reads the underlying CGWindowID via the
    /// private `_AXUIElementGetWindow`. Returns None on the rare app that
    /// refuses the call (we fall back to picking window 0 in that case).
    pub fn window_id(&self) -> Option<u32> {
        let mut id: u32 = 0;
        let err = unsafe { _AXUIElementGetWindow(self.0, &mut id) };
        if err == kAXErrorSuccess {
            Some(id)
        } else {
            None
        }
    }
}

impl Drop for WindowEl {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0 as _) }
    }
}

/// Enumerate windows of the app. Filters non-standard subroles when at least
/// one standard window exists (so background apps with only palettes still
/// surface).
pub fn windows(app: &AppEl) -> Vec<WindowEl> {
    let all = copy_windows(app.0);
    if all.is_empty() {
        return all;
    }
    let standard: Vec<bool> = all.iter().map(|w| is_standard(w)).collect();
    if standard.iter().any(|b| *b) {
        all.into_iter()
            .zip(standard)
            .filter_map(|(w, ok)| if ok { Some(w) } else { None })
            .collect()
    } else {
        all
    }
}

fn copy_windows(app: AXUIElementRef) -> Vec<WindowEl> {
    let attr = cfstr(kAXWindowsAttribute);
    let mut value: CFTypeRef = ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(app, attr.as_concrete_TypeRef(), &mut value)
    };
    if err != kAXErrorSuccess || value.is_null() {
        return vec![];
    }
    if unsafe { CFGetTypeID(value) != CFArrayGetTypeID() } {
        unsafe { CFRelease(value) };
        return vec![];
    }
    let array = value as CFArrayRef;
    let count = unsafe { CFArrayGetCount(array) };
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let raw = unsafe { CFArrayGetValueAtIndex(array, i) } as AXUIElementRef;
        if !raw.is_null() {
            unsafe { CFRetain(raw as _) };
            out.push(WindowEl(raw));
        }
    }
    unsafe { CFRelease(value) };
    out
}

fn is_standard(window: &WindowEl) -> bool {
    match copy_string_attr(window.0, kAXSubroleAttribute) {
        Some(s) => s == kAXStandardWindowSubrole,
        None => false,
    }
}

fn copy_string_attr(el: AXUIElementRef, key: &str) -> Option<String> {
    let attr = cfstr(key);
    let mut value: CFTypeRef = ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(el, attr.as_concrete_TypeRef(), &mut value)
    };
    if err != kAXErrorSuccess || value.is_null() {
        return None;
    }
    if unsafe { CFGetTypeID(value) != CFStringGetTypeID() } {
        unsafe { CFRelease(value) };
        return None;
    }
    let cf = unsafe { CFString::wrap_under_create_rule(value as CFStringRef) };
    Some(cf.to_string())
}

fn copy_bool_attr(el: AXUIElementRef, key: &str) -> bool {
    let attr = cfstr(key);
    let mut value: CFTypeRef = ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(el, attr.as_concrete_TypeRef(), &mut value)
    };
    if err != kAXErrorSuccess || value.is_null() {
        return false;
    }
    let result = unsafe { CFBooleanGetValue(value as _) };
    unsafe { CFRelease(value) };
    result
}

pub fn is_main(window: &WindowEl) -> bool {
    copy_bool_attr(window.0, kAXMainAttribute)
}

pub fn is_minimized(window: &WindowEl) -> bool {
    copy_bool_attr(window.0, kAXMinimizedAttribute)
}

/// Native fullscreen — the window occupies its own Space. Such windows are
/// absent from `kAXWindowsAttribute` while another Space is active, and
/// macOS rejects `kAXMinimizedAttribute` on them.
///
/// "AXFullScreen" has no `accessibility_sys` constant; it is the same
/// private-but-stable string AltTab, yabai and Hammerspoon read.
pub fn is_fullscreen(window: &WindowEl) -> bool {
    copy_bool_attr(window.0, AX_FULLSCREEN_ATTRIBUTE)
}

const AX_FULLSCREEN_ATTRIBUTE: &str = "AXFullScreen";

/// Returns the AX error code (0 = success) — callers log it, because macOS
/// rejects minimize on fullscreen windows and silently ignoring that made
/// the daemon claim minimizes it never performed.
pub fn unminimize(window: &WindowEl) -> AXError {
    set_minimized(window, false)
}

pub fn minimize(window: &WindowEl) -> AXError {
    set_minimized(window, true)
}

fn set_minimized(window: &WindowEl, minimized: bool) -> AXError {
    let attr = cfstr(kAXMinimizedAttribute);
    unsafe {
        let value: CFTypeRef = if minimized {
            kCFBooleanTrue as _
        } else {
            kCFBooleanFalse as _
        };
        AXUIElementSetAttributeValue(window.0, attr.as_concrete_TypeRef(), value)
    }
}

/// Set the app's AXHidden attribute. App-level equivalent of Cmd+H but
/// instant (no miniaturize animation), and goes through AX so it works on
/// the currently-active app where NSRunningApplication.hide() returns NO.
/// Returns the AX error code (0 = success) so callers can log it.
pub fn set_app_hidden(app: &AppEl, hidden: bool) -> AXError {
    let attr = cfstr(kAXHiddenAttribute);
    unsafe {
        let value: CFTypeRef = if hidden {
            kCFBooleanTrue as _
        } else {
            kCFBooleanFalse as _
        };
        AXUIElementSetAttributeValue(app.0, attr.as_concrete_TypeRef(), value)
    }
}

/// Raise within the app's window stack. On a window whose Space is not the
/// active one this is what triggers the Space switch.
pub fn raise(window: &WindowEl) -> AXError {
    let action = cfstr(kAXRaiseAction);
    unsafe { AXUIElementPerformAction(window.0, action.as_concrete_TypeRef()) }
}

pub fn title(window: &WindowEl) -> Option<String> {
    copy_string_attr(window.0, kAXTitleAttribute)
}

/// AX position+size of a window, packed into a CGRect in Quartz (top-left
/// origin) coordinates — the same coord space `CGDisplay::bounds` returns,
/// so callers can do containment tests directly without flipping.
pub fn frame(window: &WindowEl) -> Option<CGRect> {
    let pos = copy_ax_point(window.0, kAXPositionAttribute)?;
    let size = copy_ax_size(window.0, kAXSizeAttribute)?;
    Some(CGRect::new(&pos, &size))
}

/// Frontmost focused window of an app (kAXFocusedWindow). None when the
/// app has no focused window (background-only apps, or transient state).
pub fn focused_window(app: &AppEl) -> Option<WindowEl> {
    let attr = cfstr(kAXFocusedWindowAttribute);
    let mut value: CFTypeRef = ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(app.0, attr.as_concrete_TypeRef(), &mut value)
    };
    if err != kAXErrorSuccess || value.is_null() {
        return None;
    }
    if unsafe { CFGetTypeID(value) != AXUIElementGetTypeID() } {
        unsafe { CFRelease(value) };
        return None;
    }
    Some(WindowEl(value as AXUIElementRef))
}

fn copy_ax_point(el: AXUIElementRef, key: &str) -> Option<CGPoint> {
    let attr = cfstr(key);
    let mut value: CFTypeRef = ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(el, attr.as_concrete_TypeRef(), &mut value)
    };
    if err != kAXErrorSuccess || value.is_null() {
        return None;
    }
    let mut p = CGPoint::new(0.0, 0.0);
    let ok = unsafe {
        AXValueGetValue(
            value as AXValueRef,
            kAXValueTypeCGPoint,
            &mut p as *mut _ as *mut c_void,
        )
    };
    unsafe { CFRelease(value) };
    if ok { Some(p) } else { None }
}

fn copy_ax_size(el: AXUIElementRef, key: &str) -> Option<CGSize> {
    let attr = cfstr(key);
    let mut value: CFTypeRef = ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(el, attr.as_concrete_TypeRef(), &mut value)
    };
    if err != kAXErrorSuccess || value.is_null() {
        return None;
    }
    let mut s = CGSize::new(0.0, 0.0);
    let ok = unsafe {
        AXValueGetValue(
            value as AXValueRef,
            kAXValueTypeCGSize,
            &mut s as *mut _ as *mut c_void,
        )
    };
    unsafe { CFRelease(value) };
    if ok { Some(s) } else { None }
}

/// Mark window as the app's main+focused window. Required when the app is
/// already foreground — AXRaise on its own doesn't change which window
/// the OS treats as the app's main one, so focus snaps back.
/// Returns the (main, focused) AX error codes.
pub fn focus(window: &WindowEl) -> (AXError, AXError) {
    unsafe {
        let main = cfstr(kAXMainAttribute);
        let main_err = AXUIElementSetAttributeValue(
            window.0,
            main.as_concrete_TypeRef(),
            kCFBooleanTrue as _,
        );
        let focused = cfstr(kAXFocusedAttribute);
        let focused_err = AXUIElementSetAttributeValue(
            window.0,
            focused.as_concrete_TypeRef(),
            kCFBooleanTrue as _,
        );
        (main_err, focused_err)
    }
}

fn cfstr(s: &str) -> CFString {
    CFString::new(s)
}
