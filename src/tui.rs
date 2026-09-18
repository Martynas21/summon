use anyhow::{Context, Result};
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use std::collections::HashMap;
use std::fs;
use std::time::Duration;

use crate::config::{BindingValue, Config};
use crate::ipc;
use crate::paths;

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Bindings,
    Apps,
}

#[derive(PartialEq)]
enum Mode {
    Browse,
    InputHotkey,
    ConfirmDelete,
}

struct App {
    bindings: Vec<(String, String)>,      // (hotkey_raw, app_ident) sorted by hotkey
    running_apps: Vec<(String, String)>,  // (identifier, display_name)
    pane: Pane,
    binding_state: ListState,
    app_state: ListState,
    mode: Mode,
    // InputHotkey context
    hotkey_buf: String,
    hotkey_is_new: bool,           // true = new binding, false = editing existing
    hotkey_edit_idx: Option<usize>, // index being edited (when hotkey_is_new = false)
    // New-binding flow: hotkey has been entered, waiting for app selection
    pending_hotkey: Option<String>,
    // Search
    search_buf: String,
    search_active: bool,
    status: Option<String>,
    dirty: bool,
}

impl App {
    fn new(bindings: Vec<(String, String)>, running_apps: Vec<(String, String)>) -> Self {
        let mut binding_state = ListState::default();
        if !bindings.is_empty() {
            binding_state.select(Some(0));
        }
        let mut app_state = ListState::default();
        if !running_apps.is_empty() {
            app_state.select(Some(0));
        }
        Self {
            bindings,
            running_apps,
            pane: Pane::Bindings,
            binding_state,
            app_state,
            mode: Mode::Browse,
            hotkey_buf: String::new(),
            hotkey_is_new: false,
            hotkey_edit_idx: None,
            pending_hotkey: None,
            search_buf: String::new(),
            search_active: false,
            status: None,
            dirty: false,
        }
    }

    /// The active pane's list state and item count.
    fn pane_list(&mut self) -> (&mut ListState, usize) {
        match self.pane {
            Pane::Bindings => (&mut self.binding_state, self.bindings.len()),
            Pane::Apps => {
                let len = filtered_apps(&self.running_apps, &self.search_buf).len();
                (&mut self.app_state, len)
            }
        }
    }

    fn navigate_up(&mut self) {
        let (state, len) = self.pane_list();
        scroll_up(state, len);
    }

    fn navigate_down(&mut self) {
        let (state, len) = self.pane_list();
        scroll_down(state, len);
    }
}

fn scroll_up(state: &mut ListState, len: usize) {
    if len == 0 {
        return;
    }
    let i = state.selected().unwrap_or(0);
    state.select(Some(if i == 0 { len - 1 } else { i - 1 }));
}

fn scroll_down(state: &mut ListState, len: usize) {
    if len == 0 {
        return;
    }
    let i = state.selected().unwrap_or(0);
    state.select(Some((i + 1) % len));
}

fn filtered_apps<'a>(apps: &'a [(String, String)], query: &str) -> Vec<&'a (String, String)> {
    if query.is_empty() {
        apps.iter().collect()
    } else {
        let q = query.to_lowercase();
        apps.iter()
            .filter(|(id, name)| name.to_lowercase().contains(&q) || id.to_lowercase().contains(&q))
            .collect()
    }
}

fn reset_app_selection(app: &mut App) {
    let len = filtered_apps(&app.running_apps, &app.search_buf).len();
    if len > 0 {
        app.app_state.select(Some(0));
    } else {
        app.app_state.select(None);
    }
}

pub fn run() -> Result<()> {
    let cfg_path = paths::config_file()?;
    if let Some(parent) = cfg_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    if !cfg_path.exists() {
        fs::write(&cfg_path, "[bindings]\n")?;
    }

    let (config, config_err) = match fs::read_to_string(&cfg_path) {
        Err(_) => (Config::default(), None),
        Ok(s) => match toml::from_str::<Config>(&s) {
            Ok(cfg) => (cfg, None),
            Err(e) => (Config::default(), Some(format!("config parse error: {e}"))),
        },
    };

    let mut bindings: Vec<(String, String)> = config
        .bindings
        .iter()
        .map(|(k, v)| (k.clone(), v.app().to_string()))
        .collect();
    bindings.sort_by(|a, b| a.0.cmp(&b.0));

    let running_apps = list_platform_apps();
    let mut app = App::new(bindings, running_apps);
    app.status = config_err;

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| render(f, app))?;

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(());
        }

        match app.mode {
            Mode::Browse => {
                if handle_browse(app, key.code)? {
                    return Ok(());
                }
            }
            Mode::InputHotkey => handle_input_hotkey(app, key.code),
            Mode::ConfirmDelete => handle_confirm_delete(app, key.code),
        }
    }
}

fn handle_browse(app: &mut App, key: KeyCode) -> Result<bool> {
    match app.pane {
        Pane::Bindings => handle_browse_bindings(app, key),
        Pane::Apps => handle_browse_apps(app, key),
    }
}

