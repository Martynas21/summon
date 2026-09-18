#!/usr/bin/env bash
set -euo pipefail

# Wraps `cargo build --release` + ad-hoc codesign with a STABLE identifier
# AND a stable DESIGNATED REQUIREMENT.
#
# Why: macOS TCC (Accessibility) records the binary's designated requirement
# when the user first grants access. With plain ad-hoc signing, the default
# designated requirement is the binary's CDHash — content-derived, so every
# rebuild produces a new requirement and TCC silently treats the rebuilt
# binary as untrusted (matches the saved identifier, but the embedded
# requirement is now different).
#
# Forcing a custom designated requirement of `identifier "dev.summon.daemon"`
# means TCC stores an identifier-based requirement, which any future build
# with the same identifier will satisfy regardless of CDHash. Grant once,
# trust survives subsequent rebuilds.

cd "$(dirname "$0")/.."

if [[ "$(uname)" != "Darwin" ]]; then
    echo "build.sh is macOS-only (codesign step); so is summon itself." >&2
    exit 1
fi

# Make sure the brew-installed rustup shims are on PATH (in case the script
# runs from a launchd or other non-login context).
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

cargo build --release "$@"

BIN="$(pwd)/target/release/summon"
IDENT="dev.summon.daemon"

REQS="$(mktemp)"
trap 'rm -f "$REQS"' EXIT
cat >"$REQS" <<EOF
designated => identifier "$IDENT"
EOF

codesign --force \
    --identifier "$IDENT" \
    --requirements "$REQS" \
    --sign - \
    "$BIN"

echo
echo "built and signed:"
codesign -dv "$BIN" 2>&1 | grep -E '^Identifier|^CDHash'
echo
echo "designated requirement:"
codesign -d -r- "$BIN" 2>&1 | grep -E '^designated' || true
