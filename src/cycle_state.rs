use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Per-app cursor for cycling through windows on repeated hotkey presses.
#[derive(Debug, Default)]
pub struct CycleState {
    entries: HashMap<String, Entry>,
    reset_after: Option<Duration>,
}

#[derive(Debug, Clone)]
struct Entry {
    cursor: usize,
    last_press: Instant,
}

impl CycleState {
    pub fn new(reset_after_ms: u64) -> Self {
        Self {
            entries: HashMap::new(),
            reset_after: if reset_after_ms == 0 {
                None
            } else {
                Some(Duration::from_millis(reset_after_ms))
            },
        }
    }

    /// Advance the cursor for `app`. Returns the next index, modulo `window_count`.
    /// Resets to 0 if more than `reset_after` has elapsed since the last press.
    pub fn advance(&mut self, app: &str, window_count: usize) -> usize {
        if window_count == 0 {
            return 0;
        }
        let now = Instant::now();
        let entry = self.entries.entry(app.to_string()).or_insert(Entry {
            cursor: 0,
            last_press: now,
        });
        let stale = self
            .reset_after
            .map(|d| now.duration_since(entry.last_press) > d)
            .unwrap_or(false);
        if stale {
            entry.cursor = 0;
        } else {
            entry.cursor = (entry.cursor + 1) % window_count;
        }
        entry.last_press = now;
        entry.cursor
    }

    /// Peek the current cursor without advancing.
    pub fn current(&self, app: &str) -> usize {
        self.entries.get(app).map(|e| e.cursor).unwrap_or(0)
    }

    /// Reset the cursor for one app (e.g. after the app quits).
    pub fn reset(&mut self, app: &str) {
        self.entries.remove(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn advances_modulo_window_count() {
        let mut s = CycleState::new(0);
        assert_eq!(s.advance("a", 3), 1);
        assert_eq!(s.advance("a", 3), 2);
        assert_eq!(s.advance("a", 3), 0);
        assert_eq!(s.advance("a", 3), 1);
    }

    #[test]
    fn separate_per_app() {
        let mut s = CycleState::new(0);
        s.advance("a", 5);
        s.advance("a", 5);
        assert_eq!(s.current("a"), 2);
        assert_eq!(s.current("b"), 0);
    }

    #[test]
    fn zero_windows_returns_zero() {
        let mut s = CycleState::new(0);
        assert_eq!(s.advance("a", 0), 0);
    }

    #[test]
    fn resets_after_threshold() {
        let mut s = CycleState::new(50);
        s.advance("a", 3);
        s.advance("a", 3);
        sleep(Duration::from_millis(80));
        assert_eq!(s.advance("a", 3), 0);
    }

    #[test]
    fn reset_clears_app() {
        let mut s = CycleState::new(0);
        s.advance("a", 5);
        s.reset("a");
        assert_eq!(s.current("a"), 0);
    }
}
