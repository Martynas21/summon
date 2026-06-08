---
name: rust-tests
description: Write or review Rust unit tests for the summon project. Use when adding tests to any module, checking whether something is testable, or deciding how to structure a new test block. Covers test placement, naming, assertion style, fixture patterns, and the project's rule about only testing pure functions (not OS-API wrappers).
---

# Writing tests in summon

## What to test

Only test **pure functions** — code that takes inputs and returns outputs with no OS calls, no I/O,
and no side effects. Mocking OS APIs tests the mock, not the code.

**Safe to test:**
- Parsing and validation (`config.rs` — hotkey strings, TOML)
- State machines (`cycle_state.rs`, hold-threshold logic in `summoner.rs`)
- Data transformations (key/modifier mapping in `hotkey.rs`)
- String derivation (`cursor_key`, `derive_cycle_window`)
- Constructor field wiring (`Summoner::new`, `Summoner::reconfigure`)

**Do NOT test** (OS-heavy, no unit tests):
- `daemon.rs` — message pump, signal handlers
- `ipc.rs` — PID file, signals, named events
- `macos/*` — Accessibility API, NSWorkspace, dispatch
- `windows/*` — Win32 EnumWindows, SetForegroundWindow, etc.

---

## Module structure

Co-locate tests with the code they test. Always at the bottom of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // Add stdlib/crate imports only if not re-exported by super::*
    // e.g. use std::time::Duration;

    #[test]
    fn descriptive_behaviour_name() {
        // body
    }
}
```

Never put tests in a separate file. Never add `pub(crate)` to a function just to test it —
`use super::*` inside `#[cfg(test)]` already reaches private items.

---

## Naming

Name the test after the **observable behaviour**, not the function under test.

```
cursor_key_prefers_bundle_id_over_name   ✓
derive_cycle_window_zero_uses_default    ✓
test_cursor_key                          ✗
cursor_key_works                         ✗
```

Pattern: `<subject>_<condition>_<outcome>` or `<subject>_<outcome>_when_<condition>`.

---

## Assertions

```rust
assert_eq!(actual, expected);           // equality — put actual first, expected second
assert!(expr);                          // boolean truth
assert!(expr, "message: {value}");      // with a diagnostic (use when failure is non-obvious)
```

Error path — use `.unwrap_err()` then check the message:
```rust
let err = parse_hotkey("bad").unwrap_err().to_string();
assert!(err.contains("expected fragment"), "got: {err}");
```

Avoid `.unwrap()` on the happy path when the return type is `Result` — use `?` or `.unwrap()`
only if the test truly cannot fail on that line.

---

## Fixtures

Prefer inline values. For TOML config, use raw string literals:

```rust
let src = r#"
    [bindings]
    "ctrl+1" = "Ghostty"
"#;
let cfg = parse_str(src).unwrap();
```

For structs with many fields, write a small helper that returns a minimal valid instance:

```rust
fn default_config() -> ParsedConfig {
    ParsedConfig { settings: Settings::default(), bindings: vec![] }
}
```

Keep helpers inside the `mod tests` block — they are test infrastructure, not production code.

---

## Bitflag assertions (hotkey modifier tests)

`global_hotkey::hotkey::Modifiers` is a bitflag type. Use `contains()` to check individual bits:

```rust
let mods = to_gh_mods(SpecMods { ctrl: true, ..Default::default() });
assert!(mods.contains(GhMods::CONTROL));
assert!(!mods.contains(GhMods::ALT));
```

Or compare the whole value when all flags are known:
```rust
assert_eq!(mods, GhMods::CONTROL | GhMods::META);
```

---

## Running tests

```bash
source "$HOME/.cargo/env"
cargo test                          # all tests
cargo test summoner                 # filter by module/test name substring
cargo test -- --nocapture           # show println! output
```

Pinned to Rust **1.95.0** (`rust-toolchain.toml`). Tests compile for the host platform (Linux/WSL)
even though the app targets macOS and Windows; `#[cfg(target_os = "...")]` gates are active.
