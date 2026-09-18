use crate::hotkey::HotkeyRegistry;
use crate::paths;
use crate::summoner::Summoner;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[cfg(target_os = "macos")]
use crate::macos::dispatch;
#[cfg(target_os = "windows")]
use crate::windows::dispatch;

#[cfg(target_os = "macos")]
struct State {
    summoner: Summoner,
    registry: HotkeyRegistry,
    cfg_path: PathBuf,
    /// Hotkey ids currently in their Press→(Release|threshold) window, each
    /// tagged with the sequence number of the press that opened it. Empty when
    /// hold-to-minimize is disabled. Entries are removed by whichever of the
    /// release handler or the threshold timer wins; the other branch no-ops.
    pending_holds: std::collections::HashMap<u32, u32>,
    /// Monotonic press counter. `dispatch_after` can't be cancelled, so a
    /// timer from an earlier press stays in flight and would otherwise consume
    /// a later press's entry — minimizing when the user meant to summon.
    /// The seq lets that stale timer recognise itself and no-op.
    hold_seq: u32,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
static STATE: OnceLock<Mutex<State>> = OnceLock::new();

/// Lock the global daemon state, recovering from a poisoned mutex (a prior
/// handler panicked mid-update; the state is still structurally valid).
/// `None` only before STATE is initialized.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn lock_state() -> Option<std::sync::MutexGuard<'static, State>> {
    let guard = match STATE.get()?.lock() {
        Ok(g) => g,
        Err(e) => {
            warn!("STATE mutex poisoned; recovering");
            e.into_inner()
        }
    };
    Some(guard)
}

/// Pack a hotkey id and press sequence number into the single context
/// pointer the dispatch/timer plumbing carries (64-bit on both platforms), so
/// hold timers stay allocation-free while still identifying which press
/// scheduled them.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn pack_hold_ctx(id: u32, seq: u32) -> *mut std::ffi::c_void {
    (((seq as u64) << 32) | id as u64) as usize as *mut std::ffi::c_void
}

/// Inverse of [`pack_hold_ctx`], returning `(id, seq)`.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn unpack_hold_ctx(ctx: *mut std::ffi::c_void) -> (u32, u32) {
    let v = ctx as usize as u64;
    (v as u32, (v >> 32) as u32)
}

/// Owned copies of the binding fields, so the registry borrow ends before
/// the caller borrows the summoner mutably.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn binding_for(state: &State, id: u32) -> Option<(String, Option<String>)> {
    state
        .registry
        .target_for(id)
        .map(|t| (t.app.clone(), t.cmdline_contains.clone()))
}

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
                hold_seq: 0,
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
                hold_seq: 0,
                reload_event,
                stop_event,
            }))
            .ok()
            .expect("STATE initialized twice");

        spawn_ipc_watcher_thread(reload_event, stop_event);
        spawn_hotkey_forwarder();

        // Block on the Win32 message pump. Exits when WM_QUIT is posted
        // (from msg_wnd_proc's WM_SUMMON_STOP handler, triggered by post_shutdown).
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

