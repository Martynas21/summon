use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "summon", version, about = "macOS app/window summoner")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Run the daemon in the foreground (logs to stderr).
    Run,
    /// Install the LaunchAgent so the daemon auto-starts on login.
    Install,
    /// Remove the LaunchAgent.
    Uninstall,
    /// Re-read the config file in the running daemon (sends SIGHUP).
    Reload,
    /// Report daemon status, AX permission, and config validity.
    Status,
    /// Parse-check a config file (defaults to ~/.config/summon/config.toml).
    Validate {
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
}

pub fn run(args: Cli) -> Result<()> {
    match args.cmd {
        Cmd::Run => crate::daemon::run(),
        Cmd::Install => crate::launchd::install(),
        Cmd::Uninstall => crate::launchd::uninstall(),
        Cmd::Reload => crate::ipc::reload(),
        Cmd::Status => status(),
        Cmd::Validate { path } => validate(path),
    }
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
    let ax = "n/a (non-macos)";

    println!("daemon:        {}", pid.map(|p| format!("running (pid {p})")).unwrap_or_else(|| "not running".into()));
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
