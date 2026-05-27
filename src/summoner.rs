use crate::config::ParsedConfig;
use crate::cycle_state::CycleState;
#[cfg(target_os = "macos")]
use crate::macos::{app, window};
use anyhow::{Context, Result};
use std::time::Duration;
use tracing::{info, warn};

pub struct Summoner {
    cycle: CycleState,
    launch_timeout: Duration,
}

impl Summoner {
    pub fn new(cfg: &ParsedConfig) -> Self {
        Self {
            cycle: CycleState::new(cfg.settings.cycle_reset_ms),
            launch_timeout: Duration::from_secs(5),
        }
    }

    pub fn reconfigure(&mut self, cfg: &ParsedConfig) {
        self.cycle = CycleState::new(cfg.settings.cycle_reset_ms);
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
        // Snapshot active-state *before* we touch anything (activate flips it).
        let was_active = !was_launched && app::is_active(&running);

        app::ensure_visible(&running);

        let pid = app::pid(&running);
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
        // Already-frontmost → cycle to next. Otherwise just re-raise the most
        // recently summoned window so back-and-forth (Ctrl+Shift+1, +2, +1)
        // returns to a stable window.
        let idx = if was_active {
            self.cycle.advance(ident, wins.len())
        } else {
            self.cycle.touch(ident, wins.len())
        };
        let pick = &wins[idx];
        if window::is_minimized(pick) {
            window::unminimize(pick);
        }
        window::raise(pick);
        app::activate(&running);
        info!(ident, idx, total = wins.len(), was_active, "summoned");
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn summon(&mut self, _ident: &str) -> Result<()> {
        anyhow::bail!("summon is macOS-only");
    }
}
