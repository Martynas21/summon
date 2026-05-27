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
            }))
            .ok()
            .expect("STATE initialized twice");

        install_signal_sources();
        spawn_hotkey_forwarder();

        // Block main thread on NSApp.run — pumps Carbon hotkey events,
        // dispatches signal sources, runs blocks posted to the main queue.
        run_nsapp();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = cfg;
        let _ = cfg_path;
        eprintln!("summon: only macOS is supported");
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
/// thread does a blocking `recv` — no polling, no wakeups when idle. On
/// each event it boxes the id and hands ownership to `dispatch_async_f`,
/// which runs `on_hotkey_main` on the main thread.
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
                        if event.state == HotKeyState::Pressed {
                            // Embed the u32 hotkey id directly in the
                            // context pointer (always 64-bit on macOS).
                            // Avoids a per-press heap alloc on the forwarder
                            // thread and a Box::from_raw on the main thread.
                            let ctx = event.id as usize as *mut std::ffi::c_void;
                            unsafe { dispatch::async_to_main(ctx, on_hotkey_main) };
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
extern "C" fn on_hotkey_main(ctx: *mut std::ffi::c_void) {
    let id = ctx as usize as u32;
    let Some(state_lock) = STATE.get() else {
        return;
    };
    let mut state = state_lock.lock().unwrap();
    let ident = state.registry.app_for(id).map(str::to_owned);
    match ident {
        Some(ident) => {
            info!(ident, id, "hotkey fired");
            if let Err(e) = state.summoner.summon(&ident) {
                warn!(ident, "summon failed: {e:#}");
            }
        }
        None => warn!(id, "unmapped hotkey event"),
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
