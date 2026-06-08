use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "summon", version, about = "App/window summoner — bind a hotkey to an app")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Run the daemon in the foreground (logs to stderr).
    Run,
    /// Install the autostart entry (LaunchAgent on macOS, Task Scheduler on Windows).
    Install,
    /// Remove the autostart entry.
    Uninstall,
    /// Re-read the config file in the running daemon.
    Reload,
    /// Stop the running daemon.
    Stop,
    /// Report daemon status and config validity.
    Status,
    /// Parse-check a config file (defaults to the standard config path).
    Validate {
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
    /// Open the config file in $EDITOR (or a platform default). Reloads the
    /// daemon after the editor exits if the config still parses.
    Edit,
    /// Manage hotkey bindings via interactive TUI.
    Manage,
    /// Internal: launchd-spawned helper that fires the TCC modal under
    /// launchd attribution. Not for direct use.
    #[command(name = "_grant", hide = true)]
    Grant,
}

pub fn run(args: Cli) -> Result<()> {
    match args.cmd {
        Cmd::Run => crate::daemon::run(),
        Cmd::Install => install(),
        Cmd::Uninstall => uninstall(),
        Cmd::Reload => crate::ipc::reload(),
        Cmd::Stop => crate::ipc::stop(),
        Cmd::Status => status(),
        Cmd::Validate { path } => validate(path),
        Cmd::Edit => edit(),
        Cmd::Manage => crate::tui::run(),
        Cmd::Grant => grant(),
    }
}

fn install() -> Result<()> {
    #[cfg(target_os = "macos")]
    return crate::launchd::install();
    #[cfg(target_os = "windows")]
    return crate::windows::service::install();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    anyhow::bail!("install is not supported on this platform");
}

fn uninstall() -> Result<()> {
    #[cfg(target_os = "macos")]
    return crate::launchd::uninstall();
    #[cfg(target_os = "windows")]
    return crate::windows::service::uninstall();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    anyhow::bail!("uninstall is not supported on this platform");
}

fn grant() -> Result<()> {
    #[cfg(target_os = "macos")]
    return crate::launchd::grant();
    #[cfg(not(target_os = "macos"))]
    Ok(()) // no-op: no TCC equivalent on Windows/Linux
}

fn status() -> Result<()> {
    let pid = crate::ipc::running_pid().ok();
    let cfg_path = crate::paths::config_file()?;
    let cfg_status = match crate::config::load(&cfg_path) {
        Ok(cfg) => format!("ok ({} bindings)", cfg.bindings.len()),
        Err(e) => format!("error: {e}"),
    };
    #[cfg(target_os = "macos")]
    let ax = if crate::macos::permissions::is_trusted() {
        "granted"
    } else {
        "denied"
    };
    #[cfg(not(target_os = "macos"))]
    let ax = "n/a";

    println!(
        "daemon:        {}",
        pid.map(|p| format!("running (pid {p})"))
            .unwrap_or_else(|| "not running".into())
    );
    println!("ax permission: {ax}");
    println!("config:        {} ({})", cfg_path.display(), cfg_status);
    Ok(())
}

fn validate(path: Option<PathBuf>) -> Result<()> {
    let path = match path {
        Some(p) => p,
        None => crate::paths::config_file()?,
    };
    let cfg = crate::config::load(&path)?;
    println!("ok: {} bindings", cfg.bindings.len());
    Ok(())
}

fn default_config() -> &'static str {
    #[cfg(target_os = "windows")]
    return r#"[settings]
# cycle_reset_ms = 1500   # ms of inactivity before window cycle resets
# hide_previous = false   # minimize previous app's window when switching
# hold_threshold_ms = 0   # hold hotkey to minimize (0 = disabled)

[bindings]
# Use the exe stem (without .exe) as the app identifier.
# "ctrl+1" = "firefox"
# "ctrl+2" = "Code"
# "ctrl+3" = "WindowsTerminal"
# "ctrl+4" = "explorer"
"#;
    #[cfg(not(target_os = "windows"))]
    return r#"[settings]
# cycle_reset_ms = 1500   # ms of inactivity before window cycle resets
# hide_previous = false   # minimize previous app's window when switching
# hold_threshold_ms = 0   # hold hotkey to minimize (0 = disabled)

[bindings]
# Bundle ID (preferred) or display name as the app identifier.
# "ctrl+1" = "com.mitchellh.ghostty"
# "ctrl+2" = "Google Chrome"
# "ctrl+3" = { app = "Finder", launch_args = ["--new"] }
"#;
}

fn edit() -> Result<()> {
    use anyhow::{anyhow, Context};
    use std::process::Command;

    let path = crate::paths::config_file()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    if !path.exists() {
        std::fs::write(&path, default_config())
            .with_context(|| format!("creating {}", path.display()))?;
    }

    let editor = std::env::var("EDITOR")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("VISUAL").ok().filter(|s| !s.is_empty()));

    let status = match editor {
        Some(cmd) => {
            let mut parts = cmd.split_whitespace();
            let prog = parts
                .next()
                .ok_or_else(|| anyhow!("EDITOR is empty after split"))?;
            Command::new(prog)
                .args(parts)
                .arg(&path)
                .status()
                .with_context(|| format!("spawning editor: {cmd}"))?
        }
        #[cfg(target_os = "macos")]
        None => Command::new("/usr/bin/open")
            .arg("-t")
            .arg("-W")
            .arg(&path)
            .status()
            .context("spawning `open -t`")?,
        #[cfg(target_os = "windows")]
        None => Command::new("notepad")
            .arg(&path)
            .status()
            .context("spawning notepad")?,
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        None => anyhow::bail!("no editor found; set $EDITOR"),
    };
    if !status.success() {
        return Err(anyhow!("editor exited with {status}"));
    }

    match crate::config::load(&path) {
        Ok(cfg) => {
            println!("ok: {} bindings", cfg.bindings.len());
            if crate::ipc::running_pid().is_ok() {
                crate::ipc::reload()?;
                println!("reloaded");
            }
        }
        Err(e) => {
            eprintln!("config invalid, daemon NOT reloaded: {e}");
            return Err(e);
        }
    }
    Ok(())
}
