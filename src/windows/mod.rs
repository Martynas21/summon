pub mod app;
pub mod dispatch;
pub mod permissions;
pub mod proc;
pub mod screen;
pub mod service;
pub mod window;

/// Null-terminated UTF-16 for Win32 `*W` APIs.
pub fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
}
