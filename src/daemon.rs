use crate::hotkey::HotkeyRegistry;
use crate::paths;
use crate::summoner::Summoner;
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
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
                 Grant in System Settings → Privacy & Security → Accessibility, then re-run.\n\
                 (A system prompt may have just appeared if this is the first run.)"
            );
            std::process::exit(2);
        }
    }

    let cfg_path = paths::config_file()?;
    let cfg = crate::config::load(&cfg_path)
        .with_context(|| format!("loading {}", cfg_path.display()))?;
    info!(path = %cfg_path.display(), bindings = cfg.bindings.len(), "loaded config");

    let mut summoner = Summoner::new(&cfg);
    let mut registry = HotkeyRegistry::new()?;
    registry
        .register_all(&cfg.bindings)
        .context("registering hotkeys")?;

    write_pid_file()?;
    install_signal_handlers()?;

    info!(pid = std::process::id(), "summon daemon running");
    main_loop(&mut summoner, &mut registry, &cfg_path);

    cleanup_pid_file();
    registry.unregister_all();
    info!("summon daemon stopped");
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
fn main_loop(summoner: &mut Summoner, registry: &mut HotkeyRegistry, cfg_path: &std::path::Path) {
    use core_foundation_sys::runloop::{kCFRunLoopDefaultMode, CFRunLoopRunInMode};
    use global_hotkey::{GlobalHotKeyEvent, HotKeyState};

    let receiver = GlobalHotKeyEvent::receiver();
    while !STOP.load(Ordering::Relaxed) {
        // Pump main-thread runloop briefly so Carbon hotkey events fire.
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, 0) };

        while let Ok(event) = receiver.try_recv() {
            if event.state == HotKeyState::Pressed {
                if let Some(ident) = registry.app_for(event.id) {
                    let ident = ident.to_string();
                    if let Err(e) = summoner.summon(&ident) {
                        warn!(ident = %ident, "summon failed: {e:#}");
                    }
                }
            }
        }

        if RELOAD.swap(false, Ordering::Relaxed) {
            handle_reload(summoner, registry, cfg_path);
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn main_loop(_: &mut Summoner, _: &mut HotkeyRegistry, _: &std::path::Path) {
    eprintln!("summon: only macOS is supported");
}

fn handle_reload(
    summoner: &mut Summoner,
    registry: &mut HotkeyRegistry,
    cfg_path: &std::path::Path,
) {
    info!("SIGHUP received; reloading config");
    match crate::config::load(cfg_path) {
        Ok(new_cfg) => {
            registry.unregister_all();
            if let Err(e) = registry.register_all(&new_cfg.bindings) {
                error!("re-registering hotkeys after reload: {e:#}");
            }
            summoner.reconfigure(&new_cfg);
            info!(bindings = new_cfg.bindings.len(), "reload complete");
        }
        Err(e) => error!("reload: failed to parse config: {e:#}"),
    }
}
