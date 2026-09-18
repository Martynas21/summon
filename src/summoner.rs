use crate::config::ParsedConfig;
use crate::{app, screen, window};
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
    /// Max gap between two same-hotkey presses for the second to count as
    /// a cycle continuation.
    cycle_window: Duration,
    hide_previous: bool,
    /// `Some(d)` enables hold-to-minimize after `d`. `None` disables it and
    /// keeps summon firing on key Press.
    hold_threshold: Option<Duration>,
}

impl Summoner {
    pub fn new(cfg: &ParsedConfig) -> Self {
        Self {
            cursors: HashMap::new(),
            last_press: None,
            cycle_window: derive_cycle_window(cfg.settings.cycle_reset_ms),
            hide_previous: cfg.settings.hide_previous,
            hold_threshold: derive_hold_threshold(cfg.settings.hold_threshold_ms),
        }
    }

    pub fn reconfigure(&mut self, cfg: &ParsedConfig) {
        self.cycle_window = derive_cycle_window(cfg.settings.cycle_reset_ms);
        self.hide_previous = cfg.settings.hide_previous;
        self.hold_threshold = derive_hold_threshold(cfg.settings.hold_threshold_ms);
        // Keep cursor map across reloads so the user doesn't lose their place.
    }

    pub fn hold_threshold(&self) -> Option<Duration> {
        self.hold_threshold
    }

    fn cursor_entry(&mut self, key: String, now: Instant) -> &mut AppCursor {
        self.cursors.entry(key).or_insert(AppCursor {
            last_window: None,
            last_press: now,
        })
    }

