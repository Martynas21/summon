# summon

A tiny macOS daemon that binds a hotkey to an app. Press `Ctrl+1`, Ghostty pops to the foreground — whether it's already open, hidden, minimized, or on another Space. Repeat-press cycles through that app's windows.

Single self-contained Rust binary. No Lua, no Homebrew runtime, no electron.

## Why

You want `Ctrl+1` to always show Ghostty, `Ctrl+2` to always show Chrome, etc. Existing tools (Hammerspoon, skhd, Karabiner) either need a scripting host or compose poorly with app launching. `summon` does one thing: hotkey → app, with "summon" semantics that match what you'd expect.

## Install

```bash
# 1. Build (requires Rust 1.95+)
cd ~/Projects/summon
cargo build --release

# 2. Put binary on PATH (or use full path)
ln -sf "$PWD/target/release/summon" /usr/local/bin/summon

# 3. Seed a config
mkdir -p ~/.config/summon
cp examples/config.toml ~/.config/summon/config.toml
$EDITOR ~/.config/summon/config.toml

# 4. First-run permission grant (must be done before `summon install`)
summon run
#   → macOS prompts for Accessibility permission.
#   → Click "Open System Settings", grant access to `summon`.
#   → Ctrl-C the foreground daemon once granted.

# 5. Install as a background LaunchAgent (auto-starts at login)
summon install
```

## Subcommands

| Command                | What it does                                                       |
| ---------------------- | ------------------------------------------------------------------ |
| `summon run`           | Run the daemon in the foreground (logs to stderr). For debugging.  |
| `summon install`       | Write a LaunchAgent plist + bootstrap it. Auto-start on login.     |
| `summon uninstall`     | Stop and remove the LaunchAgent.                                   |
| `summon reload`        | Re-read the config in the running daemon (sends `SIGHUP`).         |
| `summon status`        | Report daemon pid, AX permission, config validity.                 |
| `summon validate [PATH]` | Parse-check a config file. Defaults to `~/.config/summon/config.toml`. |

## Config

Lives at `~/.config/summon/config.toml`. See [`examples/config.toml`](examples/config.toml) for a starter.

```toml
[settings]
cycle_reset_ms = 0       # Reset cycle cursor after N ms idle. 0 = never.

[bindings]
"ctrl+1" = "com.mitchellh.ghostty"   # by bundle id (preferred)
"ctrl+2" = "Google Chrome"           # by display name
"ctrl+3" = "com.apple.finder"
```

**App identifiers** can be either a bundle id (`com.foo.bar` — never collides, survives renames) or the display name (`Ghostty`). Bundle id matched first when ambiguous.

Find a bundle id: `mdls -name kMDItemCFBundleIdentifier /Applications/Ghostty.app`.

**Modifiers**: `ctrl`, `alt` (= `opt`, `option`), `shift`, `cmd` (= `command`, `meta`), `hyper` (= all four). At least one modifier is required.

**Keys**: `a`–`z`, `0`–`9`, `f1`–`f20`, `space`, `tab`, `enter`, `escape`, `backspace`, `delete`, `left`/`right`/`up`/`down`, `home`/`end`/`pageup`/`pagedown`.

Reload config without restarting the daemon: `summon reload`.

## Summon semantics

When you press a bound hotkey:

1. **App not running** → launches it (`open -b <bundle>` or `open -a <name>`), waits up to 5s for it to appear, then proceeds to step 2.
2. **App hidden (Cmd+H)** → un-hides.
3. **Cycle through windows**: picks one (most recent the first press, next-in-order on repeat presses). Standard windows preferred over palettes/utilities.
4. **Minimized window** → un-minimizes.
5. **Raise window** (AX `AXRaise`) + **activate app** (`NSRunningApplication.activate(.allWindows)`). macOS handles Space-switching per your Mission Control settings.

No matter the prior state, pressing the hotkey shows you that app.

## Troubleshooting

- **"Accessibility permission required"** → System Settings → Privacy & Security → Accessibility → toggle on for `summon`. If `summon` doesn't appear in the list, run `summon run` once to trigger the prompt.
- **Daemon isn't auto-starting after `summon install`** → check `launchctl print gui/$(id -u)/dev.summon.daemon`. Logs at `~/Library/Logs/summon/{stdout,stderr}.log`.
- **Hotkey does nothing** → confirm no other process owns it (Hammerspoon, Karabiner, Spotlight). `summon status` should show the daemon running and AX granted.
- **Switching Spaces doesn't work** → check System Settings → Desktop & Dock → "When switching to an application, switch to a Space with open windows for the application." Without this, macOS won't follow the window.

## File locations

| Path                                                  | What                                          |
| ----------------------------------------------------- | --------------------------------------------- |
| `~/.config/summon/config.toml`                        | Your config                                   |
| `~/Library/Application Support/summon/summon.pid`     | Daemon PID file (written while running)       |
| `~/Library/LaunchAgents/dev.summon.daemon.plist`      | LaunchAgent (after `summon install`)          |
| `~/Library/Logs/summon/{stdout,stderr}.log`           | Daemon logs (when run under launchd)          |

## License

MIT.
