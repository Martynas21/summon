use anyhow::{Context, Result};
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyModifiers},
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
    SelectApp,
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
    hotkey_buf: String,
    pending_app: Option<(String, String)>, // (ident, display_name) while adding
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
            pending_app: None,
            status: None,
            dirty: false,
        }
    }

    fn navigate_up(&mut self) {
        match self.pane {
            Pane::Bindings => scroll_up(&mut self.binding_state, self.bindings.len()),
            Pane::Apps => scroll_up(&mut self.app_state, self.running_apps.len()),
        }
    }

    fn navigate_down(&mut self) {
        match self.pane {
            Pane::Bindings => scroll_down(&mut self.binding_state, self.bindings.len()),
            Pane::Apps => scroll_down(&mut self.app_state, self.running_apps.len()),
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

        if key.code == KeyCode::Char('c')
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
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
            Mode::SelectApp => handle_select_app(app, key.code),
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
    match key {
        KeyCode::Up | KeyCode::Char('k') => app.navigate_up(),
        KeyCode::Down | KeyCode::Char('j') => app.navigate_down(),
        KeyCode::Tab => {
            app.pane = match app.pane {
                Pane::Bindings => Pane::Apps,
                Pane::Apps => Pane::Bindings,
            };
        }
        KeyCode::Char('n') => {
            if app.running_apps.is_empty() {
                app.status = Some("No running apps found — press 'r' to refresh.".into());
            } else {
                app.mode = Mode::SelectApp;
                app.pane = Pane::Apps;
                app.status = None;
            }
        }
        KeyCode::Char('d') => {
            if app.pane == Pane::Bindings && !app.bindings.is_empty() {
                app.mode = Mode::ConfirmDelete;
            }
        }
        KeyCode::Char('s') => save_config(app)?,
        KeyCode::Char('r') => {
            app.running_apps = list_platform_apps();
            app.app_state = ListState::default();
            if !app.running_apps.is_empty() {
                app.app_state.select(Some(0));
            }
            app.status = Some(format!("Refreshed — {} apps", app.running_apps.len()));
        }
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

fn handle_select_app(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Up | KeyCode::Char('k') => app.navigate_up(),
        KeyCode::Down | KeyCode::Char('j') => app.navigate_down(),
        KeyCode::Enter => {
            if let Some(idx) = app.app_state.selected() {
                if let Some(entry) = app.running_apps.get(idx) {
                    app.pending_app = Some(entry.clone());
                    app.hotkey_buf.clear();
                    app.mode = Mode::InputHotkey;
                    app.status = None;
                }
            }
        }
        KeyCode::Esc => {
            app.mode = Mode::Browse;
            app.pane = Pane::Bindings;
            app.status = None;
        }
        _ => {}
    }
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
                    let duplicate = app.bindings.iter().any(|(k, _)| {
                        crate::config::parse_hotkey(k)
                            .map(|s| s.modifiers == new_spec.modifiers && s.key == new_spec.key)
                            .unwrap_or(false)
                    });
                    if duplicate {
                        app.status = Some(format!("'{buf}' is already bound"));
                        return;
                    }
                    let (ident, _) = app.pending_app.take().unwrap();
                    app.bindings.push((buf.clone(), ident.clone()));
                    app.bindings.sort_by(|a, b| a.0.cmp(&b.0));
                    if let Some(pos) = app.bindings.iter().position(|(k, _)| k == &buf) {
                        app.binding_state.select(Some(pos));
                    }
                    app.dirty = true;
                    app.mode = Mode::Browse;
                    app.pane = Pane::Bindings;
                    app.status = Some(format!("Added {buf} → {ident}"));
                    app.hotkey_buf.clear();
                }
            }
        }
        KeyCode::Esc => {
            app.pending_app = None;
            app.hotkey_buf.clear();
            app.mode = Mode::Browse;
            app.pane = Pane::Bindings;
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
        bindings.insert(hotkey.clone(), BindingValue::Short(ident.clone()));
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
            let truncated = if ident.len() > 28 {
                format!("{}…", &ident[..27])
            } else {
                ident.clone()
            };
            ListItem::new(format!("{hotkey:<14} {truncated}"))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, area, &mut app.binding_state);
}

fn render_apps(frame: &mut Frame, app: &mut App, area: Rect) {
    let in_select = app.mode == Mode::SelectApp;
    let focused = in_select || (app.pane == Pane::Apps && app.mode == Mode::Browse);

    let border_style = if in_select {
        Style::default().fg(Color::Yellow)
    } else if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title = if in_select {
        format!(" Running Apps ({}) — Enter to pick ", app.running_apps.len())
    } else {
        format!(" Running Apps ({}) ", app.running_apps.len())
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let items: Vec<ListItem> = app
        .running_apps
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
            let app_name = app
                .pending_app
                .as_ref()
                .map(|(_, n)| n.as_str())
                .unwrap_or("app");
            let valid = app.hotkey_buf.is_empty()
                || crate::config::parse_hotkey(&app.hotkey_buf).is_ok();
            let indicator = if app.hotkey_buf.is_empty() {
                ""
            } else if valid {
                " ✓"
            } else {
                " ✗"
            };
            let color = if valid { Color::Green } else { Color::Red };
            (
                format!(" Hotkey for '{app_name}': {}{indicator}", app.hotkey_buf),
                Style::default().fg(color),
            )
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
    let text = match app.mode {
        Mode::SelectApp => " ↑↓/jk·navigate  Enter·pick  Esc·cancel",
        Mode::InputHotkey => " Type hotkey string (e.g. ctrl+1, cmd+shift+f2)  Enter·confirm  Esc·cancel  Backspace·delete",
        Mode::ConfirmDelete | Mode::ConfirmQuit => " y·confirm  any other key·cancel",
        Mode::Browse => " ↑↓/jk·navigate  Tab·switch pane  n·add  d·delete  s·save  r·refresh  q·quit",
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
