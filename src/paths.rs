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
    Ok(dirs.home_dir().join("Library").join("Application Support").join("summon"))
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
    fn log_dir_is_in_user_library_logs() {
        let p = log_dir().unwrap();
        assert!(p.ends_with("Logs/summon"), "got: {}", p.display());
    }
}
