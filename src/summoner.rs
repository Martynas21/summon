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
    last_window: Option<usize>,
    last_press: Instant,
}

pub struct Summoner {
    cursors: HashMap<String, AppCursor>,
    launch_timeout: Duration,
    cycle_reset_ms: u64,
}

impl Summoner {
    pub fn new(cfg: &ParsedConfig) -> Self {
        Self {
            cursors: HashMap::new(),
            launch_timeout: Duration::from_secs(5),
            cycle_reset_ms: cfg.settings.cycle_reset_ms,
        }
    }

    pub fn reconfigure(&mut self, cfg: &ParsedConfig) {
        self.cycle_reset_ms = cfg.settings.cycle_reset_ms;
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
        let reset_ms = self.cycle_reset_ms;
        let cursor = self.cursors.entry(ident.to_string()).or_insert(AppCursor {
            last_window: None,
            last_press: now,
        });
        if reset_ms > 0
            && now.duration_since(cursor.last_press) > Duration::from_millis(reset_ms)
        {
            cursor.last_window = None;
        }
        let last_idx = cursor
            .last_window
            .and_then(|p| wins.iter().position(|w| w.id() == p));

        let idx = if was_active && wins.len() > 1 {
            match last_idx {
                Some(i) => (i + 1) % wins.len(),
                None => 0,
            }
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
        cursor.last_window = Some(pick.id());
        cursor.last_press = now;

        let picked_title = window::title(pick).unwrap_or_default();
        info!(
            ident,
            idx,
            total = wins.len(),
            was_active,
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