fn handle_browse_bindings(app: &mut App, key: KeyCode) -> Result<bool> {
    match key {
        KeyCode::Up | KeyCode::Char('k') => app.navigate_up(),
        KeyCode::Down | KeyCode::Char('j') => app.navigate_down(),
        KeyCode::Tab => {
            app.pane = Pane::Apps;
            app.status = None;
        }
        // Edit hotkey of selected binding
        KeyCode::Char('e') | KeyCode::Enter => {
            if let Some(idx) = app.binding_state.selected() {
                if let Some((hotkey, _)) = app.bindings.get(idx) {
                    app.hotkey_buf = hotkey.clone();
                    app.hotkey_is_new = false;
                    app.hotkey_edit_idx = Some(idx);
                    app.mode = Mode::InputHotkey;
                    app.status = None;
                }
            }
        }
        KeyCode::Char('n') => {
            app.hotkey_buf.clear();
            app.hotkey_is_new = true;
            app.hotkey_edit_idx = None;
            app.mode = Mode::InputHotkey;
            app.status = None;
        }
        KeyCode::Char('d') => {
            if app.binding_state.selected().is_some() && !app.bindings.is_empty() {
                app.mode = Mode::ConfirmDelete;
            }
        }
        KeyCode::Char('q') => {
            save_config(app)?;
            return Ok(true);
        }
        KeyCode::Esc => return Ok(true),
        _ => {}
    }
    Ok(false)
}

fn handle_browse_apps(app: &mut App, key: KeyCode) -> Result<bool> {
    // Search input takes priority when active.
    if app.search_active {
        match key {
            KeyCode::Esc => {
                app.search_buf.clear();
                app.search_active = false;
                reset_app_selection(app);
            }
            KeyCode::Enter => {
                app.search_active = false;
            }
            KeyCode::Backspace => {
                app.search_buf.pop();
                reset_app_selection(app);
            }
            KeyCode::Char(c) => {
                app.search_buf.push(c);
                reset_app_selection(app);
            }
            KeyCode::Up | KeyCode::Char('k') => app.navigate_up(),
            KeyCode::Down | KeyCode::Char('j') => app.navigate_down(),
            _ => {}
        }
        return Ok(false);
    }

    match key {
        KeyCode::Up | KeyCode::Char('k') => app.navigate_up(),
        KeyCode::Down | KeyCode::Char('j') => app.navigate_down(),
        KeyCode::Char('/') => app.search_active = true,
        // Assign selected app to the currently highlighted binding on the left.
        KeyCode::Enter => assign_selected_app(app),
        // Cancel: go back to bindings pane (remove placeholder if new-binding flow).
        KeyCode::Tab | KeyCode::Esc => cancel_app_selection(app),
        KeyCode::Char('r') => apply_refresh(app, list_platform_apps()),
        KeyCode::Char('q') => {
            save_config(app)?;
            return Ok(true);
        }
        _ => {}
    }
    Ok(false)
}

fn assign_selected_app(app: &mut App) {
    let filtered = filtered_apps(&app.running_apps, &app.search_buf);
    let Some(app_idx) = app.app_state.selected() else {
        return;
    };
    let Some((ident, _name)) = filtered.get(app_idx) else {
        return;
    };
    let ident = (*ident).clone();

    // Which binding to update: the one highlighted on the left pane.
    let Some(binding_idx) = app.binding_state.selected() else {
        app.status = Some("No binding selected on the left — navigate there first.".into());
        return;
    };
    if binding_idx >= app.bindings.len() {
        return;
    }

    let hotkey = app.bindings[binding_idx].0.clone();
    app.bindings[binding_idx].1 = ident.clone();
    app.dirty = true;
    app.pending_hotkey = None;
    app.pane = Pane::Bindings;
    app.status = Some(format!("Assigned {ident} → {hotkey}"));
}

fn cancel_app_selection(app: &mut App) {
    // If a new binding was being created (hotkey entered, awaiting app), remove
    // the placeholder so the user doesn't end up with a dangling "—" entry.
    if let Some(hotkey) = app.pending_hotkey.take() {
        app.bindings.retain(|(k, _)| k != &hotkey);
        if app.bindings.is_empty() {
            app.binding_state.select(None);
        } else {
            let sel = app.binding_state.selected().unwrap_or(0);
            app.binding_state.select(Some(sel.min(app.bindings.len() - 1)));
        }
        app.status = Some("Cancelled new binding.".into());
    }
    app.pane = Pane::Bindings;
}

fn handle_input_hotkey(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Enter => {
            let buf = app.hotkey_buf.trim().to_string();
            if buf.is_empty() {
                app.status = Some("Type a hotkey first (e.g. ctrl+1)".into());
                return;
            }
            match crate::config::parse_hotkey(&buf) {
                Err(e) => {
                    app.status = Some(format!("Invalid: {e}"));
                }
                Ok(new_spec) => {
                    // Duplicate check, skipping the binding being edited.
                    let edit_idx = if app.hotkey_is_new {
                        None
                    } else {
                        app.hotkey_edit_idx
                    };
                    let duplicate = app.bindings.iter().enumerate().any(|(i, (k, _))| {
                        if Some(i) == edit_idx {
                            return false;
                        }
                        crate::config::parse_hotkey(k)
                            .map(|s| s.modifiers == new_spec.modifiers && s.key == new_spec.key)
                            .unwrap_or(false)
                    });
                    if duplicate {
                        app.status = Some(format!("'{buf}' is already bound"));
                        return;
                    }

                    if app.hotkey_is_new {
                        // Add a placeholder binding and switch to the apps pane.
                        app.bindings.push((buf.clone(), String::from("—")));
                        app.bindings.sort_by(|a, b| a.0.cmp(&b.0));
                        if let Some(pos) = app.bindings.iter().position(|(k, _)| k == &buf) {
                            app.binding_state.select(Some(pos));
                        }
                        app.pending_hotkey = Some(buf);
                        app.pane = Pane::Apps;
                        app.status = None;
                    } else if let Some(idx) = app.hotkey_edit_idx {
                        // Update the existing binding's hotkey in-place.
                        if idx < app.bindings.len() {
                            let old = app.bindings[idx].0.clone();
                            app.bindings[idx].0 = buf.clone();
                            app.bindings.sort_by(|a, b| a.0.cmp(&b.0));
                            if let Some(pos) = app.bindings.iter().position(|(k, _)| k == &buf) {
                                app.binding_state.select(Some(pos));
                            }
                            app.dirty = true;
                            app.status = Some(format!("Hotkey updated: {old} → {buf}"));
                        }
                    }

                    app.mode = Mode::Browse;
                    app.hotkey_buf.clear();
                }
            }
        }
        KeyCode::Esc => {
            app.mode = Mode::Browse;
            app.hotkey_buf.clear();
            app.status = None;
        }
        KeyCode::Backspace => {
            app.hotkey_buf.pop();
        }
        KeyCode::Char(c) => {
            if c.is_ascii_alphanumeric() || c == '+' {
                app.hotkey_buf.push(c.to_ascii_lowercase());
            }
        }
        _ => {}
    }
}

