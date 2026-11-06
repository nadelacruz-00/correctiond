#!/usr/bin/env bash
# Headless smoke test of the IBus engine. Touches nothing outside a temp dir:
# a private session bus (dbus-run-session), a private ibus-daemon on a temp
# socket, HOME and XDG_* in the temp dir. Never switches the desktop's input
# method. Task 10 grows this into the full E2E.
#
# Usage: bash scripts/headless-smoke.sh   (builds target/release/engine)
#        SMOKE_ENGINE_BIN=/path/to/engine bash scripts/headless-smoke.sh
#        (tests that binary instead, without building)
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="${SMOKE_ENGINE_BIN:-$repo/target/release/engine}"
helper="$repo/scripts/headless_smoke.py"

if [[ "${1:-}" != "--inner" ]]; then
    if [[ -z "${SMOKE_ENGINE_BIN:-}" ]]; then
        (cd "$repo" && cargo build --release --quiet)
    fi
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    mkdir -p "$tmp/home/.config" "$tmp/home/.local/share" "$tmp/home/.local/state" \
        "$tmp/home/.cache" "$tmp/run"
    chmod 700 "$tmp/run"
    status=0
    env -i PATH="$PATH" LANG="${LANG:-C.UTF-8}" \
        HOME="$tmp/home" \
        XDG_CONFIG_HOME="$tmp/home/.config" \
        XDG_DATA_HOME="$tmp/home/.local/share" \
        XDG_STATE_HOME="$tmp/home/.local/state" \
        XDG_CACHE_HOME="$tmp/home/.cache" \
        XDG_RUNTIME_DIR="$tmp/run" \
        GSETTINGS_BACKEND=memory \
        GIO_USE_VFS=local GVFS_DISABLE_FUSE=1 \
        SMOKE_TMP="$tmp" SMOKE_ENGINE_BIN="$bin" \
        dbus-run-session -- bash "${BASH_SOURCE[0]}" --inner || status=$?
    exit "$status"
fi

# --- inside the private session bus ---
tmp="$SMOKE_TMP"
sock="$tmp/ibus"
trace="$HOME/.local/state/correctiond/trace.log"
pids=()
cleanup() {
    for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
}
trap cleanup EXIT

wait_for() { # seconds, command...
    local n=$(($1 * 20)); shift
    for ((i = 0; i < n; i++)); do "$@" && return 0; sleep 0.05; done
    return 1
}

ibus-daemon --single --panel=disable --config=disable --emoji-extension=disable \
    --cache=none --address="unix:path=$sock" >"$tmp/ibus-daemon.log" 2>&1 &
pids+=($!)
wait_for 10 test -S "$sock" || { echo "FAIL: ibus-daemon did not start"; cat "$tmp/ibus-daemon.log"; exit 1; }
export IBUS_ADDRESS="unix:path=$sock"
echo "ibus-daemon: $(ibus-daemon --version 2>/dev/null || true) on $sock"

CORRECTIOND_TRACE=1 "$bin" --register 2>"$tmp/engine.stderr" &
epid=$!
pids=("$epid" "${pids[@]}")
wait_for 30 grep -q " ready" "$trace" 2>/dev/null || {
    echo "FAIL: engine not ready"; cat "$tmp/engine.stderr"; exit 1; }
echo "engine pid $epid ready; $(grep ' dictionary ' "$trace" | cut -d' ' -f2-)"

owner="$(stat -c %U "/proc/$epid/mem")"
echo "stat -c %U /proc/$epid/mem: $owner"
[[ "$owner" == root ]] || { echo "FAIL: engine is dumpable"; exit 1; }
core="$(awk '/Max core file size/ {print $5 "/" $6}' "/proc/$epid/limits")"
echo "RLIMIT_CORE soft/hard: $core"
[[ "$core" == 0/0 ]] || { echo "FAIL: core limit not zero"; exit 1; }
inet="$(ss -tuanpH 2>/dev/null | grep -c "pid=$epid," || true)"
echo "inet sockets owned by engine: $inet"
[[ "$inet" == 0 ]] || { echo "FAIL: engine opened a network socket"; exit 1; }

# Run the trace check even if the client failed: both verdicts are useful.
client=0
python3 -I "$helper" || client=$?

kill -TERM "$epid"
wait "$epid" || true
pids=("${pids[@]:1}")
traced=0
python3 -I "$helper" --check-trace "$trace" || traced=$?
if [[ -s "$tmp/engine.stderr" ]]; then
    echo "engine stderr:"; sed 's/^/  stderr| /' "$tmp/engine.stderr"
fi
if [[ "$client" != 0 || "$traced" != 0 ]]; then
    echo "headless smoke: FAIL"
    exit 1
fi
echo "headless smoke: PASS"
