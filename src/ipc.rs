use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// Read the PID file and return the pid of the running daemon (if any).
pub fn running_pid() -> Result<u32> {
    running_pid_at(&crate::paths::pid_file()?)
}

fn running_pid_at(path: &Path) -> Result<u32> {
    let raw = std::fs::read_to_string(path)
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

// ── Process control ───────────────────────────────────────────────────────────

fn process_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

fn _stop(pid: u32) -> Result<()> {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), Signal::SIGTERM)
        .with_context(|| format!("sending SIGTERM to pid {pid}"))
}

fn _reload(pid: u32) -> Result<()> {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), Signal::SIGHUP)
        .with_context(|| format!("sending SIGHUP to pid {pid}"))
}

fn launch_agent_installed() -> bool {
    crate::paths::launch_agent_plist()
        .map(|p| p.exists())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pid_file(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("summon.pid");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[test]
    fn running_pid_returns_pid_of_live_process() {
        let me = std::process::id();
        let (_dir, path) = write_pid_file(&format!("{me}\n"));
        assert_eq!(running_pid_at(&path).unwrap(), me);
    }

    #[test]
    fn running_pid_rejects_dead_process_as_stale() {
        // Far above any real pid limit, but still a positive i32 so the
        // liveness check targets a single (nonexistent) process.
        let (_dir, path) = write_pid_file("99999999");
        let err = running_pid_at(&path).unwrap_err().to_string();
        assert!(err.contains("stale pid file"), "got: {err}");
    }

    #[test]
    fn running_pid_rejects_garbage_contents() {
        let (_dir, path) = write_pid_file("not-a-pid");
        let err = format!("{:#}", running_pid_at(&path).unwrap_err());
        assert!(err.contains("invalid pid file contents"), "got: {err}");
    }

    #[test]
    fn running_pid_errors_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let err = format!("{:#}", running_pid_at(&dir.path().join("none.pid")).unwrap_err());
        assert!(err.contains("reading pid file"), "got: {err}");
    }

    #[test]
    fn pid_file_path_matches_paths_module() {
        assert_eq!(pid_file_path().unwrap(), crate::paths::pid_file().unwrap());
    }
}