    pub fn summon(&mut self, ident: &str, cmdline_filter: Option<&str>) -> Result<()> {
        let running = match app::find_running_filtered(ident, cmdline_filter) {
            Some(a) => a,
            None => {
                // Fire-and-forget: `open` itself activates the app. Blocking
                // here on launch_and_wait freezes the worker for up to 5s,
                // causing every other hotkey press to queue behind it.
                // The user's next press of this hotkey will be the cycle/focus
                // path once the app is in runningApplications.
                //
                // When a cmdline_filter is set we still launch — but a fresh
                // `open` will likely not produce a process matching the filter
                // (those are typically spawned by Playwright/MCP, not the
                // Launch Services route). Logged distinctly so users can spot
                // the misconfiguration.
                if cmdline_filter.is_some() {
                    info!(ident, filter = ?cmdline_filter, "no PID matched cmdline filter; launching anyway");
                } else {
                    info!(ident, "launching (fire-and-forget)");
                }
                app::launch(ident)
                    .with_context(|| format!("launching {ident}"))?;
                return Ok(());
            }
        };
        let pid = app::pid(&running);
        let resolved_bundle = app::bundle_id(&running).unwrap_or_default();
        let resolved_name = app::name(&running).unwrap_or_default();
        // One NSWorkspace round-trip serves both was_active and the log
        // field; the previous version called frontmostApplication twice.
        let frontmost_pid = app::frontmost_pid().unwrap_or(0);
        let was_active = frontmost_pid == pid;
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

        // Cursors keyed by resolved app identity (bundle id, name fallback)
        // rather than the user's binding string. Two bindings to the same
        // app share window-history.
        let key = cursor_key(&resolved_bundle, &resolved_name);
        let cursor_last = self.cursors.get(&key).and_then(|c| c.last_window);
        let last_idx = cursor_last
            .and_then(|id| wins.iter().position(|w| w.window_id() == Some(id)));

        let active = screen::active_display();
        let idx = pick_window_idx(
            &wins,
            last_idx,
            is_rapid,
            |w| window::is_minimized(w),
            |w| screen::window_display(w) == Some(active),
        );

        let pick = &wins[idx];
        let was_minimized = window::is_minimized(pick);
        let target_display = screen::window_display(pick);

        // Same-app cycle: minimize the window we just advanced from. Without
        // this, cycling within a multi-window app stacks visible windows on
        // top of each other instead of swapping. Order: minimize prev BEFORE
        // raising new pick to avoid pop-then-tuck flicker. The cross-app
        // hide_previous block below does NOT cover this — it's gated on
        // frontmost_pid != pid.
        if is_rapid
            && self.hide_previous
            && wins.len() > 1
            && last_idx.is_some()
            && last_idx != Some(idx)
        {
            let prev_win = &wins[last_idx.unwrap()];
            if !window::is_minimized(prev_win) {
                window::minimize(prev_win);
            }
        }

        // hide_previous fires only when the *picked window* was minimized
        // before this press — i.e. we are actually surfacing something
        // hidden. Picking an already-visible window means the user is just
        // switching focus and should keep typing without prev getting
        // cleared. Scope is the picked window's display: minimizing on a
        // display where the target isn't appearing would be pointless.
        // Order: minimize prev BEFORE raising target to avoid pop-then-tuck
        // flicker. Captured before taking the cursor mutable borrow below so
        // we can also update the prev app's cursor.
        if was_minimized
            && !was_active
            && self.hide_previous
            && frontmost_pid > 0
            && frontmost_pid != pid
        {
            let prev_running = app::for_pid(frontmost_pid);
            let prev_bid = prev_running
                .as_ref()
                .and_then(|a| app::bundle_id(a))
                .unwrap_or_default();
            let prev_name = prev_running
                .as_ref()
                .and_then(|a| app::name(a))
                .unwrap_or_default();
            if app::is_persistent_shell(&prev_bid, &prev_name) {
                info!(prev_pid = frontmost_pid, "skip minimize: persistent shell");
            } else if target_display.is_none() {
                warn!(
                    prev_pid = frontmost_pid,
                    "skip minimize: target display unknown"
                );
            } else if let Some(prev_app_el) = window::AppEl::for_pid(frontmost_pid) {
                let prev_wins = window::windows(&prev_app_el);
                let target_disp = target_display.unwrap();
                // Topmost non-minimized window on the target's display = the
                // user's last-active there. Captured BEFORE minimize so a
                // future return restores that window rather than the stale
                // cursor.
                let user_active = prev_wins
                    .iter()
                    .find(|w| {
                        !window::is_minimized(w)
                            && screen::window_display(w) == Some(target_disp)
                    })
                    .and_then(|w| w.window_id());
                let mut minimized = 0usize;
                for w in &prev_wins {
                    if window::is_minimized(w) {
                        continue;
                    }
                    if screen::window_display(w) != Some(target_disp) {
                        continue;
                    }
                    window::minimize(w);
                    minimized += 1;
                }
                if let Some(id) = user_active {
                    let prev_key = cursor_key(&prev_bid, &prev_name);
                    self.cursor_entry(prev_key, now).last_window = Some(id);
                }
                info!(
                    prev_pid = frontmost_pid,
                    bundle = %prev_bid,
                    total = prev_wins.len(),
                    minimized,
                    target_display = target_disp,
                    user_active = ?user_active,
                    "minimized previous (target-display scope)"
                );
            } else {
                warn!(prev_pid = frontmost_pid, "minimize: no AX element for prev pid");
            }
        }

        if was_minimized {
            window::unminimize(pick);
        }
        window::focus(pick);
        window::raise(pick);
        if !was_active {
            app::activate(&running);
        }

        let cursor = self.cursor_entry(key, now);
        cursor.last_window = pick.window_id();
        cursor.last_press = now;
        self.last_press = Some((ident.to_string(), now));

        // `window::title` is a synchronous AX IPC (~1-5ms). Only pay the
        // cost when DEBUG logging is enabled; the info line below stays
        // useful without the title field.
        if tracing::enabled!(tracing::Level::DEBUG) {
            let picked_title = window::title(pick).unwrap_or_default();
            tracing::debug!(ident, picked = %picked_title, "picked window");
        }
        info!(
            ident,
            idx,
            total = wins.len(),
            was_active,
            is_rapid,
            was_minimized,
            target_display = ?target_display,
            had_last = last_idx.is_some(),
            "summoned"
        );
        Ok(())
    }

