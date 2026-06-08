use crate::hotkey::HotkeyRegistry;
use crate::paths;
use crate::summoner::Summoner;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[cfg(target_os = "macos")]
struct State {
    summoner: Summoner,
    registry: HotkeyRegistry,
    cfg_path: PathBuf,
    /// Hotkey ids currently in their Press→(Release|threshold) window. Empty
    /// when hold-to-minimize is disabled. Entries are removed by whichever
    /// of the release handler or the threshold timer wins; the other branch
    /// then no-ops.
    pending_holds: std::collections::HashMap<u32, ()>,
}

#[cfg(target_os = "macos")]
static STATE: OnceLock<Mutex<State>> = OnceLock::new();

pub fn run() -> Result<()> {
    init_logging();

    #[cfg(target_os = "macos")]
    {
        // Only prompt when launched from a terminal. Under launchd KeepAlive,
        // a missing grant would otherwise spam the TCC modal every respawn.
        let trusted = if is_foreground() {
            crate::macos::permissions::request_trust()
        } else {
            crate::macos::permissions::is_trusted()
        };
        if !trusted {
            eprintln!(
                "summon: Accessibility permission required.\n\
                 Run `summon run` from a terminal to trigger the prompt,\n\
                 then grant in System Settings → Privacy & Security → Accessibility."
            );
            std::process::exit(2);
        }
    }

    if let Ok(existing) = crate::ipc::running_pid() {
        eprintln!(
            "summon: another daemon is already running (pid {existing}).\n\
             Use `summon reload` to re-read config, or `pkill summon` to stop it."
        );
        std::process::exit(1);
    }

    let cfg_path = paths::config_file()?;
    let cfg = crate::config::load(&cfg_path)
        .with_context(|| format!("loading {}", cfg_path.display()))?;
    info!(path = %cfg_path.display(), bindings = cfg.bindings.len(), "loaded config");

    write_pid_file()?;

    info!(pid = std::process::id(), "summon daemon running");

    #[cfg(target_os = "macos")]
    {
        let summoner = Summoner::new(&cfg);
        let mut registry = HotkeyRegistry::new()?;
        registry
            .register_all(&cfg.bindings)
            .context("registering hotkeys")?;

        STATE
            .set(Mutex::new(State {
                summoner,
                registry,
                cfg_path,
                pending_holds: std::collections::HashMap::new(),
            }))
            .ok()
            .expect("STATE initialized twice");

        install_signal_sources();
        spawn_hotkey_forwarder();

        // Block main thread on NSApp.run — pumps Carbon hotkey events,
        // dispatches signal sources, runs blocks posted to the main queue.
        run_nsapp();
    }
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext;
        // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 = -4
        // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 = (HANDLE)(-4)
        unsafe { SetProcessDpiAwarenessContext((-4isize) as *mut std::ffi::c_void) };

        crate::windows::dispatch::create_message_window()
            .context("creating Windows message window")?;

        let reload_event = create_win_event("Local\\summon-reload")?;
        let stop_event = create_win_event("Local\\summon-stop")?;

        let summoner = Summoner::new(&cfg);
        let mut registry = HotkeyRegistry::new()?;
        registry
            .register_all(&cfg.bindings)
            .context("registering hotkeys")?;

        STATE
            .set(Mutex::new(State {
                summoner,
                registry,
                cfg_path,
                pending_holds: std::collections::HashMap::new(),
                reload_event,
                stop_event,
            }))
            .ok()
            .expect("STATE initialized twice");

        spawn_ipc_watcher_thread(reload_event, stop_event);
        spawn_hotkey_forwarder();

        // Block on the Win32 message pump. Exits when WM_QUIT is posted
        // (from on_shutdown or SetConsoleCtrlHandler).
        crate::windows::dispatch::run_message_pump();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = cfg;
        let _ = cfg_path;
        eprintln!("summon: only macOS and Windows are supported");
    }

    cleanup_pid_file();
    Ok(())
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

fn write_pid_file() -> Result<()> {
    let path = paths::pid_file()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, std::process::id().to_string())
        .with_context(|| format!("writing pid file {}", path.display()))?;
    Ok(())
}

