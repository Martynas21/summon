# macOS Deploy Guide

## Why `scripts/build.sh` exists (don't skip it)

macOS TCC records the binary's *designated requirement* at first grant. Default ad-hoc codesign uses the CDHash — every rebuild invalidates the saved grant. The script forces `designated => identifier "dev.summon.daemon"`, so any future build with the same identifier inherits the existing Accessibility grant.

**Use the script for any release build you intend to run, not bare `cargo build --release`.**

## First install

```bash
./scripts/build.sh
target/release/summon run       # one-time TCC prompt — must be run from a TTY
target/release/summon install   # writes ~/Library/LaunchAgents/dev.summon.daemon.plist + bootstraps
target/release/summon status    # verify: pid, AX permission granted, config valid
```

The `run` step before `install` is required to trigger the Accessibility permission modal. It must be launched from a terminal (not launchd) so macOS presents the prompt to the correct parent process.

## Deploying code changes

`summon reload` is **SIGHUP only** — it re-reads config and re-registers hotkeys, but does **not** swap the binary. To pick up code changes:

```bash
./scripts/build.sh
target/release/summon stop    # SIGTERM; KeepAlive=true → launchd respawns from new binary
target/release/summon status  # verify new pid, AX still granted
```

Using `reload` after a rebuild is a silent footgun: it succeeds but the old binary keeps running.

## TCC grant flow (how `summon install` works)

`summon install` is fiddly because TCC caches its trust answer per-process and records the parent's identity at first prompt.

`launchd.rs::prompt_for_grant_via_launchd` works around both:

1. Writes a temporary `dev.summon.grant` LaunchAgent plist pointing at `summon _grant`.
2. In a loop: bootstrap helper → wait for `/tmp/summon-grant.status` ("ok" or "fail") → bootout. Each iteration is a fresh process under launchd attribution, so TCC re-evaluates cleanly.
3. `/tmp/summon-grant.prompted` sentinel ensures the modal fires exactly once.
4. After "ok", writes the real `dev.summon.daemon.plist` (with `KeepAlive=true`) and bootstraps it.

**Do not call `request_trust()` from the long-running daemon under launchd KeepAlive** — without the `is_foreground()` TTY check in `daemon::run`, every respawn after a denied grant would re-fire the modal.

## Uninstall

```bash
target/release/summon uninstall   # bootout + removes plist
```

## Log locations

```
~/Library/Logs/summon/stdout.log
~/Library/Logs/summon/stderr.log
```

`RUST_LOG` (e.g. `RUST_LOG=debug`) controls verbosity via `tracing-subscriber` `EnvFilter`.
