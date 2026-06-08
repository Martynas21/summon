use crate::config::{Modifiers as SpecMods, ParsedBinding};
use anyhow::{anyhow, Result};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers as GhMods},
    GlobalHotKeyManager,
};
use std::collections::HashMap;

/// Per-binding resolution data the daemon needs when a hotkey fires.
#[derive(Debug, Clone)]
pub struct BindingTarget {
    pub app: String,
    pub cmdline_contains: Option<String>,
}

/// Owns the global-hotkey manager and remembers which app each hotkey ID maps to.
pub struct HotkeyRegistry {
    manager: GlobalHotKeyManager,
    id_to_target: HashMap<u32, BindingTarget>,
    registered: Vec<HotKey>,
}

impl HotkeyRegistry {
    pub fn new() -> Result<Self> {
        let manager = GlobalHotKeyManager::new()
            .map_err(|e| anyhow!("global hotkey manager init failed: {e:?}"))?;
        Ok(Self {
            manager,
            id_to_target: HashMap::new(),
            registered: Vec::new(),
        })
    }

    pub fn register_all(&mut self, bindings: &[ParsedBinding]) -> Result<()> {
        for b in bindings {
            let mods = to_gh_mods(b.hotkey.modifiers);
            let code = to_code(&b.hotkey.key)
                .ok_or_else(|| anyhow!("unsupported key '{}' in '{}'", b.hotkey.key, b.hotkey.raw))?;
            let hk = HotKey::new(Some(mods), code);
            let id = hk.id();
            self.manager
                .register(hk)
                .map_err(|e| anyhow!("register '{}': {e:?}", b.hotkey.raw))?;
            self.id_to_target.insert(
                id,
                BindingTarget {
                    app: b.app.clone(),
                    cmdline_contains: b.cmdline_contains.clone(),
                },
            );
            self.registered.push(hk);
        }
        Ok(())
    }

    pub fn unregister_all(&mut self) {
        if !self.registered.is_empty() {
            let _ = self.manager.unregister_all(&self.registered);
        }
        self.registered.clear();
        self.id_to_target.clear();
    }

    pub fn target_for(&self, id: u32) -> Option<&BindingTarget> {
        self.id_to_target.get(&id)
    }
}

fn to_gh_mods(m: SpecMods) -> GhMods {
    let mut out = GhMods::empty();
    if m.ctrl {
        out |= GhMods::CONTROL;
    }
    if m.alt {
        out |= GhMods::ALT;
    }
    if m.shift {
        out |= GhMods::SHIFT;
    }
    if m.cmd {
        out |= GhMods::META;
    }
    out
}

fn to_code(key: &str) -> Option<Code> {
    use Code::*;
    Some(match key {
        "0" => Digit0, "1" => Digit1, "2" => Digit2, "3" => Digit3, "4" => Digit4,
        "5" => Digit5, "6" => Digit6, "7" => Digit7, "8" => Digit8, "9" => Digit9,
        "a" => KeyA, "b" => KeyB, "c" => KeyC, "d" => KeyD, "e" => KeyE,
        "f" => KeyF, "g" => KeyG, "h" => KeyH, "i" => KeyI, "j" => KeyJ,
        "k" => KeyK, "l" => KeyL, "m" => KeyM, "n" => KeyN, "o" => KeyO,
        "p" => KeyP, "q" => KeyQ, "r" => KeyR, "s" => KeyS, "t" => KeyT,
        "u" => KeyU, "v" => KeyV, "w" => KeyW, "x" => KeyX, "y" => KeyY, "z" => KeyZ,
        "f1" => F1, "f2" => F2, "f3" => F3, "f4" => F4, "f5" => F5,
        "f6" => F6, "f7" => F7, "f8" => F8, "f9" => F9, "f10" => F10,
        "f11" => F11, "f12" => F12, "f13" => F13, "f14" => F14, "f15" => F15,
        "f16" => F16, "f17" => F17, "f18" => F18, "f19" => F19, "f20" => F20,
        "space" => Space,
        "tab" => Tab,
        "enter" | "return" => Enter,
        "escape" | "esc" => Escape,
        "backspace" => Backspace,
        "delete" => Delete,
        "left" => ArrowLeft,
        "right" => ArrowRight,
        "up" => ArrowUp,
        "down" => ArrowDown,
        "home" => Home,
        "end" => End,
        "pageup" => PageUp,
        "pagedown" => PageDown,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Modifiers as SpecMods;
    use global_hotkey::hotkey::{Code, Modifiers as GhMods};

    // --- to_gh_mods ---

    #[test]
    fn no_modifiers_gives_empty_flags() {
        let mods = to_gh_mods(SpecMods::default());
        assert_eq!(mods, GhMods::empty());
    }

    #[test]
    fn ctrl_maps_to_control() {
        let mods = to_gh_mods(SpecMods { ctrl: true, ..Default::default() });
        assert!(mods.contains(GhMods::CONTROL));
        assert!(!mods.contains(GhMods::ALT));
        assert!(!mods.contains(GhMods::SHIFT));
        assert!(!mods.contains(GhMods::META));
    }

    #[test]
    fn all_four_modifiers_combine() {
        let mods = to_gh_mods(SpecMods { ctrl: true, alt: true, shift: true, cmd: true });
        assert_eq!(mods, GhMods::CONTROL | GhMods::ALT | GhMods::SHIFT | GhMods::META);
    }

    // --- to_code ---

    #[test]
    fn digit_keys_map() {
        assert_eq!(to_code("0"), Some(Code::Digit0));
        assert_eq!(to_code("9"), Some(Code::Digit9));
    }

    #[test]
    fn letter_keys_map() {
        assert_eq!(to_code("a"), Some(Code::KeyA));
        assert_eq!(to_code("z"), Some(Code::KeyZ));
    }

    #[test]
    fn function_keys_map() {
        assert_eq!(to_code("f1"), Some(Code::F1));
        assert_eq!(to_code("f20"), Some(Code::F20));
    }

    #[test]
    fn special_keys_map() {
        assert_eq!(to_code("space"), Some(Code::Space));
        assert_eq!(to_code("tab"), Some(Code::Tab));
        assert_eq!(to_code("backspace"), Some(Code::Backspace));
        assert_eq!(to_code("left"), Some(Code::ArrowLeft));
        assert_eq!(to_code("pageup"), Some(Code::PageUp));
    }

    #[test]
    fn enter_and_return_are_aliases() {
        assert_eq!(to_code("enter"), Some(Code::Enter));
        assert_eq!(to_code("return"), Some(Code::Enter));
    }

    #[test]
    fn esc_is_alias_for_escape() {
        assert_eq!(to_code("escape"), Some(Code::Escape));
        assert_eq!(to_code("esc"), Some(Code::Escape));
    }

    #[test]
    fn unknown_key_returns_none() {
        assert_eq!(to_code("nope"), None);
        assert_eq!(to_code(""), None);
        assert_eq!(to_code("F1"), None); // case-sensitive — config normalises to lowercase
    }
}