    /// Minimize `ident`'s frontmost window **on the display the user is
    /// working on**. No focus change, no activation, no cycle-state mutation.
    /// No-op if the app isn't running, has no enumerable windows, or has
    /// nothing visible on the active display — a hold must never reach across
    /// to a window the user can't see.
    pub fn minimize_on_active_display(
        &mut self,
        ident: &str,
        cmdline_filter: Option<&str>,
    ) -> Result<()> {
        let running = match app::find_running_filtered(ident, cmdline_filter) {
            Some(a) => a,
            None => {
                info!(ident, "minimize: app not running");
                return Ok(());
            }
        };
        let pid = app::pid(&running);
        let app_el = match window::AppEl::for_pid(pid) {
            Some(e) => e,
            None => {
                warn!(ident, pid, "minimize: no AX element");
                return Ok(());
            }
        };
        let wins = window::windows(&app_el);
        if wins.is_empty() {
            info!(ident, "minimize: no enumerable windows");
            return Ok(());
        }
        let active = screen::active_display();
        let pick = pick_minimize_idx(
            &wins,
            |w| window::is_minimized(w),
            |w| screen::window_display(w) == Some(active),
        );
        let Some(idx) = pick else {
            info!(
                ident,
                total = wins.len(),
                "minimize: nothing visible on active display"
            );
            return Ok(());
        };
        window::minimize(&wins[idx]);
        info!(ident, pid, idx, total = wins.len(), "minimized frontmost on active display");
        Ok(())
    }


}

/// Pick which window to raise/focus.
///
/// Non-cycling priority, applied in order:
///   1. Visible window on the active display — user pressed there and
///      this app is already visible there; just focus it.
///   2. Cursor's last-raised (`last_idx`) IF on active display — preserves
///      the user's history on this monitor. Critical when hide_previous
///      has minimized multiple windows of the target app on this display:
///      enumeration order would pick arbitrarily; cursor remembers which
///      one was actually theirs.
///   3. Any minimized window on the active display — fresh fallback when
///      no cursor history points here (e.g. user pressed on B, target is
///      min'd on B, but they've never summoned it before).
///   4. Cursor's last-raised on any other display — target is fully absent
///      from active display; restore it where it was.
///   5. First visible window anywhere — no cursor, target not on active
///      display, but visible somewhere.
///   6. Index 0 — last-resort fallback.
///
/// Cycle (`is_rapid` same hotkey) bypasses priority and advances the cursor
/// through ALL windows (minimized picks get unminimized by the caller).
fn pick_window_idx<W>(
    wins: &[W],
    last_idx: Option<usize>,
    is_rapid: bool,
    is_minimized: impl Fn(&W) -> bool,
    on_active_display: impl Fn(&W) -> bool,
) -> usize {
    if is_rapid && last_idx.is_some() && wins.len() > 1 {
        return (last_idx.unwrap() + 1) % wins.len();
    }
    let on_active_visible = wins
        .iter()
        .position(|w| !is_minimized(w) && on_active_display(w));
    let cursor_on_active = last_idx.filter(|&i| on_active_display(&wins[i]));
    let on_active_minimized = wins
        .iter()
        .position(|w| is_minimized(w) && on_active_display(w));
    let any_visible_idx = wins.iter().position(|w| !is_minimized(w));
    on_active_visible
        .or(cursor_on_active)
        .or(on_active_minimized)
        .or(last_idx)
        .or(any_visible_idx)
        .unwrap_or(0)
}

