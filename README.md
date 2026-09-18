# summon

Bind a hotkey to an app. Press `Ctrl+1` → app pops to the foreground, whether it's running, hidden, minimized, or on another Space. Repeat-press cycles through that app's windows.

Single self-contained Rust binary for macOS. No Lua, no scripting host, no electron.

## Why

You want `Ctrl+1` to always show your terminal, `Ctrl+2` to always show Chrome, etc. Existing tools either need a scripting host or compose poorly with app launching. `summon` does one thing: hotkey → app, with semantics that match what you'd expect.

## Install

```bash
# 1. Build (requires Rust 1.95+). Use the script, not bare `cargo build`:
#    it re-signs with a stable designated requirement so the macOS
#    Accessibility grant survives future rebuilds.
cd ~/Projects/summon
./scripts/build.sh

# 2. Put binary on PATH
mkdir -p ~/.local/bin
ln -sf "$PWD/target/release/summon" ~/.local/bin/summon

# 3. Seed a config
mkdir -p ~/.config/summon
cp examples/config.toml ~/.config/summon/config.toml
$EDITOR ~/.config/summon/config.toml

# 4. First-run permission grant
summon run
#   → macOS prompts for Accessibility permission.
#   → Click "Open System Settings", grant access to `summon`.
#   → Ctrl-C once granted.

# 5. Install as a background LaunchAgent (auto-starts at login)
summon install
```

## Subcommands

| Command                  | What it does                                                          |
| ------------------------ | --------------------------------------------------------------------- |
| `summon run`             | Run the daemon in the foreground (logs to stderr). For debugging.     |
| `summon install`         | Install the LaunchAgent autostart entry.                              |
| `summon uninstall`       | Remove the autostart entry.                                           |
| `summon reload`          | Re-read the config in the running daemon.                             |
| `summon stop`            | Stop the running daemon. launchd may restart it.                      |
| `summon status`          | Report daemon pid, permission status, config validity.                |
| `summon validate [PATH]` | Parse-check a config file.                                            |
| `summon edit`            | Open config in `$EDITOR` and reload daemon on save.                   |

## Config

Config lives at `~/.config/summon/config.toml`.

See [`examples/config.toml`](examples/config.toml) for a starter.

```toml
[settings]
cycle_reset_ms = 0       # Reset cycle cursor after N ms idle. 0 = never.
hold_threshold_ms = 0    # Hold a hotkey ≥N ms to minimize the app's
                         # frontmost window (no focus change). 0 = disabled.
                         # Recommended: 200.

[bindings]
# bundle id (preferred, survives renames) or display name
"ctrl+1" = "com.mitchellh.ghostty"
"ctrl+2" = "Google Chrome"
```

**App identifiers**: bundle id (`com.foo.bar`) or display name. Find a bundle id with `mdls -name kMDItemCFBundleIdentifier /Applications/Ghostty.app`.

**Modifiers**: `ctrl`, `alt` (= `opt`, `option`), `shift`, `cmd` (= `command`, `meta`), `hyper` (= all four). At least one modifier is required.

**Keys**: `a`–`z`, `0`–`9`, `f1`–`f20`, `space`, `tab`, `enter`, `escape`, `backspace`, `delete`, `left`/`right`/`up`/`down`, `home`/`end`/`pageup`/`pagedown`.

Reload config without restarting the daemon: `summon reload`.

## Summon semantics

When you press a bound hotkey:

1. **App not running** → launches it, waits up to 5s, then continues.
2. **App hidden** → un-hides.
3. **Cycle through windows**: picks the most-recently-raised on first press, advances on repeat presses. Standard windows preferred over palettes/utilities.
4. **Minimized window** → un-minimizes.
5. **Raise + activate** the app. Space-switching follows your Mission Control settings.

No matter the prior state, pressing the hotkey shows you that app.

## Troubleshooting

- **"Accessibility permission required"** → System Settings → Privacy & Security → Accessibility → toggle on for `summon`. Run `summon run` to trigger the prompt if it doesn't appear.
- **Daemon isn't auto-starting** → check `launchctl print gui/$(id -u)/dev.summon.daemon`. Logs at `~/Library/Logs/summon/{stdout,stderr}.log`.
- **Hotkey does nothing** → confirm no other process owns it (Hammerspoon, Karabiner, Spotlight). `summon status` should show daemon running and AX granted.
- **Switching Spaces doesn't work** → System Settings → Desktop & Dock → "When switching to an application, switch to a Space with open windows for the application."

## File locations

| Path                                              | What                                    |
| ------------------------------------------------- | --------------------------------------- |
| `~/.config/summon/config.toml`                    | Config                                  |
| `~/Library/Application Support/summon/summon.pid` | Daemon PID file                         |
| `~/Library/LaunchAgents/dev.summon.daemon.plist`  | LaunchAgent (after `summon install`)    |
| `~/Library/Logs/summon/{stdout,stderr}.log`       | Daemon logs (when run under launchd)    |

## License

MIT.
