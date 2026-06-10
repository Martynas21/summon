# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`summon` is a single-binary daemon for macOS and Windows that binds hotkeys to apps. Press → app pops to foreground (launching if needed, un-hiding/un-minimizing/Space-switching as required). Repeat presses cycle through that app's windows. Holding the hotkey (when `hold_threshold_ms > 0`) minimizes the app's frontmost window.

Supports macOS and Windows. Pinned to Rust **1.95.0** (`rust-toolchain.toml`), edition 2024.

## Key documents

- [docs/deploy-macos.md](docs/deploy-macos.md) — macOS install, TCC grant flow, `scripts/build.sh` rationale, deploying code changes
- [docs/deploy-windows.md](docs/deploy-windows.md) — WSL cross-compile setup, `cargo-xwin`, copy workflow, Task Scheduler, editor config

## Build / run / test

### macOS

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

### Windows (cross-compile from WSL)

```bash
source "$HOME/.cargo/env"
cargo xwin build --release --target x86_64-pc-windows-msvc
# → target/x86_64-pc-windows-msvc/release/summon.exe
cp target/x86_64-pc-windows-msvc/release/summon.exe /mnt/c/Users/<username>/
```

See [docs/deploy-windows.md](docs/deploy-windows.md) for one-time setup, install, and code-change workflows.

## Architecture — request flow

The daemon is a single-process, single-main-thread app. Every hotkey/signal handler runs serialised on the main queue/pump; no mutex contention in the hot path beyond the one `Mutex<State>` around `STATE`.

### macOS

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

### Windows

```
global-hotkey crate (Win32 RegisterHotKey on message pump thread)
        │   crossbeam channel
        ▼
summon-hotkey-forwarder thread (src/daemon.rs spawn_hotkey_forwarder)
        │   blocking recv() → PostMessageW(WM_SUMMON_DISPATCH, fn_ptr, ctx)
        ▼
msg_wnd_proc  (src/windows/dispatch.rs — Win32 HWND_MESSAGE window)
        │   WM_SUMMON_DISPATCH → on_hotkey_press_main / on_hotkey_release_main
        │   WM_TIMER           → on_hold_fire
        │   WM_SUMMON_RELOAD   → on_reload_main
        │   WM_SUMMON_STOP     → PostQuitMessage
        ▼
Summoner::summon(ident)   src/summoner.rs
        │   1. find_running by exe stem (case-insensitive)
        │   2. EnumWindows → visible, unowned, titled windows for pid
        │   3. cycle / raise via AttachThreadInput + SetForegroundWindow
```

IPC watcher thread blocks on `WaitForMultipleObjects([reload_event, stop_event])` and posts the appropriate message to the pump window.

## Key invariants

Shared:

- **`Summoner::summon` is the only path that mutates cycle state.** `is_rapid` requires the *previous* press to be the same hotkey — `Ctrl+1 → Ctrl+2 → Ctrl+1` does NOT cycle windows; it returns to the last raised window.
- **Order of operations in `summon` matters.** Minimize the previous frontmost *before* raising the target, otherwise the transition flickers.
- **App-not-running path is fire-and-forget.** Blocking on `launch_and_wait` (up to 5s) freezes subsequent hotkey presses. The first press launches; the second press does the cycle/focus work.

macOS only:

- **Window identity = CGWindowID, not `AXUIElementRef`.** AX pointers are not stable across queries. `_AXUIElementGetWindow` is a private-but-stable symbol; see `src/macos/window.rs`.
- **Use AX `kAXMinimizedAttribute`, not `NSRunningApplication.hide()`.** `hide()` returns NO during app activation/transition (Chrome, Edge, VSCode, Slack all rejected it in practice).

Windows only:

- **App identity = exe stem, not bundle ID.** Config bindings must use the lowercase exe stem, e.g. `"firefox"`. `looks_like_bundle_id()` always returns false on Windows.
- **`SetForegroundWindow` requires foreground eligibility.** The call comes from the message-pump thread that received `WM_SUMMON_DISPATCH`, so Windows grants eligibility. `AttachThreadInput` is used as a fallback when foreground ownership differs.

## Hold-to-minimize state machine

Active iff `settings.hold_threshold_ms > 0`. Press starts a timer; release before it fires → summon; timer fires first → minimize frontmost (no focus change, no cycle-state mutation).

