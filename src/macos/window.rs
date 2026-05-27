use accessibility_sys::{
    kAXErrorSuccess, kAXFocusedAttribute, kAXMainAttribute, kAXMinimizedAttribute, kAXRaiseAction,
    kAXStandardWindowSubrole, kAXSubroleAttribute, kAXTitleAttribute, kAXWindowsAttribute,
    AXError, AXUIElementCopyAttributeValue, AXUIElementCreateApplication,
    AXUIElementPerformAction, AXUIElementRef, AXUIElementSetAttributeValue,
};

// Private but stable since macOS 10.x — used by yabai, Hammerspoon, Rectangle,
// skhd. Maps an AXUIElement to its CGWindowID, which is the only identifier
// that is stable across AX queries (AXUIElementRef pointers are not).
extern "C" {
    fn _AXUIElementGetWindow(element: AXUIElementRef, window_id: *mut u32) -> AXError;
}
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation_sys::base::{CFRelease, CFRetain, CFTypeRef};
use core_foundation_sys::number::{kCFBooleanFalse, kCFBooleanTrue, CFBooleanGetValue};
use core_foundation_sys::string::CFStringRef;
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
    let cf = unsafe { CFString::wrap_under_create_rule(value as CFStringRef) };
    Some(cf.to_string())
}

pub fn is_main(window: &WindowEl) -> bool {
    let attr = cfstr(kAXMainAttribute);
    let mut value: CFTypeRef = ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(window.0, attr.as_concrete_TypeRef(), &mut value)
    };
    if err != kAXErrorSuccess || value.is_null() {
        return false;
    }
    let result = unsafe { CFBooleanGetValue(value as _) };
    unsafe { CFRelease(value) };
    result
}

pub fn is_minimized(window: &WindowEl) -> bool {
    let attr = cfstr(kAXMinimizedAttribute);
    let mut value: CFTypeRef = ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(window.0, attr.as_concrete_TypeRef(), &mut value)
    };
    if err != kAXErrorSuccess || value.is_null() {
        return false;
    }
    let result = unsafe { CFBooleanGetValue(value as _) };
    unsafe { CFRelease(value) };
    result
}

pub fn unminimize(window: &WindowEl) {
    let attr = cfstr(kAXMinimizedAttribute);
    unsafe {
        let _ = AXUIElementSetAttributeValue(
            window.0,
            attr.as_concrete_TypeRef(),
            kCFBooleanFalse as _,
        );
    }
}

pub fn raise(window: &WindowEl) {
    let action = cfstr(kAXRaiseAction);
    unsafe {
        let _ = AXUIElementPerformAction(window.0, action.as_concrete_TypeRef());
    }
}

pub fn title(window: &WindowEl) -> Option<String> {
    copy_string_attr(window.0, kAXTitleAttribute)
}

/// Mark window as the app's main+focused window. Required when the app is
/// already foreground — AXRaise on its own doesn't change which window
/// the OS treats as the app's main one, so focus snaps back.
pub fn focus(window: &WindowEl) {
    unsafe {
        let main = cfstr(kAXMainAttribute);
        let _ = AXUIElementSetAttributeValue(
            window.0,
            main.as_concrete_TypeRef(),
            kCFBooleanTrue as _,
        );
        let focused = cfstr(kAXFocusedAttribute);
        let _ = AXUIElementSetAttributeValue(
            window.0,
            focused.as_concrete_TypeRef(),
            kCFBooleanTrue as _,
        );
    }
}

fn cfstr(s: &str) -> CFString {
    CFString::new(s)
}