fn handle_confirm_delete(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            if let Some(idx) = app.binding_state.selected() {
                if idx < app.bindings.len() {
                    let (hotkey, ident) = app.bindings.remove(idx);
                    app.dirty = true;
                    if app.bindings.is_empty() {
                        app.binding_state.select(None);
                    } else {
                        app.binding_state.select(Some(idx.min(app.bindings.len() - 1)));
                    }
                    app.status = Some(format!("Deleted {hotkey} → {ident}"));
                }
            }
            app.mode = Mode::Browse;
        }
        _ => {
            app.mode = Mode::Browse;
            app.status = None;
        }
    }
}

fn save_config(app: &mut App) -> Result<()> {
    let path = paths::config_file()?;
    let existing: Config = fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default();
    let mut bindings = HashMap::new();
    for (hotkey, ident) in &app.bindings {
        if ident != "—" {
            bindings.insert(hotkey.clone(), BindingValue::Short(ident.clone()));
        }
    }
    let cfg = Config {
        settings: existing.settings,
        bindings,
    };
    let toml_str = toml::to_string_pretty(&cfg)?;
    fs::write(&path, toml_str)?;
    app.dirty = false;
    let reloaded = ipc::reload_quiet()?;
    app.status = Some(if reloaded { "Saved and reloaded.".into() } else { "Saved.".into() });
    Ok(())
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(vert[0]);

    render_bindings(frame, app, cols[0]);
    render_apps(frame, app, cols[1]);
    render_status(frame, app, vert[1]);
    render_help(frame, app, vert[2]);
}

