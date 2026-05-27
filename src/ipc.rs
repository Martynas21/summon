use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

/// Read the PID file and return the pid of the running daemon (if any).
pub fn running_pid() -> Result<u32> {
    let path = crate::paths::pid_file()?;
    let raw = std::fs::read_to_string(&path)
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

/// Send SIGHUP to the running daemon to trigger a config reload.
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
    send_signal(pid, nix::sys::signal::Signal::SIGHUP)
        .with_context(|| format!("sending SIGHUP to pid {pid}"))?;
    println!("reload signal sent to pid {pid}");
    Ok(())
}

fn process_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

fn send_signal(pid: u32, sig: nix::sys::signal::Signal) -> Result<()> {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), sig)?;
    Ok(())
}

/// Path to the running daemon's PID file (always returned, even if file absent).
pub fn pid_file_path() -> Result<PathBuf> {
    crate::paths::pid_file()
}