/// Bridge global-hotkey's crossbeam channel onto the main queue/pump. The
/// thread does a blocking `recv` — no polling, no wakeups when idle. Press
/// and Release events are routed to separate main-thread handlers so the
/// hold detector can run without bit-packing state into the context pointer.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn spawn_hotkey_forwarder() {
    use global_hotkey::{GlobalHotKeyEvent, HotKeyState};

    std::thread::Builder::new()
        .name("summon-hotkey-forwarder".into())
        .spawn(|| {
            let receiver = GlobalHotKeyEvent::receiver();
            loop {
                match receiver.recv() {
                    Ok(event) => {
                        // Embed the u32 hotkey id directly in the context
                        // pointer (always 64-bit on both platforms). Avoids a
                        // per-event heap alloc on the forwarder thread.
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

#[cfg(any(target_os = "macos", target_os = "windows"))]
extern "C" fn on_hotkey_press_main(ctx: *mut std::ffi::c_void) {
    let id = ctx as usize as u32;
    let Some(mut state) = lock_state() else {
        return;
    };
    let Some((ident, filter)) = binding_for(&state, id) else {
        warn!(id, "unmapped hotkey press");
        return;
    };
    match state.summoner.hold_threshold() {
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
            let seq = state.hold_seq.wrapping_add(1);
            state.hold_seq = seq;
            state.pending_holds.insert(id, seq);
            info!(ident, id, seq, threshold_ms = d.as_millis() as u64, "hotkey press (pending)");
            drop(state);
            let hold_ctx = pack_hold_ctx(id, seq);
            unsafe { dispatch::after_main_ms(d.as_millis() as u64, hold_ctx, on_hold_fire) };
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
extern "C" fn on_hotkey_release_main(ctx: *mut std::ffi::c_void) {
    let id = ctx as usize as u32;
    let Some(mut state) = lock_state() else {
        return;
    };
    let Some(seq) = state.pending_holds.remove(&id) else {
        // Timer already fired (or hold disabled — release wasn't tracked).
        return;
    };
    // Cancel the SetTimer so on_hold_fire never fires. The timer id is derived
    // from the context, so it must be the same packed value the press used.
    // macOS has no equivalent — dispatch_after can't be cancelled; its
    // hold-fire recognises the cleared/re-tagged entry and no-ops instead.
    #[cfg(target_os = "windows")]
    unsafe {
        dispatch::cancel_timer(pack_hold_ctx(id, seq))
    };
    let Some((ident, filter)) = binding_for(&state, id) else {
        warn!(id, "unmapped hotkey release");
        return;
    };
    info!(ident, id, seq, "hotkey released → summon");
    if let Err(e) = state.summoner.summon(&ident, filter.as_deref()) {
        warn!(ident, "summon failed: {e:#}");
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
extern "C" fn on_hold_fire(ctx: *mut std::ffi::c_void) {
    let (id, seq) = unpack_hold_ctx(ctx);
    let Some(mut state) = lock_state() else {
        return;
    };
    // Only the press that scheduled this timer may act on it. A mismatch means
    // that press was released (or reload cleared the map) and a later press
    // opened the current window — minimizing then would swallow the summon the
    // user asked for.
    if state.pending_holds.get(&id) != Some(&seq) {
        return;
    }
    state.pending_holds.remove(&id);
    let Some((ident, filter)) = binding_for(&state, id) else {
        warn!(id, "hold fired for unmapped id");
        return;
    };
    info!(ident, id, seq, "hold threshold elapsed → minimize");
    if let Err(e) = state
        .summoner
        .minimize_on_active_display(&ident, filter.as_deref())
    {
        warn!(ident, "minimize_on_active_display failed: {e:#}");
    }
}

/// Re-read the config and swap in the new bindings/settings. Shared by the
/// macOS SIGHUP handler and the Windows reload-event handler.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn apply_reload(state: &mut State) {
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

/// Set SIGHUP/SIGTERM/SIGINT to SIG_IGN so default disposition can't
/// terminate the daemon, then attach a libdispatch signal source per
/// signal. Sources observe via kqueue regardless of disposition; handlers
/// run on the main queue, serialized with hotkey handlers.
#[cfg(target_os = "macos")]
fn install_signal_sources() {
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
    let Some(mut state) = lock_state() else {
        return;
    };
    apply_reload(&mut state);
}

#[cfg(target_os = "macos")]
extern "C" fn on_shutdown(_ctx: *mut std::ffi::c_void) {
    info!("SIGTERM/SIGINT received; shutting down");
    if let Some(mut state) = lock_state() {
        state.registry.unregister_all();
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
    pending_holds: std::collections::HashMap<u32, u32>,
    hold_seq: u32,
    reload_event: usize, // HANDLE stored as usize (Send-safe)
    stop_event: usize,
}

// GlobalHotKeyManager on Windows contains a *mut c_void that lacks Send.
// STATE is only mutated on the message-pump (main) thread; the IPC watcher
// and hotkey forwarder only post messages and never touch STATE directly.
#[cfg(target_os = "windows")]
unsafe impl Send for State {}

// NOTE: OnceLock statics are not dropped on std::process::exit, so this Drop
// impl only fires if State is ever stack/heap allocated (e.g. in tests). The
// OS reclaims the handles on process exit regardless.
#[cfg(target_os = "windows")]
impl Drop for State {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe {
            if self.reload_event != 0 {
                CloseHandle(self.reload_event as *mut std::ffi::c_void);
            }
            if self.stop_event != 0 {
                CloseHandle(self.stop_event as *mut std::ffi::c_void);
            }
        }
    }
}

/// Called from dispatch.rs window proc when WM_SUMMON_RELOAD is received.
#[cfg(target_os = "windows")]
pub fn on_reload_main() {
    info!("reload event received; reloading config");
    let Some(mut state) = lock_state() else {
        return;
    };
    apply_reload(&mut state);
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
    let wide = crate::windows::to_wide(name);
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

#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
mod tests {
    use super::*;

    #[test]
    fn hold_ctx_round_trips_id_and_seq() {
        let (id, seq) = unpack_hold_ctx(pack_hold_ctx(524294, 7));
        assert_eq!(id, 524294);
        assert_eq!(seq, 7);
    }

    #[test]
    fn hold_ctx_round_trips_zero_seq() {
        let (id, seq) = unpack_hold_ctx(pack_hold_ctx(524294, 0));
        assert_eq!(id, 524294);
        assert_eq!(seq, 0);
    }

    #[test]
    fn hold_ctx_keeps_max_id_out_of_the_seq_half() {
        let (id, seq) = unpack_hold_ctx(pack_hold_ctx(u32::MAX, 1));
        assert_eq!(id, u32::MAX);
        assert_eq!(seq, 1);
    }

    #[test]
    fn hold_ctx_keeps_max_seq_out_of_the_id_half() {
        let (id, seq) = unpack_hold_ctx(pack_hold_ctx(1, u32::MAX));
        assert_eq!(id, 1);
        assert_eq!(seq, u32::MAX);
    }

    #[test]
    fn hold_ctx_differs_per_press_for_the_same_hotkey() {
        assert_ne!(pack_hold_ctx(524294, 1), pack_hold_ctx(524294, 2));
    }
}

