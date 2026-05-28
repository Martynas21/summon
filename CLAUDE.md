# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`summon` is a single-binary macOS daemon that binds hotkeys to apps. Press → app pops to foreground (launching if needed, un-hiding/un-minimizing/Space-switching as required). Repeat presses cycle through that app's windows. Holding the hotkey (when `hold_threshold_ms > 0`) minimizes the app's frontmost window.

macOS-only. Pinned to Rust **1.95.0** (`rust-toolchain.toml`), edition 2024.

## Build / run / test

```bash
# Plain debug build — fine for `cargo test`, NOT for `summon run` if the
# binary needs to retain TCC Accessibility trust across rebuilds.
cargo build

# Release build + ad-hoc codesign with a STABLE designated requirement
# (identifier "dev.summon.daemon"). Use this when iterating on the daemon;
# otherwise TCC re-treats every rebuild as a new untrusted binary.
./scripts/build.sh                  # equivalent: cargo build --release && codesign with stable ident
./scripts/build.sh --features foo   # extra args pass through to cargo

cargo test                          # all unit tests (mostly config/hotkey/cycle_state/launchd)
cargo test --lib hotkey_registry    # single test by substring
cargo test -- --nocapture           # show println! during tests

# Foreground daemon for debugging (RUST_LOG controls verbosity via env-filter)
RUST_LOG=debug cargo run -- run

# Production install loop after `./scripts/build.sh`:
target/release/summon run           # one-time TCC prompt (must be a TTY parent)
target/release/summon install       # writes ~/Library/LaunchAgents/dev.summon.daemon.plist + bootstraps
target/release/summon reload        # SIGHUP — re-reads ~/.config/summon/config.toml
target/release/summon status        # daemon pid + AX permission + config validity
target/release/summon uninstall     # bootout + remove plist
```

`tracing-subscriber` reads `RUST_LOG` (`EnvFilter`). Default is `info`. Daemon logs under launchd: `~/Library/Logs/summon/{stdout,stderr}.log`.

## Why `scripts/build.sh` exists (don't skip it)

macOS TCC records the binary's *designated requirement* at first grant. Default ad-hoc codesign uses the CDHash → every rebuild invalidates the saved grant. The script forces `designated => identifier "dev.summon.daemon"`, so any future build with the same identifier inherits the existing Accessibility grant. **Use the script for any release build you intend to run, not bare `cargo build --release`.**

## Architecture — request flow

The daemon is a single-process, single-main-thread app pinned to NSApp's runloop. Every hotkey/signal handler runs serialised on the main queue; no mutex contention in the hot path beyond the one `Mutex<State>` around `STATE`.

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

Key invariants:

- **`Summoner::summon` is the only path that mutates cycle state.** `is_rapid` requires the *previous* press to be the same hotkey — `Ctrl+1 → Ctrl+2 → Ctrl+1` does NOT cycle Ghostty windows; it returns to the last raised window.
- **Window identity = CGWindowID, not `AXUIElementRef`.** AX pointers are not stable across queries. `_AXUIElementGetWindow` is a private-but-stable symbol (used by yabai/Hammerspoon/Rectangle); see `src/macos/window.rs`.
- **Order of operations in `summon` matters.** Minimize the previous frontmost *before* raising the target, otherwise the transition flickers.
- **Use AX `kAXMinimizedAttribute`, not `NSRunningApplication.hide()`.** `hide()` returns NO during app activation/transition (Chrome, Edge, VSCode, Slack all rejected it in practice).
- **App-not-running path is fire-and-forget.** Blocking on `launch_and_wait` (up to 5s) freezes subsequent hotkey presses behind it. The first press launches via `open`; the second press does the cycle/focus work.

## Hold-to-minimize state machine

Active iff `settings.hold_threshold_ms > 0`. Press starts a timer; release before it fires → summon; timer fires first → minimize frontmost (no focus change, no activation, no cycle-state mutation).

- `State.pending_holds: HashMap<u32, ()>` tracks hotkey ids currently in their press→(release|timer) window. Whichever of `on_hotkey_release_main` / `on_hold_fire` runs first removes the entry; the loser no-ops.
- OS key-repeat: a second `Pressed` while the entry exists is treated as auto-repeat and ignored (no second timer scheduled).
- On reload, `pending_holds.clear()` — hotkey ids are reassigned by `global-hotkey` at register time, so stale entries would map to the wrong binding.

## Signals & control plane

`summon` (the CLI) talks to the running daemon via PID-file + POSIX signals only — no socket, no RPC.

