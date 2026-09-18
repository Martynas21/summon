# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`summon` is a single-binary daemon for macOS that binds hotkeys to apps. Press → app pops to foreground (launching if needed, un-hiding/un-minimizing/Space-switching as required). Repeat presses cycle through that app's windows. Holding the hotkey (when `hold_threshold_ms > 0`) minimizes the app's frontmost window.

macOS only. Pinned to Rust **1.95.0** (`rust-toolchain.toml`), edition 2024.

## Key documents

- [docs/deploy-macos.md](docs/deploy-macos.md) — install, TCC grant flow, `scripts/build.sh` rationale, deploying code changes

## Build / run / test

```bash
cargo build                         # debug build (fine for tests; don't use for `summon run`)
./scripts/build.sh                  # release + stable codesign — use this for any run
./scripts/build.sh --features foo   # extra cargo args pass through

cargo test                          # all unit tests
cargo test --lib hotkey_registry    # single test by substring
cargo test -- --nocapture

./scripts/coverage.sh               # line coverage via cargo-llvm-cov; --html for browsable report
                                    # one-time: rustup component add llvm-tools-preview
                                    #           cargo install cargo-llvm-cov --locked

RUST_LOG=debug cargo run -- run     # foreground daemon
```

See [docs/deploy-macos.md](docs/deploy-macos.md) for install, TCC, and code-change workflows.

## Architecture — request flow

The daemon is a single-process, single-main-thread app. Every hotkey/signal handler runs serialised on the main queue/pump; no mutex contention in the hot path beyond the one `Mutex<State>` around `STATE`.

```
global-hotkey crate (Carbon RegisterEventHotKey)
        │   crossbeam channel  ← runs on a Carbon-owned thread
        ▼
summon-hotkey-forwarder thread (src/daemon.rs spawn_hotkey_forwarder)
        │   blocking recv() → dispatch::async_to_main with hotkey id packed
        │   into the context pointer (no heap alloc per event)
        ▼
on_hotkey_press_main / on_hotkey_release_main / on_hold_fire   (main queue)
        │   take Mutex<State>, look up app ident via HotkeyRegistry.app_for(id)
        ▼
Summoner::summon(ident)   src/summoner.rs
        │   1. find_running (bundle-id first, name fallback)
        │   2. ensure_visible (un-hide)
        │   3. AX windows() filtered to standard subrole when possible
        │   4. cycle decision: is_rapid = (prev_press.same_ident && within cycle_window)
        │   5. minimize previous frontmost (if hide_previous && switching apps)
        │   6. unminimize → focus → raise → activate
```

## Key invariants

- **`Summoner::summon` is the only path that mutates cycle state.** `is_rapid` requires the *previous* press to be the same hotkey — `Ctrl+1 → Ctrl+2 → Ctrl+1` does NOT cycle windows; it returns to the last raised window.
- **Order of operations in `summon` matters.** Minimize the previous frontmost *before* raising the target, otherwise the transition flickers.
- **App-not-running path is fire-and-forget.** Blocking on `launch_and_wait` (up to 5s) freezes subsequent hotkey presses. The first press launches; the second press does the cycle/focus work.
- **Window identity = CGWindowID, not `AXUIElementRef`.** AX pointers are not stable across queries. `_AXUIElementGetWindow` is a private-but-stable symbol; see `src/window.rs`.
- **Use AX `kAXMinimizedAttribute`, not `NSRunningApplication.hide()`.** `hide()` returns NO during app activation/transition (Chrome, Edge, VSCode, Slack all rejected it in practice).
- **Never pass `NSApplicationActivateAllWindows` to `activateWithOptions`.** It raises the app's windows on *every* display, undoing the active-display scoping `pick_window_idx` just did — a multi-monitor summon would pop the app on both screens. Default activation surfaces only main/key, which `window::focus` has already set to the picked window.

## Hold-to-minimize state machine

Active iff `settings.hold_threshold_ms > 0`. Press starts a timer; release before it fires → summon; timer fires first → `Summoner::minimize_on_active_display` (no focus change, no cycle-state mutation).

