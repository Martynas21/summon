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
    crate::launchd::install()
}

fn uninstall() -> Result<()> {
    crate::launchd::uninstall()
}

fn grant() -> Result<()> {
    crate::launchd::grant()
}

fn status() -> Result<()> {
    let pid = crate::ipc::running_pid().ok();
    let cfg_path = crate::paths::config_file()?;
    let cfg_status = match crate::config::load(&cfg_path) {
        Ok(cfg) => format!("ok ({} bindings)", cfg.bindings.len()),
        Err(e) => format!("error: {e}"),
    };
    let ax = if crate::permissions::is_trusted() {
        "granted"
    } else {
        "denied"
    };

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
    r#"[settings]
# cycle_reset_ms = 1500   # ms of inactivity before window cycle resets
# hide_previous = false   # minimize previous app's window when switching
# hold_threshold_ms = 0   # hold hotkey to minimize (0 = disabled)

[bindings]
# Bundle ID (preferred) or display name as the app identifier.
# "ctrl+1" = "com.mitchellh.ghostty"
# "ctrl+2" = "Google Chrome"
# "ctrl+3" = { app = "Finder", launch_args = ["--new"] }
"#
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_subcommand() {
        for (args, want) in [
            (&["summon", "run"][..], "Run"),
            (&["summon", "install"], "Install"),
            (&["summon", "uninstall"], "Uninstall"),
            (&["summon", "reload"], "Reload"),
            (&["summon", "stop"], "Stop"),
            (&["summon", "status"], "Status"),
            (&["summon", "edit"], "Edit"),
            (&["summon", "manage"], "Manage"),
            (&["summon", "_grant"], "Grant"),
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert_eq!(format!("{:?}", cli.cmd), want, "args: {args:?}");
        }
    }

    #[test]
    fn missing_subcommand_is_an_error() {
        assert!(Cli::try_parse_from(["summon"]).is_err());
    }

    #[test]
    fn unknown_subcommand_is_an_error() {
        assert!(Cli::try_parse_from(["summon", "frobnicate"]).is_err());
    }

    #[test]
    fn validate_path_argument_is_optional() {
        let cli = Cli::try_parse_from(["summon", "validate", "/tmp/x.toml"]).unwrap();
        match cli.cmd {
            Cmd::Validate { path } => assert_eq!(path, Some(PathBuf::from("/tmp/x.toml"))),
            other => panic!("expected Validate, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["summon", "validate"]).unwrap();
        match cli.cmd {
            Cmd::Validate { path } => assert_eq!(path, None),
            other => panic!("expected Validate, got {other:?}"),
        }
    }

    #[test]
    fn default_config_template_is_valid_toml_with_no_bindings() {
        let cfg = crate::config::parse_str(default_config()).unwrap();
        assert!(cfg.bindings.is_empty());
    }

    #[test]
    fn validate_accepts_valid_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[bindings]\n\"ctrl+1\" = \"Ghostty\"\n").unwrap();
        validate(Some(path)).unwrap();
    }

    #[test]
    fn validate_rejects_invalid_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "not valid toml [").unwrap();
        assert!(validate(Some(path)).is_err());
    }
}
