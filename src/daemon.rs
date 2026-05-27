use crate::hotkey::HotkeyRegistry;
use crate::paths;
use crate::summoner::Summoner;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

static RELOAD: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);

pub fn run() -> Result<()> {
    init_logging();

    #[cfg(target_os = "macos")]
    {
        if !crate::macos::permissions::request_trust() {
            eprintln!(
                "summon: Accessibility permission required.\n\
                 Grant in System Settings → Privacy & Security → Accessibility, then re-run."
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

    let summoner = Arc::new(Mutex::new(Summoner::new(&cfg)));
    let registry = Arc::new(Mutex::new(HotkeyRegistry::new()?));
    registry
        .lock()
        .unwrap()
        .register_all(&cfg.bindings)
        .context("registering hotkeys")?;

    write_pid_file()?;
    install_signal_handlers()?;

    info!(pid = std::process::id(), "summon daemon running");

    #[cfg(target_os = "macos")]
    {
        // Spawn worker BEFORE the NSApplication runloop blocks the main thread.
        let s_w = Arc::clone(&summoner);
        let r_w = Arc::clone(&registry);
        std::thread::spawn(move || worker_loop(s_w, r_w, cfg_path));

        // Block main thread on NSApp.run — this is what actually pumps Carbon
        // hotkey events. CFRunLoop alone doesn't dispatch them.
        run_nsapp();
        // Unreachable in normal operation; STOP path exits via worker.
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = registry;
        let _ = summoner;
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

#[cfg(unix)]
fn install_signal_handlers() -> Result<()> {
    use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
    extern "C" fn on_hup(_: i32) {
        RELOAD.store(true, Ordering::Relaxed);
    }
    extern "C" fn on_term(_: i32) {
        STOP.store(true, Ordering::Relaxed);
    }
    let hup = SigAction::new(
        SigHandler::Handler(on_hup),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    let term = SigAction::new(
        SigHandler::Handler(on_term),
        SaFlags::empty(),
        SigSet::empty(),
    );
    unsafe {
        sigaction(Signal::SIGHUP, &hup)?;
        sigaction(Signal::SIGTERM, &term)?;
        sigaction(Signal::SIGINT, &term)?;
    }
    Ok(())
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

#[cfg(target_os = "macos")]
fn worker_loop(
    summoner: Arc<Mutex<Summoner>>,
    registry: Arc<Mutex<HotkeyRegistry>>,
    cfg_path: PathBuf,
) {
    use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
    use std::time::Duration;

    let receiver = GlobalHotKeyEvent::receiver();
    loop {
        if STOP.load(Ordering::Relaxed) {
            info!("SIGTERM received; shutting down");
            cleanup_pid_file();
            registry.lock().unwrap().unregister_all();
            std::process::exit(0);
        }
        if RELOAD.swap(false, Ordering::Relaxed) {
            handle_reload(&summoner, &registry, &cfg_path);
        }
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                if event.state == HotKeyState::Pressed {
                    let ident = registry
                        .lock()
                        .unwrap()
                        .app_for(event.id)
                        .map(str::to_owned);
                    if let Some(ident) = ident {
                        info!(ident, id = event.id, "hotkey fired");
                        let mut s = summoner.lock().unwrap();
                        let result = s.summon(&ident);
                        if let Err(e) = result {
                            warn!(ident, "summon failed: {e:#}");
                        }
                    } else {
                        warn!(id = event.id, "unmapped hotkey event");
                    }
                }
            }
            Err(_) => continue,
        }
    }
}

#[cfg(target_os = "macos")]
fn handle_reload(
    summoner: &Arc<Mutex<Summoner>>,
    registry: &Arc<Mutex<HotkeyRegistry>>,
    cfg_path: &std::path::Path,
) {
    info!("SIGHUP received; reloading config");
    match crate::config::load(cfg_path) {
        Ok(new_cfg) => {
            let mut r = registry.lock().unwrap();
            r.unregister_all();
            if let Err(e) = r.register_all(&new_cfg.bindings) {
                error!("re-registering hotkeys after reload: {e:#}");
            }
            summoner.lock().unwrap().reconfigure(&new_cfg);
            info!(bindings = new_cfg.bindings.len(), "reload complete");
        }
        Err(e) => error!("reload: failed to parse config: {e:#}"),
    }
}