- `State.pending_holds: HashMap<u32, u32>` maps hotkey id → the `hold_seq` of the press that opened the press→(release|timer) window. Whichever of `on_hotkey_release_main` / `on_hold_fire` runs first removes the entry; the loser no-ops.
- **The seq tag is load-bearing, not bookkeeping.** `dispatch_after` cannot be cancelled on macOS, so press N's timer stays in flight after release. Keyed on the id alone it would consume press N+1's entry and minimize when the user meant to summon — the "have to press it twice" bug. `on_hold_fire` therefore acts only when the stored seq equals the one packed into its context.
- The id and seq share one context pointer via `pack_hold_ctx` / `unpack_hold_ctx` (id in the low 32 bits), keeping the timer path allocation-free.
- OS key-repeat: a second `Pressed` while the entry exists is ignored (no second timer scheduled).
- On reload, `pending_holds.clear()` — hotkey ids are reassigned by `global-hotkey` at register time.
- **Minimize is scoped to the active display.** A hold never reaches a window the user can't see: `pick_minimize_idx` takes the frontmost non-minimized window on `screen::active_display()`, and no-ops when that display's window is already minimized or the app isn't there. Enumeration order spans every monitor, so the positional front window is often on another one.

## Signals & control plane

A PID file handles process discovery; `kill(pid, SIGHUP/SIGTERM)` delivers the signal. Signals are observed via libdispatch signal sources on the main queue. Don't add a thread-based signal handler; it races with main-queue handlers.

## Config

Config lives at `~/.config/summon/config.toml`.

```toml
[settings]
cycle_reset_ms = 0       # 0 → built-in default (1500ms)
hide_previous = false    # minimize prev app's window when switching
hold_threshold_ms = 0    # 0 = disabled; any +ve = hold-to-minimize

[bindings]
# bundle id (preferred) or display name
"ctrl+1" = "com.mitchellh.ghostty"
"ctrl+2" = "Google Chrome"
```

`HotkeySpec` normalises modifier synonyms (`opt`=`option`=`alt`, `cmd`=`command`=`meta`=`super`, `hyper`=all four) and keys (`a–z`, `0–9`, `f1–f20`, named keys). Duplicate detection happens post-normalisation, so `ctrl+1` and `control+1` collide.

## Module map

| Module                       | Responsibility                                                      |
| ---------------------------- | ------------------------------------------------------------------- |
| `src/cli.rs`                 | clap subcommand dispatch; `_grant` is the hidden launchd helper     |
| `src/daemon.rs`              | Main loop, hotkey forwarder, hold timer FSM, signal/event handlers  |
| `src/summoner.rs`            | Press → resolve app → enumerate windows → cycle/focus/raise         |
| `src/hotkey.rs`              | Wraps `global-hotkey`; maps hotkey id (u32) → app identifier        |
| `src/cycle_state.rs`         | Pure-Rust per-app cursor (superseded by Summoner's inline state, kept for tests) |
| `src/config.rs`              | TOML schema + hotkey-string parser + validation                     |
| `src/ipc.rs`                 | PID-file based control: `running_pid`, `reload`, `stop`             |
| `src/paths.rs`               | All platform-standard paths in one place                            |
| `src/launchd.rs`             | plist rendering, bootstrap/bootout, TCC grant polling loop          |
| `src/app.rs`                 | `NSWorkspace` / `NSRunningApplication` wrappers                     |
| `src/window.rs`              | AX window enumeration, raise/focus/minimize, `_AXUIElementGetWindow` |
| `src/screen.rs`              | `CGDirectDisplayID` active-display / window-display tracking        |
| `src/dispatch.rs`            | libdispatch FFI (`dispatch_async_f`, signal sources)                |
| `src/proc.rs`                | `sysctl KERN_PROCARGS2` cmdline read                                |
| `src/permissions.rs`         | `AXIsProcessTrustedWithOptions` wrapper                             |

`src/lib.rs` re-exports everything; `src/main.rs` is one-line `cli::run`. The crate is macOS-only — a `compile_error!` in `src/lib.rs` guards any other target, and there are no `#[cfg(target_os = ...)]` gates left.