fn cleanup_pid_file() {
    if let Ok(path) = paths::pid_file() {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(target_os = "macos")]
fn is_foreground() -> bool {
    use std::os::fd::AsRawFd;
    unsafe { libc::isatty(std::io::stderr().as_raw_fd()) == 1 }
}

#[cfg(target_os = "macos")]
fn run_nsapp() {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::MainThreadMarker;
    let mtm = MainThreadMarker::new().expect("daemon must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    unsafe {
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.run();
    }
}

/// Bridge global-hotkey's crossbeam channel onto the main queue. The
/// thread does a blocking `recv` — no polling, no wakeups when idle. Press
/// and Release events are routed to separate main-thread handlers so the
/// hold detector can run without bit-packing state into the context pointer.
#[cfg(target_os = "macos")]
fn spawn_hotkey_forwarder() {
    use crate::macos::dispatch;
    use global_hotkey::{GlobalHotKeyEvent, HotKeyState};

    std::thread::Builder::new()
        .name("summon-hotkey-forwarder".into())
        .spawn(|| {
            let receiver = GlobalHotKeyEvent::receiver();
            loop {
                match receiver.recv() {
                    Ok(event) => {
                        // Embed the u32 hotkey id directly in the context
                        // pointer (always 64-bit on macOS). Avoids a per-event
                        // heap alloc on the forwarder thread.
                        let ctx = event.id as usize as *mut std::ffi::c_void;
                        match event.state {
                            HotKeyState::Pressed => unsafe {
                                dispatch::async_to_main(ctx, on_hotkey_press_main)
                            },
                            HotKeyState::Released => unsafe {
                                dispatch::async_to_main(ctx, on_hotkey_release_main)
                            },
                        }
                    }
                    Err(e) => {
                        error!("hotkey channel closed: {e}");
                        return;
                    }
                }
            }
        })
        .expect("spawning hotkey forwarder thread");
}

#[cfg(target_os = "macos")]
extern "C" fn on_hotkey_press_main(ctx: *mut std::ffi::c_void) {
    use crate::macos::dispatch;
    let id = ctx as usize as u32;
    let Some(state_lock) = STATE.get() else {
        return;
    };
    let mut state = state_lock.lock().unwrap();
    let Some((ident, filter)) = state
        .registry
        .target_for(id)
        .map(|t| (t.app.clone(), t.cmdline_contains.clone()))
    else {
        warn!(id, "unmapped hotkey press");
        return;
    };
    let threshold = state.summoner.hold_threshold();
    match threshold {
        None => {
            info!(ident, id, "hotkey press (hold disabled)");
            if let Err(e) = state.summoner.summon(&ident, filter.as_deref()) {
                warn!(ident, "summon failed: {e:#}");
            }
        }
        Some(d) => {
            if state.pending_holds.contains_key(&id) {
                // Auto-repeat from the OS. The first press is still in flight;
                // ignore the duplicate so we don't reschedule a second timer.
                return;
            }
            state.pending_holds.insert(id, ());
            info!(ident, id, threshold_ms = d.as_millis() as u64, "hotkey press (pending)");
            drop(state);
            unsafe { dispatch::after_main_ms(d.as_millis() as u64, ctx, on_hold_fire) };
        }
    }
}

#[cfg(target_os = "macos")]
extern "C" fn on_hotkey_release_main(ctx: *mut std::ffi::c_void) {
    let id = ctx as usize as u32;
    let Some(state_lock) = STATE.get() else {
        return;
    };
    let mut state = state_lock.lock().unwrap();
    if state.pending_holds.remove(&id).is_none() {
        // Timer already fired (or hold disabled — release wasn't tracked).
        return;
    }
    let Some((ident, filter)) = state
        .registry
        .target_for(id)
        .map(|t| (t.app.clone(), t.cmdline_contains.clone()))
    else {
        warn!(id, "unmapped hotkey release");
        return;
    };
    info!(ident, id, "hotkey released → summon");
    if let Err(e) = state.summoner.summon(&ident, filter.as_deref()) {
        warn!(ident, "summon failed: {e:#}");
    }
}

#[cfg(target_os = "macos")]
extern "C" fn on_hold_fire(ctx: *mut std::ffi::c_void) {
    let id = ctx as usize as u32;
    let Some(state_lock) = STATE.get() else {
        return;
    };
    let mut state = state_lock.lock().unwrap();
    if state.pending_holds.remove(&id).is_none() {
        // Released before threshold (handled by release path) — or reload
        // cleared the map. Either way, no-op.
        return;
    }
    let Some((ident, filter)) = state
        .registry
        .target_for(id)
        .map(|t| (t.app.clone(), t.cmdline_contains.clone()))
    else {
        warn!(id, "hold fired for unmapped id");
        return;
    };
    info!(ident, id, "hold threshold elapsed → minimize");
    if let Err(e) = state.summoner.minimize_frontmost(&ident, filter.as_deref()) {
        warn!(ident, "minimize_frontmost failed: {e:#}");
    }
}

/// Set SIGHUP/SIGTERM/SIGINT to SIG_IGN so default disposition can't
/// terminate the daemon, then attach a libdispatch signal source per
/// signal. Sources observe via kqueue regardless of disposition; handlers
/// run on the main queue, serialized with hotkey handlers.
#[cfg(target_os = "macos")]
fn install_signal_sources() {
    use crate::macos::dispatch;
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }
    dispatch::install_signal_handler(libc::SIGHUP, on_sighup);
    dispatch::install_signal_handler(libc::SIGTERM, on_shutdown);
    dispatch::install_signal_handler(libc::SIGINT, on_shutdown);
}