fn render_bindings(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.pane == Pane::Bindings && app.mode == Mode::Browse;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(format!(" Bindings ({}) ", app.bindings.len()))
        .borders(Borders::ALL)
        .border_style(border_style);

    let items: Vec<ListItem> = app
        .bindings
        .iter()
        .map(|(hotkey, ident)| {
            // chars(), not display columns; fine for the ASCII-dominant app-name corpus
            let app_str = if ident.chars().count() > 28 {
                format!("{}…", ident.chars().take(27).collect::<String>())
            } else {
                ident.clone()
            };
            // Dim placeholder entries that haven't been assigned an app yet.
            let style = if ident == "—" {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            ListItem::new(format!("{hotkey:<14} {app_str}")).style(style)
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, area, &mut app.binding_state);
}

fn render_apps(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.pane == Pane::Apps && app.mode == Mode::Browse;
    let has_pending = app.pending_hotkey.is_some();

    let border_style = if has_pending {
        Style::default().fg(Color::Yellow)
    } else if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let filtered = filtered_apps(&app.running_apps, &app.search_buf);
    let count = filtered.len();
    let total = app.running_apps.len();

    let count_str = if app.search_buf.is_empty() {
        format!("{total}")
    } else {
        format!("{count}/{total}")
    };
    let title = if app.search_active {
        format!(" Apps ({count_str}) — /{}_  ", app.search_buf)
    } else if !app.search_buf.is_empty() {
        format!(" Apps ({count_str}) — /{} ", app.search_buf)
    } else {
        format!(" Running Apps ({count_str}) ")
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let items: Vec<ListItem> = filtered
        .iter()
        .map(|(ident, name)| {
            let text = if ident == name {
                name.clone()
            } else {
                // chars(), not display columns; fine for the ASCII-dominant app-name corpus
                let short = if ident.chars().count() > 32 {
                    format!("{}…", ident.chars().take(31).collect::<String>())
                } else {
                    ident.clone()
                };
                format!("{name} ({short})")
            };
            ListItem::new(text)
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, area, &mut app.app_state);
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let (text, style) = match &app.mode {
        Mode::ConfirmDelete => {
            let msg = if let Some(idx) = app.binding_state.selected() {
                if let Some((hotkey, ident)) = app.bindings.get(idx) {
                    format!(" Delete '{hotkey} → {ident}'? [y/N]")
                } else {
                    " Delete this binding? [y/N]".into()
                }
            } else {
                String::new()
            };
            (msg, Style::default().fg(Color::Yellow))
        }
        Mode::InputHotkey => {
            let valid = app.hotkey_buf.is_empty()
                || crate::config::parse_hotkey(&app.hotkey_buf).is_ok();
            let indicator = if app.hotkey_buf.is_empty() {
                ""
            } else if valid {
                " ✓"
            } else {
                " ✗"
            };
            let label = if app.hotkey_is_new {
                "New binding hotkey"
            } else {
                "Edit hotkey"
            };
            let color = if valid { Color::Green } else { Color::Red };
            (
                format!(" {label}: {}{indicator}", app.hotkey_buf),
                Style::default().fg(color),
            )
        }
        Mode::Browse if app.pane == Pane::Apps => {
            // Show contextual guidance when in the apps pane.
            if let Some(hotkey) = &app.pending_hotkey {
                (
                    format!(" New binding '{hotkey}' — pick an app, then press Enter"),
                    Style::default().fg(Color::Yellow),
                )
            } else if let Some(idx) = app.binding_state.selected() {
                if let Some((hotkey, current)) = app.bindings.get(idx) {
                    (
                        format!(" Enter assigns to '{hotkey}' (currently: {current})"),
                        Style::default().fg(Color::Gray),
                    )
                } else {
                    (String::new(), Style::default())
                }
            } else {
                (
                    " No binding selected — Tab to go back".into(),
                    Style::default().fg(Color::DarkGray),
                )
            }
        }
        _ => (
            app.status
                .as_ref()
                .map(|s| format!(" {s}"))
                .unwrap_or_default(),
            Style::default().fg(Color::Gray),
        ),
    };

    frame.render_widget(Paragraph::new(text).style(style), area);
}

fn render_help(frame: &mut Frame, app: &App, area: Rect) {
    let text = if app.search_active {
        " Type to filter  Enter·close search  Esc·clear & close  ↑↓/jk·navigate"
    } else {
        match (&app.mode, app.pane) {
            (Mode::InputHotkey, _) => {
                " Type hotkey (e.g. ctrl+1)  Enter·confirm  Esc·cancel  Backspace·delete"
            }
            (Mode::ConfirmDelete, _) => {
                " y·confirm  any other key·cancel"
            }
            (Mode::Browse, Pane::Bindings) => {
                " ↑↓/jk·navigate  e/Enter·edit hotkey  n·new  d·delete  Tab·pick app  q·save & quit"
            }
            (Mode::Browse, Pane::Apps) => {
                " ↑↓/jk·navigate  /·search  Enter·assign  Tab/Esc·back  r·refresh  q·save & quit"
            }
        }
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

// ── Platform app listing ───────────────────────────────────────────────────────

fn list_platform_apps() -> Vec<(String, String)> {
    crate::app::list_running_apps()
}

/// Replace the app list and reset the surrounding browse state. Split out from
/// the `r` key handler so it is testable without enumerating real running apps.
fn apply_refresh(app: &mut App, apps: Vec<(String, String)>) {
    app.running_apps = apps;
    app.search_buf.clear();
    app.search_active = false;
    reset_app_selection(app);
    app.status = Some(format!("Refreshed — {} apps", app.running_apps.len()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn pairs(src: &[(&str, &str)]) -> Vec<(String, String)> {
        src.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    /// Two bindings, two running apps, bindings pane focused on index 0.
    fn sample_app() -> App {
        App::new(
            pairs(&[("ctrl+1", "Ghostty"), ("ctrl+2", "Google Chrome")]),
            pairs(&[("com.apple.Safari", "Safari"), ("org.mozilla.firefox", "Firefox")]),
        )
    }

    fn render_to_text(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for (i, cell) in buf.content.iter().enumerate() {
            out.push_str(cell.symbol());
            if (i + 1) % buf.area.width as usize == 0 {
                out.push('\n');
            }
        }
        out
    }

    // --- construction ---

    #[test]
    fn new_selects_first_item_in_both_panes() {
        let app = sample_app();
        assert_eq!(app.binding_state.selected(), Some(0));
        assert_eq!(app.app_state.selected(), Some(0));
        assert!(app.pane == Pane::Bindings);
        assert!(app.mode == Mode::Browse);
    }

    #[test]
    fn new_with_empty_lists_selects_nothing() {
        let app = App::new(vec![], vec![]);
        assert_eq!(app.binding_state.selected(), None);
        assert_eq!(app.app_state.selected(), None);
    }

    // --- navigation ---

    #[test]
    fn navigation_wraps_at_both_ends() {
        let mut app = sample_app();
        app.navigate_up(); // from 0 wraps to last
        assert_eq!(app.binding_state.selected(), Some(1));
        app.navigate_down(); // from last wraps to 0
        assert_eq!(app.binding_state.selected(), Some(0));
    }

    #[test]
    fn navigation_on_empty_list_is_a_noop() {
        let mut app = App::new(vec![], vec![]);
        app.navigate_up();
        app.navigate_down();
        assert_eq!(app.binding_state.selected(), None);
    }

    #[test]
    fn navigation_respects_search_filter_length() {
        let mut app = sample_app();
        app.pane = Pane::Apps;
        app.search_buf = "safari".into(); // filters to 1 app
        app.navigate_down(); // single item: wraps onto itself
        assert_eq!(app.app_state.selected(), Some(0));
    }

    // --- app filtering ---

    #[test]
    fn empty_query_returns_all_apps() {
        let apps = pairs(&[("a", "A"), ("b", "B")]);
        assert_eq!(filtered_apps(&apps, "").len(), 2);
    }

    #[test]
    fn filter_matches_name_and_ident_case_insensitively() {
        let apps = pairs(&[("com.apple.Safari", "Safari"), ("org.mozilla.firefox", "Firefox")]);
        assert_eq!(filtered_apps(&apps, "SAFARI").len(), 1);
        assert_eq!(filtered_apps(&apps, "mozilla").len(), 1); // matches ident, not name
        assert_eq!(filtered_apps(&apps, "zzz").len(), 0);
    }

    // --- browse mode: bindings pane ---

    #[test]
    fn tab_switches_to_apps_pane() {
        let mut app = sample_app();
        assert!(!handle_browse(&mut app, KeyCode::Tab).unwrap());
        assert!(app.pane == Pane::Apps);
    }

    #[test]
    fn n_starts_a_new_binding_input() {
        let mut app = sample_app();
        handle_browse(&mut app, KeyCode::Char('n')).unwrap();
        assert!(app.mode == Mode::InputHotkey);
        assert!(app.hotkey_is_new);
        assert_eq!(app.hotkey_buf, "");
    }

    #[test]
    fn enter_edits_selected_binding_hotkey() {
        let mut app = sample_app();
        handle_browse(&mut app, KeyCode::Enter).unwrap();
        assert!(app.mode == Mode::InputHotkey);
        assert!(!app.hotkey_is_new);
        assert_eq!(app.hotkey_edit_idx, Some(0));
        assert_eq!(app.hotkey_buf, "ctrl+1");
    }

    #[test]
    fn d_with_selection_asks_for_delete_confirmation() {
        let mut app = sample_app();
        handle_browse(&mut app, KeyCode::Char('d')).unwrap();
        assert!(app.mode == Mode::ConfirmDelete);
    }

    #[test]
    fn d_with_no_bindings_does_nothing() {
        let mut app = App::new(vec![], pairs(&[("a", "A")]));
        handle_browse(&mut app, KeyCode::Char('d')).unwrap();
        assert!(app.mode == Mode::Browse);
    }

    #[test]
    fn esc_quits_without_saving() {
        let mut app = sample_app();
        assert!(handle_browse(&mut app, KeyCode::Esc).unwrap());
    }

    // --- hotkey input mode ---

    #[test]
    fn typed_chars_are_lowercased_and_non_hotkey_chars_dropped() {
        let mut app = sample_app();
        for c in ['C', 't', 'r', 'l', '+', '!', ' ', '3'] {
            handle_input_hotkey(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.hotkey_buf, "ctrl+3");
    }

    #[test]
    fn backspace_removes_last_char() {
        let mut app = sample_app();
        app.hotkey_buf = "ctrl+1".into();
        handle_input_hotkey(&mut app, KeyCode::Backspace);
        assert_eq!(app.hotkey_buf, "ctrl+");
    }

    #[test]
    fn esc_cancels_hotkey_input() {
        let mut app = sample_app();
        app.mode = Mode::InputHotkey;
        app.hotkey_buf = "ctrl+9".into();
        handle_input_hotkey(&mut app, KeyCode::Esc);
        assert!(app.mode == Mode::Browse);
        assert_eq!(app.hotkey_buf, "");
    }

    #[test]
    fn submitting_empty_hotkey_prompts_for_input() {
        let mut app = sample_app();
        app.mode = Mode::InputHotkey;
        handle_input_hotkey(&mut app, KeyCode::Enter);
        assert!(app.status.as_deref().unwrap().contains("Type a hotkey"));
        assert!(app.mode == Mode::InputHotkey, "should stay in input mode");
    }

    #[test]
    fn submitting_invalid_hotkey_reports_error() {
        let mut app = sample_app();
        app.mode = Mode::InputHotkey;
        app.hotkey_buf = "1".into(); // no modifier
        handle_input_hotkey(&mut app, KeyCode::Enter);
        assert!(app.status.as_deref().unwrap().contains("Invalid"));
    }

    #[test]
    fn submitting_duplicate_hotkey_is_rejected_after_normalisation() {
        let mut app = sample_app();
        app.mode = Mode::InputHotkey;
        app.hotkey_is_new = true;
        app.hotkey_buf = "control+1".into(); // normalises to existing ctrl+1
        handle_input_hotkey(&mut app, KeyCode::Enter);
        assert!(app.status.as_deref().unwrap().contains("already bound"));
        assert_eq!(app.bindings.len(), 2, "no binding added");
    }

    #[test]
    fn new_valid_hotkey_adds_placeholder_and_moves_to_app_pane() {
        let mut app = sample_app();
        app.mode = Mode::InputHotkey;
        app.hotkey_is_new = true;
        app.hotkey_buf = "ctrl+3".into();
        handle_input_hotkey(&mut app, KeyCode::Enter);
        assert!(app.pane == Pane::Apps);
        assert!(app.mode == Mode::Browse);
        assert_eq!(app.pending_hotkey.as_deref(), Some("ctrl+3"));
        let entry = app.bindings.iter().find(|(k, _)| k == "ctrl+3").unwrap();
        assert_eq!(entry.1, "—");
        // selection follows the new (sorted-in) entry
        assert_eq!(app.binding_state.selected(), Some(2));
    }

    #[test]
    fn editing_hotkey_renames_binding_in_place() {
        let mut app = sample_app();
        app.mode = Mode::InputHotkey;
        app.hotkey_is_new = false;
        app.hotkey_edit_idx = Some(0);
        app.hotkey_buf = "ctrl+9".into();
        handle_input_hotkey(&mut app, KeyCode::Enter);
        assert!(app.dirty);
        let entry = app.bindings.iter().find(|(k, _)| k == "ctrl+9").unwrap();
        assert_eq!(entry.1, "Ghostty", "app keeps its binding under the new hotkey");
        assert!(!app.bindings.iter().any(|(k, _)| k == "ctrl+1"));
        assert!(app.status.as_deref().unwrap().contains("Hotkey updated"));
    }

    #[test]
    fn editing_can_resubmit_same_hotkey_without_duplicate_error() {
        let mut app = sample_app();
        app.mode = Mode::InputHotkey;
        app.hotkey_is_new = false;
        app.hotkey_edit_idx = Some(0);
        app.hotkey_buf = "ctrl+1".into(); // unchanged
        handle_input_hotkey(&mut app, KeyCode::Enter);
        assert!(app.status.as_deref().unwrap().contains("Hotkey updated"));
    }

    // --- delete confirmation ---

    #[test]
    fn confirming_delete_removes_binding_and_clamps_selection() {
        let mut app = sample_app();
        app.binding_state.select(Some(1)); // last entry
        app.mode = Mode::ConfirmDelete;
        handle_confirm_delete(&mut app, KeyCode::Char('y'));
        assert_eq!(app.bindings.len(), 1);
        assert_eq!(app.binding_state.selected(), Some(0), "selection clamped");
        assert!(app.dirty);
        assert!(app.status.as_deref().unwrap().contains("Deleted ctrl+2"));
        assert!(app.mode == Mode::Browse);
    }

    #[test]
    fn deleting_last_binding_clears_selection() {
        let mut app = App::new(pairs(&[("ctrl+1", "Ghostty")]), vec![]);
        app.mode = Mode::ConfirmDelete;
        handle_confirm_delete(&mut app, KeyCode::Char('y'));
        assert!(app.bindings.is_empty());
        assert_eq!(app.binding_state.selected(), None);
    }

    #[test]
    fn any_other_key_cancels_delete() {
        let mut app = sample_app();
        app.mode = Mode::ConfirmDelete;
        handle_confirm_delete(&mut app, KeyCode::Char('x'));
        assert_eq!(app.bindings.len(), 2);
        assert!(app.mode == Mode::Browse);
    }

    // --- apps pane: search ---

    #[test]
    fn slash_opens_search_and_typing_filters() {
        let mut app = sample_app();
        app.pane = Pane::Apps;
        handle_browse(&mut app, KeyCode::Char('/')).unwrap();
        assert!(app.search_active);
        for c in "fire".chars() {
            handle_browse(&mut app, KeyCode::Char(c)).unwrap();
        }
        assert_eq!(app.search_buf, "fire");
        assert_eq!(filtered_apps(&app.running_apps, &app.search_buf).len(), 1);
        assert_eq!(app.app_state.selected(), Some(0), "selection reset to filtered top");
    }

    #[test]
    fn search_backspace_pops_and_esc_clears() {
        let mut app = sample_app();
        app.pane = Pane::Apps;
        app.search_active = true;
        app.search_buf = "fire".into();
        handle_browse(&mut app, KeyCode::Backspace).unwrap();
        assert_eq!(app.search_buf, "fir");
        handle_browse(&mut app, KeyCode::Esc).unwrap();
        assert_eq!(app.search_buf, "");
        assert!(!app.search_active);
    }

    #[test]
    fn search_enter_keeps_filter_but_closes_input() {
        let mut app = sample_app();
        app.pane = Pane::Apps;
        app.search_active = true;
        app.search_buf = "safari".into();
        handle_browse(&mut app, KeyCode::Enter).unwrap();
        assert!(!app.search_active);
        assert_eq!(app.search_buf, "safari");
    }

    // --- assigning apps to bindings ---

    #[test]
    fn enter_assigns_selected_app_to_selected_binding() {
        let mut app = sample_app();
        app.pane = Pane::Apps;
        handle_browse(&mut app, KeyCode::Enter).unwrap();
        assert_eq!(app.bindings[0].1, "com.apple.Safari");
        assert!(app.dirty);
        assert!(app.pane == Pane::Bindings);
        assert!(app.status.as_deref().unwrap().contains("Assigned"));
    }

    #[test]
    fn assign_respects_search_filter() {
        let mut app = sample_app();
        app.pane = Pane::Apps;
        app.search_buf = "firefox".into();
        reset_app_selection(&mut app);
        assign_selected_app(&mut app);
        assert_eq!(app.bindings[0].1, "org.mozilla.firefox");
    }

    #[test]
    fn assign_without_binding_selected_warns() {
        let mut app = sample_app();
        app.pane = Pane::Apps;
        app.binding_state.select(None);
        assign_selected_app(&mut app);
        assert!(app.status.as_deref().unwrap().contains("No binding selected"));
        assert!(!app.dirty);
    }

    #[test]
    fn cancelling_new_binding_removes_placeholder() {
        let mut app = sample_app();
        // Full new-binding flow: n → type ctrl+3 → Enter → Esc in apps pane.
        handle_browse(&mut app, KeyCode::Char('n')).unwrap();
        for c in "ctrl+3".chars() {
            handle_input_hotkey(&mut app, KeyCode::Char(c));
        }
        handle_input_hotkey(&mut app, KeyCode::Enter);
        assert_eq!(app.bindings.len(), 3);
        handle_browse(&mut app, KeyCode::Esc).unwrap();
        assert_eq!(app.bindings.len(), 2, "placeholder removed");
        assert!(app.pending_hotkey.is_none());
        assert!(app.pane == Pane::Bindings);
        assert!(app.status.as_deref().unwrap().contains("Cancelled"));
    }

    #[test]
    fn tab_from_apps_returns_to_bindings_without_pending() {
        let mut app = sample_app();
        app.pane = Pane::Apps;
        handle_browse(&mut app, KeyCode::Tab).unwrap();
        assert!(app.pane == Pane::Bindings);
        assert_eq!(app.bindings.len(), 2);
    }

    // --- edge cases: stale selections, no-op keys ---

    #[test]
    fn arrow_keys_navigate_bindings_pane() {
        let mut app = sample_app();
        handle_browse(&mut app, KeyCode::Down).unwrap();
        assert_eq!(app.binding_state.selected(), Some(1));
        handle_browse(&mut app, KeyCode::Up).unwrap();
        assert_eq!(app.binding_state.selected(), Some(0));
    }

    #[test]
    fn arrow_keys_navigate_apps_pane_even_during_search() {
        let mut app = sample_app();
        app.pane = Pane::Apps;
        handle_browse(&mut app, KeyCode::Down).unwrap();
        assert_eq!(app.app_state.selected(), Some(1));
        handle_browse(&mut app, KeyCode::Up).unwrap();
        assert_eq!(app.app_state.selected(), Some(0));
        app.search_active = true;
        handle_browse(&mut app, KeyCode::Down).unwrap();
        assert_eq!(app.app_state.selected(), Some(1));
        handle_browse(&mut app, KeyCode::Up).unwrap();
        assert_eq!(app.app_state.selected(), Some(0));
    }

    #[test]
    fn unhandled_keys_are_ignored_in_every_browse_state() {
        let mut app = sample_app();
        assert!(!handle_browse(&mut app, KeyCode::F(1)).unwrap());
        app.pane = Pane::Apps;
        assert!(!handle_browse(&mut app, KeyCode::F(1)).unwrap());
        app.search_active = true;
        assert!(!handle_browse(&mut app, KeyCode::F(1)).unwrap());
        assert!(app.mode == Mode::Browse);
    }

    #[test]
    fn edit_with_no_selection_is_a_noop() {
        let mut app = App::new(vec![], vec![]);
        handle_browse(&mut app, KeyCode::Enter).unwrap();
        assert!(app.mode == Mode::Browse);
    }

    #[test]
    fn non_character_keys_are_ignored_during_hotkey_input() {
        let mut app = sample_app();
        handle_input_hotkey(&mut app, KeyCode::Tab);
        assert_eq!(app.hotkey_buf, "");
    }

    #[test]
    fn editing_with_stale_index_changes_nothing() {
        let mut app = sample_app();
        app.mode = Mode::InputHotkey;
        app.hotkey_is_new = false;
        app.hotkey_edit_idx = Some(9);
        app.hotkey_buf = "ctrl+9".into();
        handle_input_hotkey(&mut app, KeyCode::Enter);
        assert!(!app.dirty);
        assert!(app.mode == Mode::Browse, "still exits input mode");
    }

    #[test]
    fn editing_with_no_index_changes_nothing() {
        let mut app = sample_app();
        app.mode = Mode::InputHotkey;
        app.hotkey_is_new = false;
        app.hotkey_edit_idx = None;
        app.hotkey_buf = "ctrl+9".into();
        handle_input_hotkey(&mut app, KeyCode::Enter);
        assert!(!app.dirty);
        assert_eq!(app.bindings.len(), 2);
        assert!(app.mode == Mode::Browse);
    }

    #[test]
    fn confirming_delete_with_stale_selection_only_exits_mode() {
        let mut app = sample_app();
        app.binding_state.select(Some(9));
        app.mode = Mode::ConfirmDelete;
        handle_confirm_delete(&mut app, KeyCode::Char('y'));
        assert_eq!(app.bindings.len(), 2);
        assert!(app.mode == Mode::Browse);
    }

    #[test]
    fn assign_with_no_app_selected_is_a_noop() {
        let mut app = sample_app();
        app.app_state.select(None);
        assign_selected_app(&mut app);
        assert_eq!(app.bindings[0].1, "Ghostty");
    }

    #[test]
    fn assign_with_stale_app_selection_is_a_noop() {
        let mut app = sample_app();
        app.app_state.select(Some(9));
        assign_selected_app(&mut app);
        assert_eq!(app.bindings[0].1, "Ghostty");
    }

    #[test]
    fn assign_with_stale_binding_selection_is_a_noop() {
        let mut app = sample_app();
        app.binding_state.select(Some(9));
        assign_selected_app(&mut app);
        assert!(!app.dirty);
    }

    #[test]
    fn cancelling_the_only_new_binding_clears_selection() {
        let mut app = App::new(vec![], vec![]);
        handle_browse(&mut app, KeyCode::Char('n')).unwrap();
        for c in "ctrl+1".chars() {
            handle_input_hotkey(&mut app, KeyCode::Char(c));
        }
        handle_input_hotkey(&mut app, KeyCode::Enter);
        assert_eq!(app.bindings.len(), 1);
        cancel_app_selection(&mut app);
        assert!(app.bindings.is_empty());
        assert_eq!(app.binding_state.selected(), None);
    }

    #[test]
    fn refresh_replaces_app_list_and_clears_search() {
        let mut app = sample_app();
        app.pane = Pane::Apps;
        app.search_buf = "saf".into();
        app.search_active = true;
        apply_refresh(&mut app, pairs(&[("com.apple.Safari", "Safari")]));
        assert_eq!(app.running_apps, pairs(&[("com.apple.Safari", "Safari")]));
        assert_eq!(app.search_buf, "");
        assert!(!app.search_active);
        assert!(app.status.as_deref().unwrap().contains("Refreshed — 1 apps"));
    }

    // --- rendering (in-memory TestBackend, no terminal) ---

    #[test]
    fn render_shows_bindings_and_apps() {
        let mut app = sample_app();
        let text = render_to_text(&mut app);
        assert!(text.contains("Bindings (2)"), "got:\n{text}");
        assert!(text.contains("ctrl+1"));
        assert!(text.contains("Ghostty"));
        assert!(text.contains("Running Apps (2)"));
        assert!(text.contains("Safari (com.apple.Safari)"));
    }

    #[test]
    fn render_truncates_long_idents() {
        let mut app = App::new(
            pairs(&[("ctrl+1", "an.extremely.long.bundle.identifier.that.never.ends")]),
            vec![],
        );
        let text = render_to_text(&mut app);
        assert!(text.contains("…"), "got:\n{text}");
    }

    #[test]
    fn render_hotkey_input_shows_validity_indicator() {
        let mut app = sample_app();
        app.mode = Mode::InputHotkey;
        app.hotkey_is_new = true;
        app.hotkey_buf = "ctrl+3".into();
        let text = render_to_text(&mut app);
        assert!(text.contains("New binding hotkey: ctrl+3 ✓"), "got:\n{text}");
        app.hotkey_buf = "bogus".into();
        let text = render_to_text(&mut app);
        assert!(text.contains("✗"), "got:\n{text}");
    }

    #[test]
    fn render_confirm_delete_names_the_binding() {
        let mut app = sample_app();
        app.mode = Mode::ConfirmDelete;
        let text = render_to_text(&mut app);
        assert!(text.contains("Delete 'ctrl+1 → Ghostty'? [y/N]"), "got:\n{text}");
    }

    #[test]
    fn render_apps_pane_shows_pending_hotkey_guidance() {
        let mut app = sample_app();
        app.pane = Pane::Apps;
        app.pending_hotkey = Some("ctrl+3".into());
        let text = render_to_text(&mut app);
        assert!(text.contains("New binding 'ctrl+3'"), "got:\n{text}");
    }

    #[test]
    fn render_search_title_shows_query_and_counts() {
        let mut app = sample_app();
        app.pane = Pane::Apps;
        app.search_active = true;
        app.search_buf = "saf".into();
        let text = render_to_text(&mut app);
        assert!(text.contains("Apps (1/2)"), "got:\n{text}");
        assert!(text.contains("/saf"), "got:\n{text}");
    }

    #[test]
    fn render_apps_title_keeps_filter_after_search_closes() {
        let mut app = sample_app();
        app.pane = Pane::Apps;
        app.search_active = false;
        app.search_buf = "saf".into();
        let text = render_to_text(&mut app);
        assert!(text.contains("Apps (1/2) — /saf"), "got:\n{text}");
    }

    #[test]
    fn render_shows_placeholder_binding() {
        let mut app = App::new(pairs(&[("ctrl+3", "—")]), vec![]);
        let text = render_to_text(&mut app);
        assert!(text.contains("ctrl+3"), "got:\n{text}");
        assert!(text.contains("—"), "got:\n{text}");
    }

    #[test]
    fn render_apps_shows_plain_name_when_ident_equals_name() {
        let mut app = App::new(vec![], pairs(&[("firefox", "firefox")]));
        let text = render_to_text(&mut app);
        assert!(text.contains("firefox"), "got:\n{text}");
        assert!(!text.contains("firefox (firefox)"), "got:\n{text}");
    }

    #[test]
    fn render_truncates_long_app_idents_in_apps_pane() {
        let mut app = App::new(
            vec![],
            pairs(&[("an.extremely.long.bundle.identifier.example", "Long App")]),
        );
        let text = render_to_text(&mut app);
        assert!(text.contains("Long App ("), "got:\n{text}");
        assert!(text.contains("…"), "got:\n{text}");
    }

    #[test]
    fn render_clamps_stale_selection_to_last_binding() {
        // The stateful List widget clamps an out-of-range selection during
        // render, so the status line sees the clamped index, not the stale one.
        let mut app = sample_app();
        app.mode = Mode::ConfirmDelete;
        app.binding_state.select(Some(9));
        let text = render_to_text(&mut app);
        assert!(text.contains("Delete 'ctrl+2 → Google Chrome'? [y/N]"), "got:\n{text}");
        app.binding_state.select(None);
        let text = render_to_text(&mut app);
        assert!(!text.contains("Delete '"), "got:\n{text}");
    }

    #[test]
    fn render_empty_hotkey_input_shows_no_validity_indicator() {
        let mut app = sample_app();
        app.mode = Mode::InputHotkey;
        app.hotkey_is_new = false;
        let text = render_to_text(&mut app);
        assert!(text.contains("Edit hotkey:"), "got:\n{text}");
        assert!(!text.contains('✓') && !text.contains('✗'), "got:\n{text}");
    }

    #[test]
    fn render_apps_pane_status_reflects_binding_selection() {
        let mut app = sample_app();
        app.pane = Pane::Apps;
        let text = render_to_text(&mut app);
        assert!(text.contains("Enter assigns to 'ctrl+1'"), "got:\n{text}");
        app.binding_state.select(Some(9)); // clamped to last entry by the List render
        let text = render_to_text(&mut app);
        assert!(text.contains("Enter assigns to 'ctrl+2'"), "got:\n{text}");
        app.binding_state.select(None);
        let text = render_to_text(&mut app);
        assert!(text.contains("No binding selected"), "got:\n{text}");
    }
}
