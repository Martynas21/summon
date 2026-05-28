use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Top-level config file shape.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Config {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub bindings: HashMap<String, BindingValue>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Settings {
    /// Reset cycle cursor after this many ms of inactivity. 0 = never reset.
    #[serde(default)]
    pub cycle_reset_ms: u64,
    /// Hide previously-frontmost app (Cmd+H equivalent) when switching to a
    /// different app via summon. Skipped when cycling within the same app
    /// and when the frontmost is Finder.
    #[serde(default)]
    pub hide_previous: bool,
    /// Hold any hotkey this many ms (no key release) to minimize the bound
    /// app's frontmost window. 0 disables hold detection and restores
    /// zero-latency summon on key press. Recommended value: 200.
    #[serde(default)]
    pub hold_threshold_ms: u64,
}

/// Binding value supports either a bare string (app identifier) or a table form
/// for future extensibility (launch_args, working dir, etc.).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum BindingValue {
    Short(String),
    Full(BindingFull),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BindingFull {
    pub app: String,
    #[serde(default)]
    pub launch_args: Vec<String>,
}

impl BindingValue {
    pub fn app(&self) -> &str {
        match self {
            BindingValue::Short(s) => s,
            BindingValue::Full(f) => &f.app,
        }
    }
    pub fn launch_args(&self) -> &[String] {
        match self {
            BindingValue::Short(_) => &[],
            BindingValue::Full(f) => &f.launch_args,
        }
    }
}

/// A normalised, validated binding map: hotkey-spec → app identifier.
#[derive(Debug, Clone)]
pub struct ParsedConfig {
    pub settings: Settings,
    pub bindings: Vec<ParsedBinding>,
}

#[derive(Debug, Clone)]
pub struct ParsedBinding {
    pub hotkey: HotkeySpec,
    pub app: String,
    pub launch_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HotkeySpec {
    pub modifiers: Modifiers,
    pub key: String,
    /// Original string as written in the config.
    pub raw: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub cmd: bool,
}

impl Modifiers {
    pub fn is_empty(self) -> bool {
        !(self.ctrl || self.alt || self.shift || self.cmd)
    }
}

/// Load + parse a config file. Returns `ParsedConfig`.
pub fn load(path: &Path) -> Result<ParsedConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading config {}", path.display()))?;
    parse_str(&raw)
}

/// Parse + validate from a string.
pub fn parse_str(src: &str) -> Result<ParsedConfig> {
    let cfg: Config = toml::from_str(src).context("invalid TOML")?;
    let mut parsed = Vec::with_capacity(cfg.bindings.len());
    let mut seen: std::collections::HashSet<(Modifiers, String)> =
        std::collections::HashSet::new();
    for (raw_key, value) in &cfg.bindings {
        let hotkey = parse_hotkey(raw_key)?;
        if !seen.insert((hotkey.modifiers, hotkey.key.clone())) {
            bail!("duplicate hotkey: {raw_key}");
        }
        parsed.push(ParsedBinding {
            hotkey,
            app: value.app().to_string(),
            launch_args: value.launch_args().to_vec(),
        });
    }
    parsed.sort_by(|a, b| a.hotkey.raw.cmp(&b.hotkey.raw));
    Ok(ParsedConfig {
        settings: cfg.settings,
        bindings: parsed,
    })
}

/// Parse hotkey strings like "ctrl+shift+1", "cmd+f19".
pub fn parse_hotkey(s: &str) -> Result<HotkeySpec> {
    let mut mods = Modifiers::default();
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
        bail!("invalid hotkey '{s}'");
    }
    let (key_part, mod_parts) = parts.split_last().unwrap();
    for m in mod_parts {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "alt" | "opt" | "option" => mods.alt = true,
            "shift" => mods.shift = true,
            "cmd" | "command" | "meta" | "super" => mods.cmd = true,
            "hyper" => {
                mods.ctrl = true;
                mods.alt = true;
                mods.shift = true;
                mods.cmd = true;
            }
            other => bail!("unknown modifier '{other}' in '{s}'"),
        }
    }
    if mods.is_empty() {
        bail!("hotkey '{s}' has no modifier (e.g. 'ctrl+1')");
    }
    let key = normalise_key(key_part)
        .ok_or_else(|| anyhow!("unknown key '{key_part}' in hotkey '{s}'"))?;
    Ok(HotkeySpec {
        modifiers: mods,
        key,
        raw: s.to_string(),
    })
}

