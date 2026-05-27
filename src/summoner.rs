use crate::config::ParsedConfig;
#[cfg(target_os = "macos")]
use crate::macos::{app, window};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Per-app cycle state — remembers the AXUIElementRef pointer of the window
/// we last raised. On the next press while the app is active we advance
/// from that window; while inactive we re-raise it. The cursor lives in
/// our own memory (not AX) because AX attribute reads lag our writes.
struct AppCursor {
    /// CGWindowID of the window we last raised. Stable across AX queries.
    last_window: Option<u32>,
    last_press: Instant,
}

pub struct Summoner {
    cursors: HashMap<String, AppCursor>,
    /// The most recent hotkey we processed and when. Used to detect
    /// "consecutive presses of the same hotkey" — only those count as cycle
    /// intent. Pressing Ctrl+1 → Ctrl+2 → Ctrl+1 must NOT cycle.
    last_press: Option<(String, Instant)>,
    launch_timeout: Duration,
    /// Max gap between two same-hotkey presses for the second to count as
    /// a cycle continuation.
    cycle_window: Duration,
}

impl Summoner {
    pub fn new(cfg: &ParsedConfig) -> Self {
        Self {
            cursors: HashMap::new(),
            last_press: None,
            launch_timeout: Duration::from_secs(5),
            cycle_window: derive_cycle_window(cfg.settings.cycle_reset_ms),
        }
    }

    pub fn reconfigure(&mut self, cfg: &ParsedConfig) {
        self.cycle_window = derive_cycle_window(cfg.settings.cycle_reset_ms);
        // Keep cursor map across reloads so the user doesn't lose their place.
    }

    #[cfg(target_os = "macos")]
    pub fn summon(&mut self, ident: &str) -> Result<()> {
        let (running, was_launched) = match app::find_running(ident) {
            Some(a) => (a, false),
            None => {
                info!(ident, "launching");
                let a = app::launch_and_wait(ident, self.launch_timeout)
                    .with_context(|| format!("launching {ident}"))?;
                (a, true)
            }
        };
        let pid = app::pid(&running);
        let resolved_bundle = app::bundle_id(&running).unwrap_or_default();
        let resolved_name = app::name(&running).unwrap_or_default();
        let was_active = !was_launched && app::is_active(&running);
        let frontmost_pid = app::frontmost_pid().unwrap_or(0);
        info!(
            ident,
            pid,
            bundle = %resolved_bundle,
            name = %resolved_name,
            was_active,
            frontmost_pid,
            "resolved app"
        );

        app::ensure_visible(&running);

        let app_el = match window::AppEl::for_pid(pid) {
            Some(e) => e,
            None => {
                warn!(ident, pid, "no AX element for app; activate only");
                app::activate(&running);
                return Ok(());
            }
        };
        let wins = window::windows(&app_el);
        if wins.is_empty() {
            warn!(ident, "no enumerable windows; activate only");
            app::activate(&running);
            return Ok(());
        }

        let now = Instant::now();
        let cycle_window = self.cycle_window;

        // "Cycle intent" = the immediately previous press was THIS same hotkey,
        // within cycle_window. Pressing a different hotkey resets the chain,
        // so Ctrl+1 → Ctrl+2 → Ctrl+1 returns the user to the same Ghostty
        // window they last had focused, not the next one.
        let is_rapid = matches!(
            &self.last_press,
            Some((prev_ident, prev_time))
                if prev_ident == ident && now.duration_since(*prev_time) <= cycle_window
        );

        let cursor = self.cursors.entry(ident.to_string()).or_insert(AppCursor {
            last_window: None,
            last_press: now,
        });
        let last_idx = cursor
            .last_window
            .and_then(|id| wins.iter().position(|w| w.window_id() == Some(id)));

        let idx = if is_rapid && last_idx.is_some() && wins.len() > 1 {
            (last_idx.unwrap() + 1) % wins.len()
        } else {
            last_idx.unwrap_or(0)
        };

        let pick = &wins[idx];
        if window::is_minimized(pick) {
            window::unminimize(pick);
        }
        window::focus(pick);
        window::raise(pick);
        if !was_active {
            app::activate(&running);
        }
        cursor.last_window = pick.window_id();
        cursor.last_press = now;
        self.last_press = Some((ident.to_string(), now));

        let picked_title = window::title(pick).unwrap_or_default();
        info!(
            ident,
            idx,
            total = wins.len(),
            was_active,
            is_rapid,
            had_last = last_idx.is_some(),
            picked = %picked_title,
            "summoned"
        );
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn summon(&mut self, _ident: &str) -> Result<()> {
        anyhow::bail!("summon is macOS-only");
    }
}

fn derive_cycle_window(configured_ms: u64) -> Duration {
    // 0 = "use the sensible default" (1500ms). Any positive value overrides.
    if configured_ms == 0 {
        Duration::from_millis(1500)
    } else {
        Duration::from_millis(configured_ms)
    }
}