- `State.pending_holds: HashMap<u32, ()>` tracks hotkey ids in their press→(release|timer) window. Whichever of `on_hotkey_release_main` / `on_hold_fire` runs first removes the entry; the loser no-ops.
- OS key-repeat: a second `Pressed` while the entry exists is ignored (no second timer scheduled).
- On reload, `pending_holds.clear()` — hotkey ids are reassigned by `global-hotkey` at register time.

## Signals & control plane

Both platforms use a PID file for process discovery. Signal/event delivery differs.

**macOS:** `kill(pid, SIGHUP/SIGTERM)` — signals observed via libdispatch signal sources on the main queue. Don't add a thread-based signal handler; it races with main-queue handlers.

**Windows:** Named Win32 events (`Local\summon-reload`, `Local\summon-stop`) — daemon creates them with `CreateEventW`; CLI opens and sets them; IPC watcher thread posts to the message pump.

## Config

| Platform | Path |
|----------|------|
| macOS    | `~/.config/summon/config.toml` |
| Windows  | `%APPDATA%\summon\config.toml` |

```toml
[settings]
cycle_reset_ms = 0       # 0 → built-in default (1500ms)
hide_previous = false    # minimize prev app's window when switching
hold_threshold_ms = 0    # 0 = disabled; any +ve = hold-to-minimize

[bindings]
# macOS: bundle id (preferred) or display name
"ctrl+1" = "com.mitchellh.ghostty"
"ctrl+2" = "Google Chrome"

# Windows: exe stem (case-insensitive, no .exe)
"ctrl+1" = "firefox"
"ctrl+2" = "Code"
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
| `src/launchd.rs` *(macOS)*   | plist rendering, bootstrap/bootout, TCC grant polling loop          |
| `src/macos/app.rs`           | `NSWorkspace` / `NSRunningApplication` wrappers                     |
| `src/macos/window.rs`        | AX window enumeration, raise/focus/minimize, `_AXUIElementGetWindow` |
| `src/macos/dispatch.rs`      | libdispatch FFI (`dispatch_async_f`, signal sources)                |
| `src/macos/permissions.rs`   | `AXIsProcessTrustedWithOptions` wrapper                             |
| `src/windows/app.rs`         | `EnumWindows` + `QueryFullProcessImageNameW` to find/launch apps    |
| `src/windows/window.rs`      | `EnumWindows` window list, raise via `AttachThreadInput`            |
| `src/windows/dispatch.rs`    | Hidden `HWND_MESSAGE` window; `PostMessageW`-based async_to_main    |
| `src/windows/screen.rs`      | `MonitorFromPoint` / `MonitorFromRect` display tracking             |
| `src/windows/proc.rs`        | `NtQueryInformationProcess` + `ReadProcessMemory` cmdline read      |
| `src/windows/service.rs`     | Task Scheduler XML install/uninstall via `schtasks.exe`             |
| `src/windows/permissions.rs` | Stub — no AX equivalent on Windows; always returns `true`          |

`src/lib.rs` re-exports everything; `src/main.rs` is one-line `cli::run`. Platform modules are `#[cfg(target_os = "...")]`-gated.

## Win32 FFI gotchas

- **`HWND` and `HANDLE` are `*mut c_void` in windows-sys 0.59**, not `isize`. Null checks use `.is_null()`. Store handles in statics as `usize` (cast at use) to satisfy `Send`/`Sync`.
- **Many Win32 APIs are missing or misrouted in windows-sys 0.59 feature paths.** Declare them manually when the import fails:
  ```rust
  #[link(name = "kernel32")]
  unsafe extern "system" {
      fn OpenEventW(dw_desired_access: u32, b_inherit_handle: i32, lp_name: *const u16) -> *mut std::ffi::c_void;
  }
  ```
  Affected functions: `CreateEventW`, `OpenEventW`, `SetEvent`, `WaitForMultipleObjects`, `QueryFullProcessImageNameW`, `AttachThreadInput`.
- **`INFINITE` and `WAIT_OBJECT_0`** may not resolve from `Win32::Foundation`. Use literals `0xFFFF_FFFFu32` and `0u32`.
- **`STILL_ACTIVE`** is `259u32` — declare as a constant.
- **`DPI_AWARENESS_CONTEXT`** is `*mut c_void`. Pass `(-4isize) as *mut c_void` for `DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2`.
- **Rust 2024 requires explicit `unsafe {}` inside `unsafe fn`** for calls to unsafe functions. Wrap them to silence the lint.
- **`GlobalHotKeyManager` (Windows) does not implement `Send`** — add `unsafe impl Send for State {}` on the Windows `State` struct. It is only mutated on the message-pump thread.
