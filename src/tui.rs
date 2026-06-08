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
    ConfirmQuit,
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

    fn navigate_up(&mut self) {
        match self.pane {
            Pane::Bindings => scroll_up(&mut self.binding_state, self.bindings.len()),
            Pane::Apps => {
                let len = filtered_apps(&self.running_apps, &self.search_buf).len();
                scroll_up(&mut self.app_state, len);
            }
        }
    }

    fn navigate_down(&mut self) {
        match self.pane {
            Pane::Bindings => scroll_down(&mut self.binding_state, self.bindings.len()),
            Pane::Apps => {
                let len = filtered_apps(&self.running_apps, &self.search_buf).len();
                scroll_down(&mut self.app_state, len);
            }
        }
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

    let config: Config = fs::read_to_string(&cfg_path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default();

    let mut bindings: Vec<(String, String)> = config
        .bindings
        .iter()
        .map(|(k, v)| (k.clone(), v.app().to_string()))
        .collect();
    bindings.sort_by(|a, b| a.0.cmp(&b.0));

    let running_apps = list_platform_apps();
    let mut app = App::new(bindings, running_apps);

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
            if app.dirty {
                app.mode = Mode::ConfirmQuit;
                continue;
            }
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
            Mode::ConfirmQuit => {
                if handle_confirm_quit(app, key.code) {
                    return Ok(());
                }
            }
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
        KeyCode::Char('s') => save_config(app)?,
        KeyCode::Char('q') | KeyCode::Esc => {
            if app.dirty {
                app.mode = Mode::ConfirmQuit;
            } else {
                return Ok(true);
            }
        }
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
        KeyCode::Char('r') => {
            app.running_apps = list_platform_apps();
            app.search_buf.clear();
            app.search_active = false;
            reset_app_selection(app);
            app.status = Some(format!("Refreshed — {} apps", app.running_apps.len()));
        }
        KeyCode::Char('s') => save_config(app)?,
        KeyCode::Char('q') => {
            if app.dirty {
                app.mode = Mode::ConfirmQuit;
            } else {
                return Ok(true);
            }
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

fn handle_confirm_quit(app: &mut App, key: KeyCode) -> bool {
    match key {
        KeyCode::Char('y') | KeyCode::Char('Y') => true,
        _ => {
            app.mode = Mode::Browse;
            false
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
    if ipc::running_pid().is_ok() {
        ipc::reload()?;
        app.status = Some("Saved and reloaded.".into());
    } else {
        app.status = Some("Saved.".into());
    }
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
            let app_str = if ident.len() > 28 {
                format!("{}…", &ident[..27])
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
                let short = if ident.len() > 32 {
                    format!("{}…", &ident[..31])
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
        Mode::ConfirmQuit => (
            " Unsaved changes — quit without saving? [y/N]".into(),
            Style::default().fg(Color::Yellow),
        ),
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
            (Mode::ConfirmDelete, _) | (Mode::ConfirmQuit, _) => {
                " y·confirm  any other key·cancel"
            }
            (Mode::Browse, Pane::Bindings) => {
                " ↑↓/jk·navigate  e/Enter·edit hotkey  n·new  d·delete  Tab·pick app  s·save  q·quit"
            }
            (Mode::Browse, Pane::Apps) => {
                " ↑↓/jk·navigate  /·search  Enter·assign  Tab/Esc·back  r·refresh  s·save  q·quit"
            }
        }
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

// ── Platform app listing ───────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn list_platform_apps() -> Vec<(String, String)> {
    crate::macos::app::list_running_apps()
}

#[cfg(target_os = "windows")]
fn list_platform_apps() -> Vec<(String, String)> {
    crate::windows::app::list_running_apps()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn list_platform_apps() -> Vec<(String, String)> {
    Vec::new()
}