/// Frontmost non-minimized window on the active display, or None when the app
/// has nothing visible there.
///
/// Hold-to-minimize uses this instead of "window 0": enumeration order spans
/// every display, so the positional front window is often on another monitor.
/// None means "do nothing" — either this display's window is already minimized
/// or the app isn't here at all, and in both cases the user sees no change.
fn pick_minimize_idx<W>(
    wins: &[W],
    is_minimized: impl Fn(&W) -> bool,
    on_active_display: impl Fn(&W) -> bool,
) -> Option<usize> {
    wins.iter()
        .position(|w| !is_minimized(w) && on_active_display(w))
}

fn cursor_key(bundle_id: &str, name: &str) -> String {
    if bundle_id.is_empty() { name } else { bundle_id }.to_string()
}

fn derive_cycle_window(configured_ms: u64) -> Duration {
    // 0 = "use the sensible default" (1500ms). Any positive value overrides.
    Duration::from_millis(if configured_ms == 0 { 1500 } else { configured_ms })
}

/// 0 means "disabled — fire summon on press, ignore release". Any positive
/// value enables hold detection at that millisecond threshold.
fn derive_hold_threshold(configured_ms: u64) -> Option<Duration> {
    (configured_ms != 0).then(|| Duration::from_millis(configured_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ParsedConfig, Settings};

    fn cfg_with(settings: Settings) -> ParsedConfig {
        ParsedConfig { settings, bindings: vec![] }
    }

    // --- derive_hold_threshold ---

    #[test]
    fn hold_threshold_zero_is_disabled() {
        assert_eq!(derive_hold_threshold(0), None);
    }

    #[test]
    fn hold_threshold_positive_enables() {
        assert_eq!(derive_hold_threshold(200), Some(Duration::from_millis(200)));
    }

    // --- derive_cycle_window ---

    #[test]
    fn cycle_window_zero_uses_default() {
        assert_eq!(derive_cycle_window(0), Duration::from_millis(1500));
    }

    #[test]
    fn cycle_window_positive_overrides_default() {
        assert_eq!(derive_cycle_window(300), Duration::from_millis(300));
    }

    // --- cursor_key ---

    #[test]
    fn cursor_key_prefers_bundle_id_over_name() {
        assert_eq!(cursor_key("com.apple.Safari", "Safari"), "com.apple.Safari");
    }

    #[test]
    fn cursor_key_falls_back_to_name_when_bundle_id_empty() {
        assert_eq!(cursor_key("", "Firefox"), "Firefox");
    }

    #[test]
    fn cursor_key_both_empty_gives_empty_string() {
        assert_eq!(cursor_key("", ""), "");
    }

    // --- pick_window_idx ---

    /// Windows modelled as (is_minimized, on_active_display) tuples.
    fn pick(wins: &[(bool, bool)], last_idx: Option<usize>, is_rapid: bool) -> usize {
        pick_window_idx(wins, last_idx, is_rapid, |w| w.0, |w| w.1)
    }

    #[test]
    fn rapid_cycle_advances_past_cursor_and_wraps() {
        let wins = [(false, true), (false, true), (false, true)];
        assert_eq!(pick(&wins, Some(0), true), 1);
        assert_eq!(pick(&wins, Some(2), true), 0);
    }

    #[test]
    fn rapid_without_cursor_falls_back_to_priority() {
        let wins = [(true, false), (false, true)];
        assert_eq!(pick(&wins, None, true), 1);
    }

    #[test]
    fn rapid_with_single_window_does_not_cycle() {
        let wins = [(false, true)];
        assert_eq!(pick(&wins, Some(0), true), 0);
    }

    #[test]
    fn visible_on_active_display_beats_cursor() {
        // Cursor points at index 2 (also on active), but index 1 is the
        // first visible window on the active display.
        let wins = [(true, true), (false, true), (false, true)];
        assert_eq!(pick(&wins, Some(2), false), 1);
    }

    #[test]
    fn cursor_on_active_display_beats_minimized_on_active() {
        // No visible window on active; cursor (minimized, on active) wins
        // over the earlier minimized-on-active candidate.
        let wins = [(true, true), (true, true), (false, false)];
        assert_eq!(pick(&wins, Some(1), false), 1);
    }

    #[test]
    fn minimized_on_active_display_beats_offscreen_cursor() {
        let wins = [(false, false), (true, true)];
        assert_eq!(pick(&wins, Some(0), false), 1);
    }

    #[test]
    fn cursor_off_active_display_beats_any_visible() {
        // Nothing on the active display at all; restore the cursor's window
        // where it was rather than the first visible one.
        let wins = [(false, false), (true, false)];
        assert_eq!(pick(&wins, Some(1), false), 1);
    }

    #[test]
    fn any_visible_window_when_no_cursor() {
        let wins = [(true, false), (false, false)];
        assert_eq!(pick(&wins, None, false), 1);
    }

    #[test]
    fn defaults_to_first_window_when_all_minimized_off_display() {
        let wins = [(true, false), (true, false)];
        assert_eq!(pick(&wins, None, false), 0);
    }

    // --- pick_minimize_idx ---

    /// Same (is_minimized, on_active_display) modelling as `pick`.
    fn pick_min(wins: &[(bool, bool)]) -> Option<usize> {
        pick_minimize_idx(wins, |w| w.0, |w| w.1)
    }

    #[test]
    fn minimize_picks_first_visible_window_on_active_display() {
        let wins = [(true, true), (false, true), (false, true)];
        assert_eq!(pick_min(&wins), Some(1));
    }

    #[test]
    fn minimize_skips_visible_windows_on_other_displays() {
        let wins = [(false, false), (false, true)];
        assert_eq!(pick_min(&wins), Some(1));
    }

    #[test]
    fn minimize_does_nothing_when_active_display_window_already_minimized() {
        let wins = [(true, true)];
        assert_eq!(pick_min(&wins), None);
    }

    #[test]
    fn minimize_does_nothing_when_app_is_only_on_another_display() {
        let wins = [(false, false), (true, false)];
        assert_eq!(pick_min(&wins), None);
    }

    #[test]
    fn minimize_does_nothing_without_windows() {
        assert_eq!(pick_min(&[]), None);
    }

    // --- Summoner::new ---

    #[test]
    fn new_reads_hide_previous_from_config() {
        let s = Summoner::new(&cfg_with(Settings { hide_previous: true, ..Default::default() }));
        assert!(s.hide_previous);
    }

    #[test]
    fn new_hold_threshold_enabled_by_default() {
        let s = Summoner::new(&cfg_with(Settings::default()));
        assert_eq!(s.hold_threshold(), Some(Duration::from_millis(200)));
    }

    #[test]
    fn new_reads_hold_threshold_from_config() {
        let s = Summoner::new(&cfg_with(Settings { hold_threshold_ms: 250, ..Default::default() }));
        assert_eq!(s.hold_threshold(), Some(Duration::from_millis(250)));
    }

    // --- Summoner::reconfigure ---

    #[test]
    fn reconfigure_updates_hold_threshold() {
        let mut s = Summoner::new(&cfg_with(Settings::default()));
        assert_eq!(s.hold_threshold(), Some(Duration::from_millis(200)));
        s.reconfigure(&cfg_with(Settings { hold_threshold_ms: 100, ..Default::default() }));
        assert_eq!(s.hold_threshold(), Some(Duration::from_millis(100)));
    }

    #[test]
    fn reconfigure_updates_hide_previous() {
        let mut s = Summoner::new(&cfg_with(Settings { hide_previous: false, ..Default::default() }));
        assert!(!s.hide_previous);
        s.reconfigure(&cfg_with(Settings { hide_previous: true, ..Default::default() }));
        assert!(s.hide_previous);
    }
}
