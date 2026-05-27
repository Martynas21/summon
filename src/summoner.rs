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
        let running = match app::find_running(ident) {
            Some(a) => a,
            None => {
                info!(ident, "launching");
                app::launch_and_wait(ident, self.launch_timeout)
                    .with_context(|| format!("launching {ident}"))?
            }
        };
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
        let idx = self.cycle.advance(ident, wins.len());
        let pick = &wins[idx];
        if window::is_minimized(pick) {
            window::unminimize(pick);
        }
        window::raise(pick);
        app::activate(&running);
        info!(ident, idx, total = wins.len(), "summoned");
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn summon(&mut self, _ident: &str) -> Result<()> {
        anyhow::bail!("summon is macOS-only");
    }
}