- PID file: `~/Library/Application Support/summon/summon.pid` (`paths::pid_file()`). `process_alive` uses `kill(pid, 0)`.
- `summon reload` → validate config locally first → `SIGHUP` → daemon re-registers hotkeys.
- `summon stop` → `SIGTERM` → graceful shutdown (warns if LaunchAgent will restart it).
- All three signals (SIGHUP/SIGTERM/SIGINT) are set to `SIG_IGN` and then observed via libdispatch signal sources on the main queue (see `install_signal_sources`). Don't add a thread-based signal handler — it would race with main-queue handlers.

## Install / TCC grant flow

`summon install` is fiddly because TCC caches its "is this process trusted" answer **per-process for the lifetime of the calling process** and **records the parent's identity** when first prompted from a non-launchd parent.

`launchd.rs::prompt_for_grant_via_launchd` works around both:

1. Writes a temporary `dev.summon.grant` LaunchAgent plist pointing at `summon _grant` (hidden subcommand, see `cli::Cmd::Grant`).
2. In a loop: bootstrap helper → wait for `/tmp/summon-grant.status` ("ok" or "fail") → bootout. Each iteration is a fresh process under launchd attribution, so TCC re-evaluates trust cleanly each tick.
3. `/tmp/summon-grant.prompted` sentinel ensures the modal fires exactly once across the polling loop.
4. After "ok", writes the real `dev.summon.daemon.plist` (with `KeepAlive=true`) and bootstraps it.

**Do not call `request_trust()` from the long-running daemon under launchd KeepAlive** — without the `is_foreground()` TTY check in `daemon::run`, every respawn after a denied grant would re-fire the modal.

## Config

`~/.config/summon/config.toml`, parsed by `src/config.rs` into `ParsedConfig` (sorted, deduped, validated). Settings shape:

```toml
[settings]
cycle_reset_ms = 0       # window cycle cursor reset; 0 → built-in default (1500ms)
hide_previous = false    # minimize prev frontmost app's windows when switching apps via summon
hold_threshold_ms = 0    # 0 = disabled (fire on press). Any +ve = enable hold-to-minimize.

[bindings]
"ctrl+1" = "com.mitchellh.ghostty"          # bundle id (preferred, matched first)
"ctrl+2" = "Google Chrome"                  # display name fallback
"cmd+f19" = { app = "Finder", launch_args = ["--new"] }
```

`HotkeySpec` normalises modifier synonyms (`opt`=`option`=`alt`, `cmd`=`command`=`meta`=`super`, `hyper`=all four) and keys (`a–z`, `0–9`, `f1–f20`, named keys). Duplicate detection happens post-normalisation, so `ctrl+1` and `control+1` collide.

## Module map

| Module                      | Responsibility                                                      |
| --------------------------- | ------------------------------------------------------------------- |
| `src/cli.rs`                | clap subcommand dispatch; `_grant` is the hidden launchd helper     |
| `src/daemon.rs`             | NSApp loop, hotkey forwarder, hold timer FSM, signal handlers       |
| `src/summoner.rs`           | Press → resolve app → enumerate windows → cycle/focus/raise         |
| `src/hotkey.rs`             | Wraps `global-hotkey`; maps hotkey id (u32) → app identifier        |
| `src/cycle_state.rs`        | Pure-Rust per-app cursor (currently superseded by Summoner's inline state, kept for tests) |
| `src/config.rs`             | TOML schema + hotkey-string parser + validation                     |
| `src/ipc.rs`                | PID-file based control: `running_pid`, `reload`, `stop`             |
| `src/launchd.rs`            | plist rendering, bootstrap/bootout, TCC grant polling loop          |
| `src/paths.rs`              | All XDG/macOS-standard paths in one place                           |
| `src/macos/app.rs`          | `NSWorkspace` / `NSRunningApplication` wrappers (find/launch/activate) |
| `src/macos/window.rs`       | AX window enumeration, raise/focus/minimize, `_AXUIElementGetWindow` |
| `src/macos/dispatch.rs`     | libdispatch FFI (`dispatch_async_f`, signal sources)                |
| `src/macos/permissions.rs`  | `AXIsProcessTrustedWithOptions` wrapper                             |

`src/lib.rs` re-exports everything; `src/main.rs` is one-line `cli::run`. The `macos` module is `#[cfg(target_os = "macos")]`-gated — non-macOS builds compile (CI sanity) but `summon run` exits with an error.
