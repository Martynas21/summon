use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn config_dir() -> Result<PathBuf> {
    let dirs = directories::BaseDirs::new().context("no home dir")?;
    #[cfg(not(target_os = "windows"))]
    return Ok(dirs.home_dir().join(".config").join("summon"));
    #[cfg(target_os = "windows")]
    return Ok(dirs.data_dir().join("summon"));
}

pub fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn state_dir() -> Result<PathBuf> {
    let dirs = directories::BaseDirs::new().context("no home dir")?;
    #[cfg(target_os = "macos")]
    return Ok(dirs.home_dir().join("Library").join("Application Support").join("summon"));
    #[cfg(target_os = "windows")]
    return Ok(dirs.data_local_dir().join("summon"));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return Ok(dirs.home_dir().join(".local").join("share").join("summon"));
}

pub fn pid_file() -> Result<PathBuf> {
    Ok(state_dir()?.join("summon.pid"))
}

pub fn log_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let dirs = directories::BaseDirs::new().context("no home dir")?;
        return Ok(dirs.home_dir().join("Library").join("Logs").join("summon"));
    }
    #[cfg(not(target_os = "macos"))]
    return Ok(state_dir()?.join("Logs"));
}

#[cfg(target_os = "macos")]
pub fn launch_agent_plist() -> Result<PathBuf> {
    let dirs = directories::BaseDirs::new().context("no home dir")?;
    Ok(dirs
        .home_dir()
        .join("Library")
        .join("LaunchAgents")
        .join("dev.summon.daemon.plist"))
}

#[cfg(target_os = "macos")]
pub fn launch_agent_label() -> &'static str {
    "dev.summon.daemon"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_file_is_config_toml_in_summon_dir() {
        let p = config_file().unwrap();
        assert_eq!(p.file_name().unwrap(), "config.toml");
        assert!(p.parent().unwrap().ends_with("summon"), "got: {}", p.display());
    }

    #[test]
    fn pid_file_lives_in_state_dir() {
        let p = pid_file().unwrap();
        assert_eq!(p.file_name().unwrap(), "summon.pid");
        assert_eq!(p.parent().unwrap(), state_dir().unwrap());
    }

    #[test]
    fn log_dir_is_platform_appropriate() {
        let p = log_dir().unwrap();
        #[cfg(target_os = "macos")]
        assert!(p.ends_with("Logs/summon"), "got: {}", p.display());
        #[cfg(not(target_os = "macos"))]
        assert_eq!(p, state_dir().unwrap().join("Logs"));
    }
}
