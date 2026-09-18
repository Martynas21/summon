#!/usr/bin/env bash
set -euo pipefail

# Runs the test suite under cargo-llvm-cov and reports line coverage.
#
# Two numbers matter:
#   1. The default report — coverage of everything that compiles on the host.
#   2. The "testable surface" report — excludes only the modules that are
#      pure OS entry points (see .claude/skills/rust-tests): daemon.rs
#      (run loop / signal handlers), main.rs, launchd.rs, and the Cocoa/AX
#      wrappers (app, window, screen, dispatch, proc, permissions).
#
# Principle: test our code's outcomes; never test OS methods, never mock
# them. Known OS-bound remainders inside the included files (kept visible
# in the report rather than hidden by exclusion):
#   - hotkey.rs HotkeyRegistry        — registers real global hotkeys
#   - tui.rs run/event_loop/save_config — real terminal, real config path
#   - cli.rs run dispatch, status/edit/install — spawn editors, query daemon
#   - ipc.rs stop/reload senders      — signal real processes
# Everything else should stay at 100% line coverage.
#
# One-time setup:
#   rustup component add llvm-tools-preview
#   cargo install cargo-llvm-cov --locked
#
# Usage:
#   ./scripts/coverage.sh           # text summary (both reports)
#   ./scripts/coverage.sh --html    # also write HTML to target/llvm-cov/html

cd "$(dirname "$0")/.."

if ! cargo llvm-cov --version >/dev/null 2>&1; then
    echo "cargo-llvm-cov not installed. One-time setup:" >&2
    echo "  rustup component add llvm-tools-preview" >&2
    echo "  cargo install cargo-llvm-cov --locked" >&2
    exit 1
fi

UNTESTABLE='src/(daemon|main|launchd|app|window|screen|dispatch|proc|permissions)\.rs$'

echo "=== Full report (everything compiled on host) ==="
cargo llvm-cov --summary-only

echo
echo "=== Testable surface (policy-excluded modules ignored) ==="
cargo llvm-cov report --summary-only --ignore-filename-regex "$UNTESTABLE"

if [[ "${1:-}" == "--html" ]]; then
    cargo llvm-cov report --html --ignore-filename-regex "$UNTESTABLE"
    echo "HTML report: target/llvm-cov/html/index.html"
fi
