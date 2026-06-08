# Windows Deploy Guide

The Windows `.exe` is cross-compiled from WSL using `cargo-xwin`, which downloads the MSVC toolchain automatically — no Windows SDK or Visual Studio install required.

## One-time WSL setup

```bash
# Install Rust if not present
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Add the Windows MSVC target and cargo-xwin
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin
```

## Build and copy

```bash
source "$HOME/.cargo/env"   # if cargo isn't in PATH yet
cargo xwin build --release --target x86_64-pc-windows-msvc
# output: target/x86_64-pc-windows-msvc/release/summon.exe

cp target/x86_64-pc-windows-msvc/release/summon.exe /mnt/c/Users/<username>/
```

## First install (PowerShell on Windows)

```powershell
.\summon.exe status    # verify binary works; shows config path
.\summon.exe edit      # create and open config at %APPDATA%\summon\config.toml
.\summon.exe run       # test in foreground first
.\summon.exe install   # register Task Scheduler entry for login autostart
```

No Accessibility permission prompt — Windows has no equivalent of macOS TCC for hotkeys.

## Deploying code changes

Task Scheduler does not auto-respawn on stop, so after rebuilding you need to restart manually:

```bash
# WSL: rebuild and copy
cargo xwin build --release --target x86_64-pc-windows-msvc
cp target/x86_64-pc-windows-msvc/release/summon.exe /mnt/c/Users/<username>/
```

```powershell
# PowerShell: stop old daemon, start new binary
.\summon.exe stop
.\summon.exe run
```

## Config

Config lives at `%APPDATA%\summon\config.toml`. Use `summon edit` to open it.

Use the **exe stem** (filename without `.exe`, case-insensitive) as the app identifier — bundle IDs don't exist on Windows:

```toml
[settings]
# cycle_reset_ms = 1500
# hide_previous = false
# hold_threshold_ms = 0

[bindings]
"ctrl+1" = "firefox"
"ctrl+2" = "Code"
"ctrl+3" = "WindowsTerminal"
```

## Editor setup for `summon edit`

`EDITOR` / `VISUAL` env vars are respected. VS Code requires `code.cmd --wait` — the bare `code` file is a shell script that `CreateProcess` can't execute directly:

```powershell
$env:EDITOR = "code.cmd --wait"
.\summon.exe edit
```

To make this permanent:
```powershell
[System.Environment]::SetEnvironmentVariable("EDITOR", "code.cmd --wait", "User")
```

Without `--wait`, VS Code launches and exits immediately and summon thinks editing is done before you've changed anything.

## Uninstall

```powershell
.\summon.exe uninstall   # removes Task Scheduler entry
```

## Log locations

```
%LOCALAPPDATA%\summon\Logs\stdout.log
%LOCALAPPDATA%\summon\Logs\stderr.log
```

## File locations summary

| Purpose      | Path |
|--------------|------|
| Config       | `%APPDATA%\summon\config.toml` |
| PID file     | `%LOCALAPPDATA%\summon\summon.pid` |
| Logs         | `%LOCALAPPDATA%\summon\Logs\` |
