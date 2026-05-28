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
    /// Stop the running daemon (sends SIGTERM).
    Stop,
    /// Report daemon status, AX permission, and config validity.
    Status,
    /// Parse-check a config file (defaults to ~/.config/summon/config.toml).
    Validate {
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
    /// Open the config file in $EDITOR (or $VISUAL, or TextEdit). Reloads
    /// the daemon after the editor exits if the config still parses.
    Edit,
    /// Internal: launchd-spawned helper that fires the TCC modal under
    /// launchd attribution. Not for direct use.
    #[command(name = "_grant", hide = true)]
    Grant,
}

pub fn run(args: Cli) -> Result<()> {
    match args.cmd {
        Cmd::Run => crate::daemon::run(),
        Cmd::Install => crate::launchd::install(),
        Cmd::Uninstall => crate::launchd::uninstall(),
        Cmd::Reload => crate::ipc::reload(),
        Cmd::Stop => crate::ipc::stop(),
        Cmd::Status => status(),
        Cmd::Validate { path } => validate(path),
        Cmd::Edit => edit(),
        Cmd::Grant => crate::launchd::grant(),
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

fn edit() -> Result<()> {
    use anyhow::{anyhow, Context};
    use std::process::Command;

    let path = crate::paths::config_file()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    if !path.exists() {
        std::fs::write(&path, "[settings]\n\n[bindings]\n")
            .with_context(|| format!("creating {}", path.display()))?;
    }

    let editor = std::env::var("EDITOR")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("VISUAL").ok().filter(|s| !s.is_empty()));

    let status = match editor {
        Some(cmd) => {
            // Honour $EDITOR with args (e.g. "code --wait"). Splitting on
            // whitespace mirrors what git and other tools do — good enough
            // for the common cases, no shell-injection surface.
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
        None => Command::new("/usr/bin/open")
            .arg("-t")
            .arg("-W")
            .arg(&path)
            .status()
            .context("spawning `open -t`")?,
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