#[cfg(target_os = "macos")]
extern "C" fn on_sighup(_ctx: *mut std::ffi::c_void) {
    info!("SIGHUP received; reloading config");
    let Some(state_lock) = STATE.get() else {
        return;
    };
    let mut state = state_lock.lock().unwrap();
    let cfg_path = state.cfg_path.clone();
    match crate::config::load(&cfg_path) {
        Ok(new_cfg) => {
            state.registry.unregister_all();
            if let Err(e) = state.registry.register_all(&new_cfg.bindings) {
                error!("re-registering hotkeys after reload: {e:#}");
            }
            state.summoner.reconfigure(&new_cfg);
            // Hotkey ids are assigned by global-hotkey at register time, so
            // post-reload they may map to different bindings. Discard any
            // in-flight hold state — pending timers fired afterwards will
            // find nothing and no-op.
            state.pending_holds.clear();
            info!(bindings = new_cfg.bindings.len(), "reload complete");
        }
        Err(e) => error!("reload: failed to parse config: {e:#}"),
    }
}

#[cfg(target_os = "macos")]
extern "C" fn on_shutdown(_ctx: *mut std::ffi::c_void) {
    info!("SIGTERM/SIGINT received; shutting down");
    if let Some(state_lock) = STATE.get() {
        state_lock.lock().unwrap().registry.unregister_all();
    }
    cleanup_pid_file();
    std::process::exit(0);
}

// ── Windows implementation ────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
struct State {
    summoner: Summoner,
    registry: HotkeyRegistry,
    cfg_path: PathBuf,
    pending_holds: std::collections::HashMap<u32, ()>,
    reload_event: usize, // HANDLE stored as usize (Send-safe)
    stop_event: usize,
}

// GlobalHotKeyManager on Windows contains a *mut c_void that lacks Send.
// STATE is only mutated on the message-pump (main) thread; the IPC watcher
// and hotkey forwarder only post messages and never touch STATE directly.
#[cfg(target_os = "windows")]
unsafe impl Send for State {}

#[cfg(target_os = "windows")]
static STATE: OnceLock<Mutex<State>> = OnceLock::new();

/// Called from dispatch.rs window proc when WM_SUMMON_RELOAD is received.
#[cfg(target_os = "windows")]
pub fn on_reload_main() {
    info!("reload event received; reloading config");
    let Some(state_lock) = STATE.get() else {
        return;
    };
    let mut state = state_lock.lock().unwrap();
    let cfg_path = state.cfg_path.clone();
    match crate::config::load(&cfg_path) {
        Ok(new_cfg) => {
            state.registry.unregister_all();
            if let Err(e) = state.registry.register_all(&new_cfg.bindings) {
                error!("re-registering hotkeys after reload: {e:#}");
            }
            state.summoner.reconfigure(&new_cfg);
            state.pending_holds.clear();
            info!(bindings = new_cfg.bindings.len(), "reload complete");
        }
        Err(e) => error!("reload: failed to parse config: {e:#}"),
    }
}

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(
        lp_event_attributes: *const std::ffi::c_void,
        b_manual_reset: i32,
        b_initial_state: i32,
        lp_name: *const u16,
    ) -> *mut std::ffi::c_void;

    fn WaitForMultipleObjects(
        ncount: u32,
        lphandles: *const *mut std::ffi::c_void,
        bwaitall: i32,
        dwmilliseconds: u32,
    ) -> u32;
}

#[cfg(target_os = "windows")]
fn create_win_event(name: &str) -> Result<usize> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = OsStr::new(name).encode_wide().chain(Some(0)).collect();
    let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, wide.as_ptr()) };
    if handle.is_null() {
        anyhow::bail!("CreateEventW failed for {name}");
    }
    Ok(handle as usize)
}

