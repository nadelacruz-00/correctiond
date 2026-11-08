#!/usr/bin/env bash
# Headless E2E of the keystroke-to-commit path through a real ibus-daemon.
# Touches nothing outside a temp dir: a private session bus
# (dbus-run-session, so org.nadelacruz.Correctiond1 is claimed only there),
# a private ibus-daemon on a temp socket, HOME and XDG_* in the temp dir.
# Never switches the desktop's input method, never reaches the real
# keyring. The client is tests/e2e/ibus_roundtrip.py.
#
# Usage: bash scripts/e2e.sh   (builds target/release/engine)
#        E2E_ENGINE_BIN=/path/to/engine bash scripts/e2e.sh
#        (tests that binary instead, without building)
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="${E2E_ENGINE_BIN:-$repo/target/release/engine}"
client="$repo/tests/e2e/ibus_roundtrip.py"

if [[ "${1:-}" != "--inner" ]]; then
    if [[ -z "${E2E_ENGINE_BIN:-}" ]]; then
        (cd "$repo" && cargo build --release --quiet)
    fi
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    mkdir -p "$tmp/home/.config/correctiond" "$tmp/home/.local/share" \
        "$tmp/home/.local/state" "$tmp/home/.cache" "$tmp/run"
    chmod 700 "$tmp/run"
    # The app "e2e" is allowlisted; any caller may use the control plane
    # (there is no GNOME Shell on this bus). One small fixture lexicon: this
    # tests the keystroke-to-commit path, not scoring. (With the shipped
    # list a field's first "bulid" stays below the 0.6 open margin; see
    # builtin_english_fixes_bulid_after_an_english_word in main.rs.)
    printf 'the\t1000\nhello\t80\nbuild\t50\n' >"$tmp/home/en.txt"
    cat >"$tmp/home/.config/correctiond/config.toml" <<TOML
allowlist = ["e2e"]
allow_any_caller = true

[[languages]]
code = "en"
lexicon = "$tmp/home/en.txt"
TOML
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
        E2E_TMP="$tmp" E2E_ENGINE_BIN="$bin" \
        dbus-run-session -- bash "${BASH_SOURCE[0]}" --inner || status=$?
    exit "$status"
fi

# --- inside the private session bus ---
tmp="$E2E_TMP"
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

CORRECTIOND_TRACE=1 "$bin" --register 2>"$tmp/engine.stderr" &
epid=$!
pids=("$epid" "${pids[@]}")
wait_for 30 grep -q " ready" "$trace" 2>/dev/null || {
    echo "FAIL: engine not ready"; cat "$tmp/engine.stderr"; exit 1; }
echo "engine pid $epid ready on a private session bus and ibus-daemon"

status=0
python3 -I -u "$client" || status=$?

# `engine import` end to end: the fd goes over this private bus to the
# running engine. There is no keyring here, so the store is memory-only and
# a valid file must be refused. Output must never quote content or paths.
import_check() { # label, expected line, file
    local out rc=0
    out="$("$bin" import "$3" 2>&1)" || rc=$?
    if [[ "$rc" == 1 && "$out" == "$2" ]]; then
        echo "PASS import $1: exit 1, \"$out\""
    else
        echo "FAIL import $1: exit $rc, \"$out\""
        status=1
    fi
}
printf '#correctiond-vocab v1\n# fixture\nw\tsikretongsalita\t5\nb\tsikretongsalita\tnaman\t3\n' \
    >"$tmp/fixture.vocab.tsv"
printf 'sikretongsalita\n' >"$tmp/bad.vocab.tsv"
import_check "refused without keyring" \
    "learning store not available; unlock your keyring and retry" "$tmp/fixture.vocab.tsv"
import_check "bad header" "not a correctiond vocab v1 file" "$tmp/bad.vocab.tsv"
import_check "missing file" "cannot open the file" "$tmp/missing-sikretongsalita.vocab.tsv"
rc=0
out="$("$bin" import 2>&1)" || rc=$?
if [[ "$rc" == 64 && "$out" == "usage: engine import FILE" ]]; then
    echo "PASS import usage: exit 64, \"$out\""
else
    echo "FAIL import usage: exit $rc, \"$out\""
    status=1
fi
if grep -q sikretongsalita "$tmp/engine.stderr" "$trace" 2>/dev/null; then
    echo "FAIL import: a fixture word reached the engine's logs"; status=1
fi

# `engine doctor` against the private bus and ibus-daemon. The component
# file is generated into a temp dir with exec set to this binary. A unique
# word is typed first; neither output may contain it.
mkdir -p "$tmp/component"
sed "s|<exec>[^<]*</exec>|<exec>$bin</exec>|" "$repo/data/correctiond.xml" \
    >"$tmp/component/correctiond.xml"