/// Normalise a key string to its canonical name. Accepts a-z, 0-9, F1-F20, and
/// a handful of named keys. Returns the canonical lowercase form.
fn normalise_key(s: &str) -> Option<String> {
    let lower = s.to_ascii_lowercase();
    if lower.len() == 1 && (lower.chars().next().unwrap().is_ascii_alphanumeric()) {
        return Some(lower);
    }
    if let Some(rest) = lower.strip_prefix('f') {
        if let Ok(n) = rest.parse::<u8>() {
            if (1..=20).contains(&n) {
                return Some(format!("f{n}"));
            }
        }
    }
    match lower.as_str() {
        "space" | "tab" | "enter" | "return" | "escape" | "esc" | "backspace" | "delete"
        | "left" | "right" | "up" | "down" | "home" | "end" | "pageup" | "pagedown" => {
            Some(lower)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_binding() {
        let src = r#"
            [bindings]
            "ctrl+1" = "Ghostty"
        "#;
        let cfg = parse_str(src).unwrap();
        assert_eq!(cfg.bindings.len(), 1);
        assert_eq!(cfg.bindings[0].app, "Ghostty");
        assert!(cfg.bindings[0].hotkey.modifiers.ctrl);
        assert_eq!(cfg.bindings[0].hotkey.key, "1");
    }

    #[test]
    fn parses_full_form() {
        let src = r#"
            [bindings]
            "cmd+f19" = { app = "Finder", launch_args = ["--new"] }
        "#;
        let cfg = parse_str(src).unwrap();
        assert_eq!(cfg.bindings[0].app, "Finder");
        assert_eq!(cfg.bindings[0].launch_args, vec!["--new"]);
        assert_eq!(cfg.bindings[0].hotkey.key, "f19");
    }

    #[test]
    fn parses_settings_default() {
        let src = "";
        let cfg = parse_str(src).unwrap();
        assert_eq!(cfg.settings.cycle_reset_ms, 0);
        assert_eq!(cfg.settings.hold_threshold_ms, 0);
        assert!(cfg.bindings.is_empty());
    }

    #[test]
    fn parses_hold_threshold() {
        let src = r#"
            [settings]
            hold_threshold_ms = 250
        "#;
        let cfg = parse_str(src).unwrap();
        assert_eq!(cfg.settings.hold_threshold_ms, 250);
    }

    #[test]
    fn rejects_unknown_modifier() {
        let src = r#"
            [bindings]
            "fnord+1" = "Foo"
        "#;
        let err = parse_str(src).unwrap_err().to_string();
        assert!(err.contains("unknown modifier"), "got: {err}");
    }

    #[test]
    fn rejects_no_modifier() {
        let src = r#"
            [bindings]
            "1" = "Foo"
        "#;
        let err = parse_str(src).unwrap_err().to_string();
        assert!(err.contains("no modifier"), "got: {err}");
    }

    #[test]
    fn rejects_unknown_key() {
        let src = r#"
            [bindings]
            "ctrl+nope" = "Foo"
        "#;
        let err = parse_str(src).unwrap_err().to_string();
        assert!(err.contains("unknown key"), "got: {err}");
    }

    #[test]
    fn rejects_invalid_format() {
        let err = parse_hotkey("ctrl+").unwrap_err().to_string();
        assert!(err.contains("invalid hotkey"));
    }

    #[test]
    fn hyper_expands_to_all_mods() {
        let h = parse_hotkey("hyper+a").unwrap();
        assert!(h.modifiers.ctrl && h.modifiers.alt && h.modifiers.shift && h.modifiers.cmd);
    }

    #[test]
    fn accepts_synonyms() {
        assert!(parse_hotkey("control+1").is_ok());
        assert!(parse_hotkey("opt+1").is_ok());
        assert!(parse_hotkey("option+1").is_ok());
        assert!(parse_hotkey("command+1").is_ok());
    }

    #[test]
    fn duplicate_hotkey_after_normalisation_passes() {
        // "ctrl+1" and "control+1" are equivalent — but the HashMap keys differ,
        // so the TOML layer accepts both and our duplicate check sees them as one
        // HotkeySpec collision.
        let src = r#"
            [bindings]
            "ctrl+1" = "A"
            "control+1" = "B"
        "#;
        let err = parse_str(src).unwrap_err().to_string();
        assert!(err.contains("duplicate hotkey"), "got: {err}");
    }
}