#[cfg(target_os = "windows")]
fn spawn_ipc_watcher_thread(reload_event: usize, stop_event: usize) {
    std::thread::Builder::new()
        .name("summon-ipc-watcher".into())
        .spawn(move || loop {
            let handles: [*mut std::ffi::c_void; 2] = [
                reload_event as *mut std::ffi::c_void,
                stop_event as *mut std::ffi::c_void,
            ];
            // INFINITE = 0xFFFFFFFF, WAIT_OBJECT_0 = 0
            let result = unsafe {
                WaitForMultipleObjects(2, handles.as_ptr(), 0, 0xFFFF_FFFFu32)
            };
            if result == 0 {
                crate::windows::dispatch::post_reload();
            } else if result == 1 {
                crate::windows::dispatch::post_shutdown();
                break;
            } else {
                warn!("WaitForMultipleObjects returned {result:#x}; stopping IPC watcher");
                break;
            }
        })
        .expect("spawning IPC watcher thread");
}

#[cfg(target_os = "windows")]
fn spawn_hotkey_forwarder() {
    use crate::windows::dispatch;
    use global_hotkey::{GlobalHotKeyEvent, HotKeyState};

    std::thread::Builder::new()
        .name("summon-hotkey-forwarder".into())
        .spawn(|| {
            let receiver = GlobalHotKeyEvent::receiver();
            loop {
                match receiver.recv() {
                    Ok(event) => {
                        let ctx = event.id as usize as *mut std::ffi::c_void;
                        match event.state {
                            HotKeyState::Pressed => unsafe {
                                dispatch::async_to_main(ctx, on_hotkey_press_main)
                            },
                            HotKeyState::Released => unsafe {
                                dispatch::async_to_main(ctx, on_hotkey_release_main)
                            },
                        }
                    }
                    Err(e) => {
                        error!("hotkey channel closed: {e}");
                        return;
                    }
                }
            }
        })
        .expect("spawning hotkey forwarder thread");
}

#[cfg(target_os = "windows")]
extern "C" fn on_hotkey_press_main(ctx: *mut std::ffi::c_void) {
    use crate::windows::dispatch;
    let id = ctx as usize as u32;
    let Some(state_lock) = STATE.get() else {
        return;
    };
    let mut state = state_lock.lock().unwrap();
    let Some((ident, filter)) = state
        .registry
        .target_for(id)
        .map(|t| (t.app.clone(), t.cmdline_contains.clone()))
    else {
        warn!(id, "unmapped hotkey press");
        return;
    };
    let threshold = state.summoner.hold_threshold();
    match threshold {
        None => {
            info!(ident, id, "hotkey press (hold disabled)");
            if let Err(e) = state.summoner.summon(&ident, filter.as_deref()) {
                warn!(ident, "summon failed: {e:#}");
            }
        }
        Some(d) => {
            if state.pending_holds.contains_key(&id) {
                return; // OS key-repeat; ignore
            }
            state.pending_holds.insert(id, ());
            info!(ident, id, threshold_ms = d.as_millis() as u64, "hotkey press (pending)");
            drop(state);
            unsafe { dispatch::after_main_ms(d.as_millis() as u64, ctx, on_hold_fire) };
        }
    }
}

#[cfg(target_os = "windows")]
extern "C" fn on_hotkey_release_main(ctx: *mut std::ffi::c_void) {
    use crate::windows::dispatch;
    let id = ctx as usize as u32;
    let Some(state_lock) = STATE.get() else {
        return;
    };
    let mut state = state_lock.lock().unwrap();
    if state.pending_holds.remove(&id).is_none() {
        return; // Timer already fired or hold disabled.
    }
    // Cancel the SetTimer so on_hold_fire never fires.
    unsafe { dispatch::cancel_timer(ctx) };
    let Some((ident, filter)) = state
        .registry
        .target_for(id)
        .map(|t| (t.app.clone(), t.cmdline_contains.clone()))
    else {
        warn!(id, "unmapped hotkey release");
        return;
    };
    info!(ident, id, "hotkey released → summon");
    if let Err(e) = state.summoner.summon(&ident, filter.as_deref()) {
        warn!(ident, "summon failed: {e:#}");
    }
}

#[cfg(target_os = "windows")]
extern "C" fn on_hold_fire(ctx: *mut std::ffi::c_void) {
    let id = ctx as usize as u32;
    let Some(state_lock) = STATE.get() else {
        return;
    };
    let mut state = state_lock.lock().unwrap();
    if state.pending_holds.remove(&id).is_none() {
        return; // Released before threshold fired (cancel_timer beat us).
    }
    let Some((ident, filter)) = state
        .registry
        .target_for(id)
        .map(|t| (t.app.clone(), t.cmdline_contains.clone()))
    else {
        warn!(id, "hold fired for unmapped id");
        return;
    };
    info!(ident, id, "hold threshold elapsed → minimize");
    if let Err(e) = state.summoner.minimize_frontmost(&ident, filter.as_deref()) {
        warn!(ident, "minimize_frontmost failed: {e:#}");
    }
}
