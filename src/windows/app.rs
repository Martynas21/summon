use anyhow::{Context, Result};
use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, FALSE};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;

// QueryFullProcessImageNameW lives in kernel32 / psapi; declare manually to
// avoid windows-sys feature path issues.
#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryFullProcessImageNameW(
        hprocess: *mut c_void,
        dwflags: u32,
        lpexename: *mut u16,
        lpdwsize: *mut u32,
    ) -> i32;
}

const PROCESS_NAME_WIN32: u32 = 0;

/// A running application identified by PID and lowercase exe stem.
#[derive(Debug, Clone)]
pub struct RunningApp {
    pub pid: u32,
    /// Lowercase exe stem, e.g. "firefox" for "firefox.exe".
    pub exe_name: String,
}

pub fn find_running_filtered(ident: &str, cmdline_filter: Option<&str>) -> Option<RunningApp> {
    let ident_lower = ident.to_lowercase();
    let mut candidates: Vec<RunningApp> = Vec::new();

    for_each_process(|pid, stem| {
        if stem != ident_lower {
            return true;
        }
        if let Some(filter) = cmdline_filter {
            if !super::proc::cmdline_contains(pid, filter) {
                return true;
            }
        }
        candidates.push(RunningApp { pid, exe_name: stem.to_string() });
        true
    });

    if candidates.len() <= 1 {
        return candidates.into_iter().next();
    }
    // Electron apps (VS Code, Slack, …) spawn many helper processes sharing
    // the same exe name. The main process may not own any HWNDs — renderer
    // processes do. Prefer a candidate that owns enumerable windows.
    if let Some(app) = candidates
        .iter()
        .find(|a| !super::window::tray_windows_for_pid(a.pid).is_empty())
    {
        return Some(app.clone());
    }
    candidates.into_iter().next()
}

pub fn launch(ident: &str) -> Result<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let wide_open: Vec<u16> = OsStr::new("open").encode_wide().chain(Some(0)).collect();
    let wide_ident: Vec<u16> = OsStr::new(ident).encode_wide().chain(Some(0)).collect();

    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            wide_open.as_ptr(),
            wide_ident.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        )
    };
    if result as usize > 32 {
        Ok(())
    } else {
        anyhow::bail!("ShellExecuteW failed for {:?} (code {})", ident, result as usize)
    }
}

pub fn launch_and_wait_filtered(
    ident: &str,
    timeout: Duration,
    cmdline_filter: Option<&str>,
) -> Result<RunningApp> {
    launch(ident)?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(app) = find_running_filtered(ident, cmdline_filter) {
            return Ok(app);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {:?} to start", ident);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// No-op: Windows hide/show is per-HWND and handled in window.rs.
pub fn ensure_visible(_app: &RunningApp) {}

pub fn activate(app: &RunningApp) {
    let wins = super::window::windows_for_pid(app.pid);
    if let Some(w) = wins.first() {
        super::window::raise(w);
    }
}

pub fn pid(app: &RunningApp) -> u32 {
    app.pid
}

pub fn name(app: &RunningApp) -> Option<String> {
    Some(app.exe_name.clone())
}

pub fn for_pid(pid: u32) -> Option<RunningApp> {
    exe_path_for_pid(pid).map(|path| RunningApp {
        pid,
        exe_name: exe_stem(&path),
    })
}

pub fn frontmost_pid() -> Option<u32> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, &mut pid);
        if pid == 0 { None } else { Some(pid) }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Enumerate all processes using CreateToolhelp32Snapshot. Calls `cb(pid,
/// exe_stem)` for every process; return `false` from the callback to stop early.
/// This covers apps (e.g. Ghostty) that don't own a traditional top-level HWND
/// and would be missed by an EnumWindows-based scan.
fn for_each_process(mut cb: impl FnMut(u32, &str) -> bool) {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
    };
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() {
        return;
    }

    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

    if unsafe { Process32FirstW(snapshot, &mut entry) } != FALSE {
        loop {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let raw = String::from_utf16_lossy(&entry.szExeFile[..len]);
            let stem = exe_stem(&raw);
            if !stem.is_empty() && !cb(entry.th32ProcessID, &stem) {
                break;
            }
            if unsafe { Process32NextW(snapshot, &mut entry) } == FALSE {
                break;
            }
        }
    }

    unsafe { CloseHandle(snapshot) };
}

fn exe_path_for_pid(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == FALSE || len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]).to_lowercase())
    }
}

fn exe_stem(path: &str) -> String {
    PathBuf::from(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| path.to_lowercase())
}

/// Returns (identifier, display_name) for all user-facing processes.
pub fn list_running_apps() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::<String>::new();

    for_each_process(|pid, stem| {
        if is_system_stem(stem) {
            return true;
        }
        // Filter system dirs when we can get the full path; if we can't,
        // include the app anyway (it passed the stem filter).
        let include = match exe_path_for_pid(pid) {
            Some(path) => !is_windows_dir(&path),
            None => true,
        };
        if include && seen.insert(stem.to_string()) {
            out.push((stem.to_string(), stem.to_string()));
        }
        true
    });

    out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    out
}

fn is_system_stem(stem: &str) -> bool {
    matches!(
        stem,
        "system" | "idle" | "registry" | "smss" | "csrss" | "wininit" | "winlogon"
            | "lsass" | "services" | "svchost" | "dwm" | "conhost" | "searchindexer"
            | "taskhostw" | "sihost" | "fontdrvhost" | "dllhost" | "ctfmon"
            | "runtimebroker" | "applicationframehost" | "searchhost" | "spoolsv"
            | "wudfhost" | "msdtc" | "lsm" | "audiodg" | "wermgr" | "unsecapp"
            | "wmiprvse" | "securityhealthservice" | "textinputhost"
            | "backgroundtransferhost" | "useroobebroker"
    )
}

fn is_windows_dir(path: &str) -> bool {
    let p = path.to_lowercase();
    p.contains("\\windows\\system32\\")
        || p.contains("\\windows\\syswow64\\")
        || p.contains("\\windows\\systemapps\\")
}
