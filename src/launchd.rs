use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const LABEL: &str = "dev.summon.daemon";

pub fn install() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        if !crate::macos::permissions::is_trusted() {
            return Err(anyhow!(
                "refusing to install: Accessibility permission not granted.\n\
                 Run `summon run` once to trigger the prompt + grant access, then retry `summon install`."
            ));
        }
    }

    let binary = std::env::current_exe().context("locating current executable")?;
    let log_dir = crate::paths::log_dir()?;
    std::fs::create_dir_all(&log_dir)?;
    let stdout = log_dir.join("stdout.log");
    let stderr = log_dir.join("stderr.log");

    let plist_path = crate::paths::launch_agent_plist()?;
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let plist = render_plist(LABEL, &binary, &stdout, &stderr);
    std::fs::write(&plist_path, plist)
        .with_context(|| format!("writing {}", plist_path.display()))?;

    // If a previous instance is loaded, bootout first to allow a clean bootstrap.
    let _ = bootout(&plist_path);
    bootstrap(&plist_path)?;
    println!(
        "installed LaunchAgent at {} (label {LABEL}); daemon should now be running",
        plist_path.display()
    );
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let plist_path = crate::paths::launch_agent_plist()?;
    if plist_path.exists() {
        let _ = bootout(&plist_path);
        std::fs::remove_file(&plist_path)
            .with_context(|| format!("removing {}", plist_path.display()))?;
        println!("removed LaunchAgent at {}", plist_path.display());
    } else {
        println!("no LaunchAgent at {}", plist_path.display());
    }
    Ok(())
}

fn bootstrap(plist_path: &Path) -> Result<()> {
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let status = Command::new("/bin/launchctl")
        .arg("bootstrap")
        .arg(&domain)
        .arg(plist_path)
        .status()
        .context("running launchctl bootstrap")?;
    if !status.success() {
        return Err(anyhow!(
            "launchctl bootstrap {} {} exited with {}",
            domain,
            plist_path.display(),
            status
        ));
    }
    Ok(())
}

fn bootout(plist_path: &Path) -> Result<()> {
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let status = Command::new("/bin/launchctl")
        .arg("bootout")
        .arg(&domain)
        .arg(plist_path)
        .status()
        .context("running launchctl bootout")?;
    if !status.success() {
        return Err(anyhow!("launchctl bootout exited with {}", status));
    }
    Ok(())
}

fn render_plist(label: &str, binary: &Path, stdout: &Path, stderr: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{bin}</string>
    <string>run</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>{out}</string>
  <key>StandardErrorPath</key><string>{err}</string>
</dict>
</plist>
"#,
        label = label,
        bin = escape_xml(binary),
        out = escape_xml(stdout),
        err = escape_xml(stderr),
    )
}

fn escape_xml(p: &Path) -> String {
    p.display()
        .to_string()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Test helper (and useful for `summon status` later): expected plist path.
#[allow(dead_code)]
pub fn plist_path() -> Result<PathBuf> {
    crate::paths::launch_agent_plist()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn renders_plist_with_paths() {
        let s = render_plist(
            "dev.summon.daemon",
            &PathBuf::from("/usr/local/bin/summon"),
            &PathBuf::from("/tmp/out.log"),
            &PathBuf::from("/tmp/err.log"),
        );
        assert!(s.contains("<string>dev.summon.daemon</string>"));
        assert!(s.contains("<string>/usr/local/bin/summon</string>"));
        assert!(s.contains("<string>/tmp/out.log</string>"));
        assert!(s.contains("<string>/tmp/err.log</string>"));
        assert!(s.contains("<key>RunAtLoad</key><true/>"));
    }

    #[test]
    fn escapes_ampersand_in_path() {
        let s = render_plist(
            "x",
            &PathBuf::from("/a&b/summon"),
            &PathBuf::from("/x"),
            &PathBuf::from("/y"),
        );
        assert!(s.contains("/a&amp;b/summon"));
    }
}