python3 -I -u "$client" teach-word || status=1
rc=0
out=$(IBUS_COMPONENT_PATH="$tmp/component" "$bin" doctor) || rc=$?
rcj=0
outj=$(IBUS_COMPONENT_PATH="$tmp/component" "$bin" doctor --json) || rcj=$?
doctor_ok() { # label, rc, output, expected line
    if [[ "$2" -le 2 ]] && grep -qF -- "$4" <<<"$3"; then
        echo "PASS doctor $1: \"$4\" (exit $2)"
    else
        echo "FAIL doctor $1: missing \"$4\" or exit $2"; echo "$3" | sed 's/^/  doctor| /'
        status=1
    fi
}
doctor_ok "ibus" "$rc" "$out" "ibus: ok"
doctor_ok "engine_running" "$rc" "$out" "engine_running: ok"
doctor_ok "component" "$rc" "$out" "component: ok"
doctor_ok "json" "$rcj" "$outj" '"key":"engine_running","status":"ok"'
if grep -q zorblaxite <<<"$out$outj"; then
    echo "FAIL doctor: a typed word reached the output"; status=1
else
    echo "PASS doctor: no typed word in text or --json output"
fi
rc=0
err="$("$bin" doctor --bogus 2>&1)" || rc=$?
if [[ "$rc" == 64 && "$err" == "usage: correctiond doctor [--json]" ]]; then
    echo "PASS doctor usage: exit 64"
else
    echo "FAIL doctor usage: exit $rc, \"$err\""; status=1
fi

stop_engine() {
    kill -TERM "$epid"
    wait "$epid" || true
    pids=("${pids[@]:1}")
}
stop_engine
if [[ -s "$tmp/engine.stderr" ]]; then
    echo "engine stderr:"; sed 's/^/  stderr| /' "$tmp/engine.stderr"
fi

# (4) Taglish: the shipped English list plus a Tagalog lexicon at the path
# data/ATTRIBUTION.md documents (the config default). A fresh engine per
# case, since lexicons load at startup. The store is memory-only (no
# keyring), so nothing learned in one case carries into the next.
tl_rel=".local/share/correctiond/lexicons/tl.txt"
tl="$HOME/$tl_rel"
cat >"$HOME/.config/correctiond/config.toml" <<TOML
allowlist = ["e2e"]
allow_any_caller = true

[[languages]]
code = "en"
lexicon = "builtin:en"

[[languages]]
code = "tl"
lexicon = "~/$tl_rel"
TOML
taglish() { # case
    [[ -f "$trace" ]] && mv "$trace" "$trace.$1.prev"
    CORRECTIOND_TRACE=1 "$bin" --register 2>"$tmp/engine-$1.stderr" &
    epid=$!
    pids=("$epid" "${pids[@]}")
    if ! wait_for 30 grep -q " ready" "$trace" 2>/dev/null; then
        echo "FAIL $1: engine not ready"; cat "$tmp/engine-$1.stderr"; status=1
        stop_engine; return
    fi
    python3 -I -u "$client" "$1" || status=1
    stop_engine
}

# (4a) Absent: the case must bite, and startup says why, once, with the path.
rm -f "$tl"
taglish tl-absent
want="correctiond: WARNING: no lexicon for language 'tl' at $tl;"
if [[ "$(grep -cF -- "$want" "$tmp/engine-tl-absent.stderr")" == 1 ]]; then
    echo "PASS (4a) missing TL lexicon: one WARNING line naming 'tl' and $tl_rel"
else
    echo "FAIL (4a) missing TL lexicon: expected one line starting \"$want\""; status=1
fi

# (4b) Present: built with the documented command (the builder creates the
# directories, 0700).
python3 -I "$repo/scripts/build-lexicon.py" --lang tl \
    --words <(printf 'naman\nkain\nopo\n') --out "$tl"
taglish tl-present
if grep -q "WARNING: no lexicon" "$tmp/engine-tl-present.stderr"; then
    echo "FAIL (4b): the TL lexicon was built but the engine still warns"; status=1
fi
for d in "$HOME/.local/share/correctiond" "$HOME/.local/share/correctiond/lexicons"; do
    if [[ "$(stat -c %a "$d")" != 700 ]]; then
        echo "FAIL (4b): $d is not 0700"; status=1
    fi
done
for f in "$tmp"/engine-tl-*.stderr; do
    echo "engine stderr ($(basename "$f" .stderr)):"; sed 's/^/  stderr| /' "$f"
done
if grep -q "naman\|haman" "$tmp"/engine-tl-*.stderr "$trace"* 2>/dev/null; then
    echo "FAIL (4): a typed word reached the engine's logs"; status=1
fi

if [[ "$status" != 0 ]]; then
    # Key classes and event names only (trace.rs): safe to print.
    for t in "$trace"*; do sed 's/^/  trace| /' "$t" 2>/dev/null || true; done
    echo "e2e: FAIL"
    exit 1
fi
echo "e2e: PASS"
