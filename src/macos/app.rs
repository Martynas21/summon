use anyhow::{anyhow, Context, Result};
use objc2::rc::Retained;
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use objc2_foundation::NSString;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

/// A bundle id contains a dot and no whitespace (com.foo.bar).
pub fn looks_like_bundle_id(s: &str) -> bool {
    s.contains('.') && !s.chars().any(char::is_whitespace)
}

/// Try bundle-id match first (if it looks like one), then localized-name fallback.
pub fn find_running(ident: &str) -> Option<Retained<NSRunningApplication>> {
    if looks_like_bundle_id(ident) {
        if let Some(app) = find_by_bundle_id(ident) {
            return Some(app);
        }
    }
    find_by_name(ident)
}

pub fn find_by_bundle_id(bid: &str) -> Option<Retained<NSRunningApplication>> {
    // System-side filtered lookup beats iterating all running apps. O(1) vs
    // O(N) over the runningApplications array. Returns matches across all
    // instances (Chrome can have several PIDs); we take the first.
    let ns = NSString::from_str(bid);
    let arr = unsafe { NSRunningApplication::runningApplicationsWithBundleIdentifier(&ns) };
    if arr.count() == 0 {
        None
    } else {
        Some(unsafe { arr.objectAtIndex(0) })
    }
}

pub fn find_by_name(name: &str) -> Option<Retained<NSRunningApplication>> {
    for_each_app(|app| {
        unsafe { app.localizedName() }
            .map(|ns| ns.to_string().eq_ignore_ascii_case(name))
            .unwrap_or(false)
    })
}

fn for_each_app<F>(mut matches: F) -> Option<Retained<NSRunningApplication>>
where
    F: FnMut(&NSRunningApplication) -> bool,
{
    let workspace = unsafe { NSWorkspace::sharedWorkspace() };
    let apps = unsafe { workspace.runningApplications() };
    let n = apps.count();
    for i in 0..n {
        let app: Retained<NSRunningApplication> = unsafe { apps.objectAtIndex(i) };
        if matches(&app) {
            return Some(app);
        }
    }
    None
}

/// Launch (or just activate) app via `/usr/bin/open`. Bundle id → `-b`,
/// display name → `-a`. Returns once `open` exits (it's fast; spawns and
/// returns).
pub fn launch(ident: &str) -> Result<()> {
    let mut cmd = Command::new("/usr/bin/open");
    if looks_like_bundle_id(ident) {
        cmd.arg("-b").arg(ident);
    } else {
        cmd.arg("-a").arg(ident);
    }
    let status = cmd
        .status()
        .with_context(|| format!("spawning open for {ident}"))?;
    if !status.success() {
        return Err(anyhow!("`open` for '{}' exited with {}", ident, status));
    }
    Ok(())
}

/// Launch + poll until the app appears in runningApplications, or timeout.
pub fn launch_and_wait(
    ident: &str,
    timeout: Duration,
) -> Result<Retained<NSRunningApplication>> {
    launch(ident)?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(app) = find_running(ident) {
            return Ok(app);
        }
        if Instant::now() >= deadline {
            return Err(anyhow!("timed out waiting for '{}' to launch", ident));
        }
        sleep(Duration::from_millis(50));
    }
}

/// Un-hide (Cmd+H) if currently hidden.
pub fn ensure_visible(app: &NSRunningApplication) {
    unsafe {
        if app.isHidden() {
            app.unhide();
        }
    }
}

/// Bring app forward; macOS handles Space-switching per Mission Control settings.
pub fn activate(app: &NSRunningApplication) {
    // NSApplicationActivateAllWindows (1<<0) | NSApplicationActivateIgnoringOtherApps (1<<1)
    let opts = NSApplicationActivationOptions(1 | 2);
    unsafe {
        let _ = app.activateWithOptions(opts);
    }
}

pub fn pid(app: &NSRunningApplication) -> i32 {
    unsafe { app.processIdentifier() }
}

/// Hide app (Cmd+H equivalent). All windows go away; app stays running.
/// Returns the API's bool result so callers can log/trace it.
pub fn hide(app: &NSRunningApplication) -> bool {
    unsafe { app.hide() }
}

/// Look up the NSRunningApplication for a given pid.
pub fn for_pid(pid: i32) -> Option<Retained<NSRunningApplication>> {
    unsafe { NSRunningApplication::runningApplicationWithProcessIdentifier(pid) }
}

/// PID of whichever app currently has the frontmost window. Callers needing
/// "is this app active" should compare this against the app's PID directly,
/// reusing a single `frontmost_pid()` call rather than making two ObjC
/// round-trips per press.
///
/// Note: `NSRunningApplication.isActive()` is KVO-driven from the main
/// runloop and can be stale on worker threads — always go through this.
pub fn frontmost_pid() -> Option<i32> {
    let ws = unsafe { NSWorkspace::sharedWorkspace() };
    unsafe { ws.frontmostApplication() }.map(|a| unsafe { a.processIdentifier() })
}

pub fn bundle_id(app: &NSRunningApplication) -> Option<String> {
    unsafe { app.bundleIdentifier() }.map(|ns| ns.to_string())
}

pub fn name(app: &NSRunningApplication) -> Option<String> {
    unsafe { app.localizedName() }.map(|ns| ns.to_string())
}
