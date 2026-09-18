#!/usr/bin/env bash
set -euo pipefail

# Rebuild and swap the running daemon onto the new binary.
#
# Why this exists: `summon reload` is SIGHUP only — it re-reads config and
# re-registers hotkeys but keeps running the OLD binary. After a rebuild it
# reports success while your code change isn't live, which is the footgun
# docs/deploy-macos.md warns about.
#
# The actual swap relies on the LaunchAgent's KeepAlive=true: SIGTERM the
# daemon and launchd respawns it from the (now rebuilt) path in the plist.
#
# Extra args are passed through to scripts/build.sh → cargo build --release.

cd "$(dirname "$0")/.."

BIN="target/release/summon"
PLIST="$HOME/Library/LaunchAgents/dev.summon.daemon.plist"

# Parse "daemon:  running (pid 1234)". Empty when not running.
daemon_pid() {
    "$BIN" status 2>/dev/null | sed -n 's/^daemon:.*pid \([0-9]\{1,\}\)).*/\1/p'
}

old_pid=""
if [[ -x "$BIN" ]]; then
    old_pid="$(daemon_pid)"
fi

bash scripts/build.sh "$@"
echo

if [[ ! -f "$PLIST" ]]; then
    echo "No LaunchAgent at $PLIST."
    echo "Nothing will respawn automatically. Run:  $BIN install"
    exit 1
fi

if [[ -z "$old_pid" ]]; then
    echo "Daemon was not running; asking launchd to start it."
    launchctl kickstart "gui/$(id -u)/dev.summon.daemon" >/dev/null 2>&1 || true
else
    echo "Stopping pid $old_pid (launchd will respawn from the new binary)..."
    "$BIN" stop >/dev/null
fi

# launchd throttles respawns to ~10s, so allow generous headroom.
new_pid=""
for _ in $(seq 1 40); do
    sleep 0.5
    new_pid="$(daemon_pid)"
    [[ -n "$new_pid" && "$new_pid" != "$old_pid" ]] && break
    new_pid=""
done

echo
if [[ -z "$new_pid" ]]; then
    echo "Daemon did not come back within 20s." >&2
    echo "Check: tail ~/Library/Logs/summon/stderr.log" >&2
    "$BIN" status >&2 || true
    exit 1
fi

"$BIN" status
echo
echo "deployed: pid ${old_pid:-none} → $new_pid"
