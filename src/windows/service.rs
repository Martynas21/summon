/// Windows autostart via Task Scheduler — analogous to macOS LaunchAgents.
/// Uses `schtasks.exe` with an XML task definition; no elevation required for
/// user-level tasks.
use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const TASK_NAME: &str = "summon\\daemon";

pub fn install() -> Result<()> {
    let binary = current_binary_path()?;
    let xml = render_task_xml(&binary);

    // Write UTF-16 LE with BOM (required by schtasks /xml)
    let tmp = temp_xml_path();
    write_utf16_file(&tmp, &xml).context("writing task XML")?;

    let status = Command::new("schtasks")
        .args(["/create", "/xml", tmp.to_str().unwrap(), "/tn", TASK_NAME, "/f"])
        .status()
        .context("running schtasks /create")?;

    let _ = fs::remove_file(&tmp);

    if !status.success() {
        anyhow::bail!("schtasks /create failed (exit {})", status.code().unwrap_or(-1));
    }

    println!("summon installed as Task Scheduler task '{TASK_NAME}'.");
    println!("It will start automatically at next logon.");
    println!("Run `summon run` to start it now without logging off.");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let status = Command::new("schtasks")
        .args(["/delete", "/tn", TASK_NAME, "/f"])
        .status()
        .context("running schtasks /delete")?;

    if !status.success() {
        // Exit code 1 means the task didn't exist — treat as success
        eprintln!(
            "note: schtasks /delete returned {}, task may not have been registered",
            status.code().unwrap_or(-1)
        );
    } else {
        println!("summon Task Scheduler task removed.");
    }
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn current_binary_path() -> Result<PathBuf> {
    env::current_exe().context("resolving current executable path")
}

fn temp_xml_path() -> PathBuf {
    env::temp_dir().join("summon-task.xml")
}

fn render_task_xml(binary: &PathBuf) -> String {
    let exe = binary.display().to_string().replace('&', "&amp;").replace('<', "&lt;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <Delay>PT5S</Delay>
    </LogonTrigger>
  </Triggers>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>run</Arguments>
    </Exec>
  </Actions>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
  </Settings>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
</Task>"#,
        exe = exe
    )
}

/// Write `content` as UTF-16 LE with BOM, as required by `schtasks /xml`.
fn write_utf16_file(path: &PathBuf, content: &str) -> Result<()> {
    let mut bytes: Vec<u8> = Vec::with_capacity(content.len() * 2 + 2);
    // BOM: 0xFF 0xFE
    bytes.push(0xFF);
    bytes.push(0xFE);
    for c in content.encode_utf16() {
        bytes.push((c & 0xFF) as u8);
        bytes.push((c >> 8) as u8);
    }
    fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}
