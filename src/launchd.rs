use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const LABEL: &str = "dev.summon.daemon";
const GRANT_LABEL: &str = "dev.summon.grant";
const GRANT_STATUS_PATH: &str = "/tmp/summon-grant.status";
const GRANT_PROMPTED_PATH: &str = "/tmp/summon-grant.prompted";

pub fn install() -> Result<()> {
    let binary = std::env::current_exe().context("locating current executable")?;

    #[cfg(target_os = "macos")]
    {
        prompt_for_grant_via_launchd(&binary).map_err(|e| {
            anyhow!(
                "Accessibility grant not confirmed: {e}\n\n\
                 If the modal never appeared, add the binary manually:\n\
                 System Settings → Privacy & Security → Accessibility → +\n  {}",
                binary.display()
            )
        })?;
    }

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

/// `summon _grant` entry point. Spawned by launchd via a temporary plist,
/// so its parent is launchd (not Ghostty) and `request_trust(true)` fires
/// the TCC modal instead of being short-circuited by parent attribution.
///
/// Single-shot: checks once and exits. Writes the result to a status file
/// the install loop reads, then re-bootstraps for the next check. We must
/// check from a fresh process each time because TCC's "is trusted" answer
/// is cached for the lifetime of the calling process at the first call.
pub fn grant() -> Result<()> {
    let _ = std::fs::remove_file(GRANT_STATUS_PATH);

    #[cfg(target_os = "macos")]
    let trusted = {
        let trusted_now = crate::macos::permissions::is_trusted();
        // Fire the modal exactly once across the install's polling loop.
        // The sentinel prevents subsequent helper invocations from
        // re-popping it after the user has dismissed but before they grant.
        if !trusted_now && !std::path::Path::new(GRANT_PROMPTED_PATH).exists() {
            let _ = crate::macos::permissions::request_trust();
            let _ = std::fs::write(GRANT_PROMPTED_PATH, "");
        }
        trusted_now
    };
    #[cfg(not(target_os = "macos"))]
    let trusted = true;

    let outcome = if trusted { "ok" } else { "fail" };
    std::fs::write(GRANT_STATUS_PATH, outcome)
        .with_context(|| format!("writing {GRANT_STATUS_PATH}"))?;
    Ok(())
}

/// Loop bootstrapping the `_grant` helper until it reports trusted, or we
/// time out. Each iteration is a fresh launchd-spawned process so TCC's
/// per-process trust cache doesn't mask grants made mid-loop.
#[cfg(target_os = "macos")]
fn prompt_for_grant_via_launchd(binary: &Path) -> Result<()> {
    let _ = std::fs::remove_file(GRANT_STATUS_PATH);
    let _ = std::fs::remove_file(GRANT_PROMPTED_PATH);

    let plist_path = std::env::temp_dir().join(format!("{GRANT_LABEL}.plist"));
    let plist = render_grant_plist(GRANT_LABEL, binary);
    std::fs::write(&plist_path, plist)
        .with_context(|| format!("writing {}", plist_path.display()))?;

    println!("checking Accessibility grant (a modal will appear if not granted)…");

    let deadline = Instant::now() + Duration::from_secs(180);
    let mut prompted_user = false;
    let granted = loop {
        match run_grant_helper(&plist_path) {
            Ok(s) if s == "ok" => break true,
            Ok(_) => {
                if !prompted_user {
                    println!(
                        "Modal opened. Toggle on `summon` in System Settings → \n\
                         Privacy & Security → Accessibility. Waiting up to 3 minutes…"
                    );
                    prompted_user = true;
                }
            }
            Err(e) => eprintln!("grant helper invocation failed: {e:#}"),
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(2000));
    };

    let _ = std::fs::remove_file(&plist_path);
    let _ = std::fs::remove_file(GRANT_STATUS_PATH);
    let _ = std::fs::remove_file(GRANT_PROMPTED_PATH);

    if granted {
        println!("Accessibility granted.");
        Ok(())
    } else {
        Err(anyhow!("user did not grant within timeout"))
    }
}

/// Bootstrap a one-shot `_grant` helper, wait for it to write its status
/// file (typically <1s), then bootout. Returns the helper's outcome.
#[cfg(target_os = "macos")]
fn run_grant_helper(plist_path: &Path) -> Result<String> {
    let _ = std::fs::remove_file(GRANT_STATUS_PATH);
    let _ = bootout(plist_path);
    bootstrap(plist_path).context("bootstrapping grant helper")?;

    let deadline = Instant::now() + Duration::from_secs(10);
    let outcome = loop {
        if let Ok(s) = std::fs::read_to_string(GRANT_STATUS_PATH) {
            break Some(s.trim().to_string());
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let _ = bootout(plist_path);
    outcome.ok_or_else(|| anyhow!("helper produced no status"))
}

#[cfg(target_os = "macos")]
fn render_grant_plist(label: &str, binary: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{bin}</string>
    <string>_grant</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#,
        label = label,
        bin = escape_xml(binary),
    )
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
    // Silence stderr: pre-install bootout always errors (errno 5) when
    // nothing is loaded, which is the common case. Real failures still
    // surface via the non-zero exit status.
    let status = Command::new("/bin/launchctl")
        .arg("bootout")
        .arg(&domain)
        .arg(plist_path)
        .stderr(std::process::Stdio::null())
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
