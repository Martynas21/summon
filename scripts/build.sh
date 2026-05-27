#!/usr/bin/env bash
set -euo pipefail

# Wraps `cargo build --release` + ad-hoc codesign with a STABLE identifier.
#
# Why: macOS Accessibility (TCC) keys trust by code-signing identifier. Rust's
# default ad-hoc signing produces an identifier with a random suffix per build
# (e.g. "summon-d0733a5f438274c9"), so every rebuild silently invalidates the
# user's existing Accessibility grant. Forcing a fixed identifier means the
# user grants once and the trust survives all future builds.

cd "$(dirname "$0")/.."

# Make sure the brew-installed rustup shims are on PATH (in case the script
# runs from a launchd or other non-login context).
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

cargo build --release "$@"

BIN="$(pwd)/target/release/summon"
IDENT="dev.summon.daemon"

codesign --force --identifier "$IDENT" --sign - "$BIN"

echo
echo "built and signed:"
codesign -dv "$BIN" 2>&1 | grep -E '^Identifier|^CDHash'
