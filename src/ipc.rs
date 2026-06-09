use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

/// Read the PID file and return the pid of the running daemon (if any).
pub fn running_pid() -> Result<u32> {
    let path = crate::paths::pid_file()?;
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading pid file {}", path.display()))?;
    let pid: u32 = raw
        .trim()
        .parse()
        .with_context(|| format!("invalid pid file contents: {raw:?}"))?;
    if !process_alive(pid) {
        return Err(anyhow!("stale pid file {} (pid {pid} not running)", path.display()));
    }
    Ok(pid)
}

/// Send a stop signal to the running daemon.
pub fn stop() -> Result<()> {
    let pid = running_pid()?;
    _stop(pid)?;
    println!("stop signal sent to pid {pid}");
    #[cfg(target_os = "macos")]
    if launch_agent_installed() {
        println!(
            "note: LaunchAgent is installed; launchd will restart the daemon.\n\
             Use `summon uninstall` to stop it permanently."
        );
    }
    Ok(())
}

/// Send reload signal silently — for use inside the TUI.
/// Returns Ok(true) if a running daemon was found and signalled,
/// Ok(false) if no daemon was running.
pub fn reload_quiet() -> Result<bool> {
    match running_pid() {
        Ok(pid) => {
            _reload(pid)?;
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

/// Send a reload signal to the running daemon.
/// Validates the config first so the user sees parse errors immediately
/// (the daemon would have failed silently and kept the old config).
pub fn reload() -> Result<()> {
    let cfg_path = crate::paths::config_file()?;
    match crate::config::load(&cfg_path) {
        Ok(cfg) => {
            println!("config ok ({} bindings)", cfg.bindings.len());
        }
        Err(e) => {
            eprintln!("config invalid — daemon will not reload:\n{e:#}");
            return Err(e);
        }
    }
    let pid = running_pid()?;
    _reload(pid)?;
    println!("reload signal sent to pid {pid}");
    Ok(())
}

/// Path to the running daemon's PID file (always returned, even if file absent).
pub fn pid_file_path() -> Result<PathBuf> {
    crate::paths::pid_file()
}

// ── Unix implementation ───────────────────────────────────────────────────────

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

#[cfg(unix)]
fn _stop(pid: u32) -> Result<()> {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), Signal::SIGTERM)
        .with_context(|| format!("sending SIGTERM to pid {pid}"))
}

#[cfg(unix)]
fn _reload(pid: u32) -> Result<()> {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), Signal::SIGHUP)
        .with_context(|| format!("sending SIGHUP to pid {pid}"))
}

#[cfg(target_os = "macos")]
fn launch_agent_installed() -> bool {
    crate::paths::launch_agent_plist()
        .map(|p| p.exists())
        .unwrap_or(false)
}

// ── Windows implementation ────────────────────────────────────────────────────

// Manual Win32 declarations — module paths for these vary across windows-sys versions.
#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenEventW(
        dw_desired_access: u32,
        b_inherit_handle: i32,
        lp_name: *const u16,
    ) -> *mut std::ffi::c_void;

    fn SetEvent(hevent: *mut std::ffi::c_void) -> i32;
}

// STILL_ACTIVE = 259 (STATUS_PENDING), used by GetExitCodeProcess for live processes.
#[cfg(target_os = "windows")]
const STILL_ACTIVE: u32 = 259;

#[cfg(target_os = "windows")]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
        if handle.is_null() {
            return false;
        }
        let mut exit_code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);
        ok != 0 && exit_code == STILL_ACTIVE
    }
}

#[cfg(target_os = "windows")]
fn _stop(pid: u32) -> Result<()> {
    open_named_event("Local\\summon-stop")
        .with_context(|| format!("opening stop event for pid {pid}; is the daemon running?"))
        .and_then(set_and_close_event)
}

#[cfg(target_os = "windows")]
fn _reload(pid: u32) -> Result<()> {
    open_named_event("Local\\summon-reload")
        .with_context(|| format!("opening reload event for pid {pid}; is the daemon running?"))
        .and_then(set_and_close_event)
}

#[cfg(target_os = "windows")]
fn open_named_event(name: &str) -> Result<*mut std::ffi::c_void> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    const EVENT_MODIFY_STATE: u32 = 0x0002;
    let wide: Vec<u16> = OsStr::new(name).encode_wide().chain(std::iter::once(0)).collect();
    let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, wide.as_ptr()) };
    if handle.is_null() {
        anyhow::bail!("OpenEventW failed for {name}");
    }
    Ok(handle)
}

#[cfg(target_os = "windows")]
fn set_and_close_event(handle: *mut std::ffi::c_void) -> Result<()> {
    use windows_sys::Win32::Foundation::CloseHandle;
    unsafe {
        SetEvent(handle);
        CloseHandle(handle);
    }
    Ok(())
}
