use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn config_dir() -> Result<PathBuf> {
    let dirs = directories::BaseDirs::new().context("no home dir")?;
    Ok(dirs.home_dir().join(".config").join("summon"))
}

pub fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn state_dir() -> Result<PathBuf> {
    let dirs = directories::BaseDirs::new().context("no home dir")?;
    Ok(dirs
        .home_dir()
        .join("Library")
        .join("Application Support")
        .join("summon"))
}

pub fn pid_file() -> Result<PathBuf> {
    Ok(state_dir()?.join("summon.pid"))
}

pub fn log_dir() -> Result<PathBuf> {
    let dirs = directories::BaseDirs::new().context("no home dir")?;
    Ok(dirs.home_dir().join("Library").join("Logs").join("summon"))
}

pub fn launch_agent_plist() -> Result<PathBuf> {
    let dirs = directories::BaseDirs::new().context("no home dir")?;
    Ok(dirs
        .home_dir()
        .join("Library")
        .join("LaunchAgents")
        .join("dev.summon.daemon.plist"))
}

pub fn launch_agent_label() -> &'static str {
    "dev.summon.daemon"
}
