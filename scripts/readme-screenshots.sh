#!/usr/bin/env bash
# Makes the README screenshots from synthetic data only. Like
# scripts/extension-smoke.sh it runs `gnome-shell --headless` with HOME, XDG_*
# and GSettings (keyfile backend) in a temp dir, on a private session bus and
# a private system bus, with scripts/fake_correctiond.py (mode `readme`:
# invented counts and recent words) standing in for the engine. It never
# reads or touches the desktop session, the real engine or its data.
#
# Shots (PNG, via Shell.Screenshot through org.gnome.Shell.Eval, which the
# test-only helper scripts/smoke-unsafe@correctiond.test enables in the temp
# HOME only):
#   quick-settings.png       Quick Settings with the Autocorrect menu open,
#                            over a "Text Editor" window (scripts/readme_screens.js)
#   settings-general.png     the Settings window, General page
#   settings-apps.png        ... Apps page (allowlist from a synthetic config.toml)
#   settings-languages.png   ... Languages page (synthetic lexicon headers)
#   settings-learning.png    ... Learning page (a synthetic import-report.json)
#   settings-words.png       ... Words page, invented words (stub wordsCalls)
#   osk-ergonomic-split.png  GJS OSK with the "landscape ergonomic v5" custom
#                            layout, over the editor window; only when gjs-osk
#                            is installed and that layout exists. The layout is
#                            the one real read: a read-only `gsettings get` of
#                            gjs-osk's custom-layout key.
# The Settings window is the real fillWindow (extension/prefsWindow.js) run
# standalone in scripts/readme_screens.js, not through GNOME's preferences
# service, so the Words page needs no Shell prompt.
#
# Usage: bash scripts/readme-screenshots.sh [OUT_DIR]   (default docs/screenshots)
#   KEEP=1 keeps the temp dir (logs, raw PNGs) and prints its path.
# Needs gnome-shell 50, gjs, dbus-run-session, python3; pngquant or optipng
# are used when installed. Safe to rerun: it overwrites the PNGs it makes.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
uuid=correctiond-companion@nadelacruz-00.github.io
osk_uuid=gjsosk@vishram1123.com
osk_dir=/usr/share/gnome-shell/extensions/$osk_uuid
unsafe_uuid=smoke-unsafe@correctiond.test
osk_layout_name='landscape ergonomic v5'
width=1600 height=1000

if [[ "${1:-}" == --inner ]]; then
    # --- inside the private session bus ---
    log="$SHOTS_DIR/run.log"
    sysaddr="unix:path=$XDG_RUNTIME_DIR/sysbus"
    dbus-daemon --session --nofork --address="$sysaddr" &
    sysbus=$!
    sleep 0.3
    export DBUS_SYSTEM_BUS_ADDRESS="$sysaddr"
    python3 -I "$repo/scripts/fake_correctiond.py" "$SHOTS_DIR/calls.log" readme &
    fake=$!
    gnome-shell --headless --wayland --no-x11 --virtual-monitor "${width}x${height}" \
        > "$SHOTS_DIR/shell.log" 2>&1 &
    shell=$!
    editor='' prefs=''
    cleanup_inner() {
        kill "$editor" "$prefs" 2>/dev/null || true
        kill -INT "$shell" 2>/dev/null || true
        wait "$shell" 2>/dev/null || true
        kill "$fake" "$sysbus" 2>/dev/null || true
    }
    trap cleanup_inner EXIT
    say() { echo "$*" >> "$log"; }
    # ev JS: Eval's string result on stdout; returns 1 when Eval fails.
    ev() {
        local reply
        reply="$(gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell \
            -m org.gnome.Shell.Eval "$1" 2>&1)" || true
        if [[ "$reply" == "(true, "* ]]; then
            reply="${reply#(true, \'}"; printf '%s' "${reply%\')}"
            return 0
        fi
        [[ "${QUIET:-}" == 1 ]] || say "EVAL-FAILED: $reply"
        return 1
    }
    # wait_for FILE TEXT: until FILE has TEXT (up to 15 s).
    wait_for() {
        for _ in $(seq 60); do
            grep -qF -- "$2" "$1" 2>/dev/null && return 0
            sleep 0.25
        done
        say "TIMEOUT $2"
        return 1
    }
    client() { # client LOG ARGS...: a GTK client of the headless shell
        local out=$1; shift
        WAYLAND_DISPLAY=wayland-0 GDK_BACKEND=wayland GSK_RENDERER=cairo GTK_A11Y=none \
            gjs -m "$repo/scripts/readme_screens.js" "$@" > "$out" 2>&1
    }
    focus() { # focus PID
        ev "(() => { const w = global.get_window_actors().map(a => a.meta_window)
            .find(w => w.get_pid() === $1); if (!w) throw new Error('no window');
            Main.activateWindow(w); return 'ok'; })()" > /dev/null
        sleep 0.5
    }
    # shot NAME RECT_JS: RECT_JS is a JS expression giving [x, y, w, h]; the
    # area (clamped to the screen) is written to $SHOTS_DIR/raw/NAME.png.
    shot() {
        local path="$SHOTS_DIR/raw/$1.png"
        rm -f "$path"
        # No notices (the headless shell says the screen lock is off).
        ev 'Main.messageTray.getSources().forEach(s => s.destroy())' > /dev/null || true
        sleep 0.5
        ev "(() => { const {Gio, Shell} = imports.gi;
            const [sw, sh] = [global.stage.width, global.stage.height];
            let [x, y, w, h] = ($2).map(Math.round);
            x = Math.max(0, x); y = Math.max(0, y);
            w = Math.min(w, sw - x); h = Math.min(h, sh - y);
            const stream = Gio.File.new_for_path('$path').replace(null, false, Gio.FileCreateFlags.NONE, null);
            globalThis.readmeShot = 'pending';
            new Shell.Screenshot().screenshot_area(x, y, w, h, stream, (o, res) => {
                try { o.screenshot_area_finish(res); globalThis.readmeShot = 'ok'; }
                catch (e) { globalThis.readmeShot = 'error ' + e.message; }
                finally { stream.close(null); }
            });
            return [x, y, w, h].join(','); })()" > "$SHOTS_DIR/rect" || return 1
        say "RECT $1 $(cat "$SHOTS_DIR/rect")"
        for _ in $(seq 40); do
            local state; state="$(ev 'globalThis.readmeShot')" || return 1
            case "$state" in
            '"ok"') say "SHOT $1"; return 0 ;;
            '"pending"') sleep 0.25 ;;
            *) say "SHOT-FAILED $1: $state"; return 1 ;;
            esac
        done
        say "SHOT-TIMEOUT $1"; return 1
    }
    # The rectangle of PID's window, MARGIN px around its frame.
    win_rect() { # win_rect PID MARGIN
        printf '%s' "(() => { const w = global.get_window_actors().map(a => a.meta_window)
            .find(w => w.get_pid() === $1); const r = w.get_frame_rect();
            return [r.x - $2, r.y - $2, r.width + 2 * $2, r.height + 2 * $2]; })()"
    }

    # The shell is up and Eval works (the helper extension has run).
    for _ in $(seq 80); do
        QUIET=1 ev '1' > /dev/null && break
        sleep 0.5
    done
    grep -q '^owned ' "$SHOTS_DIR/calls.log" || say "the fake engine did not start"
    sleep 2

    # --- Quick Settings over the editor ---
    client "$SHOTS_DIR/editor.log" editor &
    wait_for "$SHOTS_DIR/editor.log" 'SCREENS ready' || exit 1
    editor="$(sed -n 's/^SCREENS ready //p' "$SHOTS_DIR/editor.log")"
    sleep 1
    focus "$editor"
    ev "(() => { const e = Main.extensionManager.lookup('$uuid').stateObj;
        const qs = Main.panel.statusArea.quickSettings;
        const t = qs.menu._grid.get_children()
            .find(c => c.constructor.\$gtype.name === 'CorrectiondCompanionToggle');
        if (!t) throw new Error('toggle not in Quick Settings');
        Main.panel.closeQuickSettings(); qs.menu.open(); t.menu.open(); return 'ok'; })()" > /dev/null
    sleep 2
    # The menu's box (menu.actor is a 0x0 wrapper), with the panel above it
    # and some of the focused window to its left.
    shot quick-settings "(() => { const a = Main.panel.statusArea.quickSettings.menu.box;
        const [x, y] = a.get_transformed_position(); const [w, h] = a.get_transformed_size();
        return [x - 160, 0, global.stage.width - x + 160, y + h + 40]; })()" || true
    ev 'Main.panel.closeQuickSettings()' > /dev/null || true
    sleep 1

    # --- the Settings window, over the plain background ---
    ev "global.get_window_actors().map(a => a.meta_window)
        .find(w => w.get_pid() === $editor)?.minimize()" > /dev/null || true
    rm -f "$SHOTS_DIR/prefs.fifo"; mkfifo "$SHOTS_DIR/prefs.fifo"
    client "$SHOTS_DIR/prefs.log" prefs "$SHOTS_CONFIG" "$SHOTS_REPORT" < "$SHOTS_DIR/prefs.fifo" &
    exec 3> "$SHOTS_DIR/prefs.fifo"
    if wait_for "$SHOTS_DIR/prefs.log" 'SCREENS ready'; then
        prefs="$(sed -n 's/^SCREENS ready //p' "$SHOTS_DIR/prefs.log")"
        sleep 1.5
        focus "$prefs"
        # PAGE:HEIGHT, the window just taller than the page's content (the
        # Learning page is cut after "Your words", the Words list scrolls).
        for spec in General:580 Apps:680 Languages:540 Learning:880 Words:900; do
            page="${spec%:*}"
            echo "$page ${spec#*:}" >&3
            wait_for "$SHOTS_DIR/prefs.log" "SCREENS page $page" || continue
            sleep 0.7
            shot "settings-${page,,}" "$(win_rect "$prefs" 32)" || true
        done
        kill "$prefs" 2>/dev/null || true
        prefs=''
    fi
    exec 3>&-
    sleep 1

    # --- GJS OSK over the editor ---
    if [[ "$SHOTS_OSK" == 1 ]]; then
        focus "$editor"
        gsettings --schemadir "$osk_dir/schemas" \
            set org.gnome.shell.extensions.gjsosk.indicator opened true 2>> "$log" || true
        sleep 4
        shot osk-ergonomic-split "[0, 0, global.stage.width, global.stage.height]" || true
    fi
    exit 0
fi

# --- outside: build the temp HOME, run the inner part, keep the PNGs ---

out="${1:-$repo/docs/screenshots}"; mkdir -p "$out"
tmp="$(mktemp -d /tmp/ccs.XXXXXX)" # short: Wayland socket paths max 108 bytes
cleanup() {
    fusermount3 -u -q "$tmp/run/doc" 2>/dev/null || true
    if [[ "${KEEP:-}" == 1 ]]; then echo "kept $tmp"; else rm -rf "$tmp"; fi
}
trap cleanup EXIT
home="$tmp/home" dir="$tmp/shots"
mkdir -p "$home/.config/glib-2.0/settings" "$home/.config/correctiond" \
    "$home/.local/share/correctiond/lexicons" "$home/.cache" "$home/.local/state" \
    "$tmp/run" "$dir/raw"
chmod 700 "$tmp/run"
extdir="$home/.local/share/gnome-shell/extensions/$uuid"
mkdir -p "$extdir" "$home/.local/share/gnome-shell/extensions/$unsafe_uuid"
install -m 0644 "$repo/extension/metadata.json" "$repo/extension/extension.js" \
    "$repo/extension/lib.js" "$repo/extension/config.js" "$repo/extension/prefs.js" \
    "$repo/extension/prefsWindow.js" "$repo/extension/wordsPage.js" "$extdir/"
install -m 0644 "$repo/scripts/$unsafe_uuid/metadata.json" "$repo/scripts/$unsafe_uuid/extension.js" \
    "$home/.local/share/gnome-shell/extensions/$unsafe_uuid/"

# Synthetic engine settings and import report (docs/import-report.md).
config="$home/.config/correctiond/config.toml"
cat > "$config" <<'EOF'
osk_source = "correctiond:us"
allowlist = [
    "firefox",
    "org.gnome.TextEditor",
    "chromium",
]
broken = []

[[languages]]
code = "en"
lexicon = "builtin:en"

[[languages]]
code = "tl"
lexicon = "~/.local/share/correctiond/lexicons/tl.txt"

[[languages]]
code = "war"
lexicon = "~/.local/share/correctiond/lexicons/war.txt"
EOF
printf '# lexicon lang=tl entries=48210\n' > "$home/.local/share/correctiond/lexicons/tl.txt"
printf '# lexicon lang=war entries=21377\n' > "$home/.local/share/correctiond/lexicons/war.txt"
report="$home/.local/share/correctiond/import-report.json"
cat > "$report" <<'EOF'
{
  "imported_at": "2026-11-02T09:30:00Z",
  "own_messages": 12345,
  "threads": 57,
  "words": 2104,
  "pairs": 6518,
  "names_skipped": 311,
  "gate": {"result": "passed", "failed_checks": []},
  "lists_installed": true,
  "tl_added": 412,
  "war_added": 187,
  "replay": {
    "qwerty": {
      "full": {
        "baseline": {"chat_changed_rate": 0.083, "fil_changed_rate": 0.061, "typo_fix_rate": 0.52,
                     "oov_changed_count": 940, "slang_changed_rate": 0.147},
        "rebuild": {"chat_changed_rate": 0.012, "fil_changed_rate": 0.009, "typo_fix_rate": 0.71,
                    "oov_changed_count": 121, "slang_changed_rate": 0.021}
      }
    }
  },
  "typo_share": {"evaluation": 0.02, "production_delta": 0.01}
}
EOF

# GSettings: dark style, our extensions; gjs-osk with the ergonomic layout.
exts="'$unsafe_uuid', '$uuid'"
osk_section=''
with_osk=0
if [[ -f "$osk_dir/metadata.json" ]]; then
    # The one real read: gjs-osk's custom layouts, read-only; keep only the named one.
    layout="$(gsettings --schemadir "$osk_dir/schemas" get org.gnome.shell.extensions.gjsosk custom-layout \
        2>/dev/null | python3 -I -c '
import ast, json, sys
name, builtins = sys.argv[1], sys.argv[2]
try:
    raw = ast.literal_eval(sys.stdin.read().strip())
    entries = [json.loads(e) if isinstance(e, str) else e for e in json.loads(raw)]
except Exception:
    sys.exit(0)
for e in entries:
    if isinstance(e, list) and e and isinstance(e[-1], dict) and e[-1].get("name") == name:
        # layout-landscape: the first custom layout comes after the built-in ones.
        print(len(json.load(open(builtins))))
        # custom-layout, as a keyfile value: a GVariant string holding a
        # one-entry JSON array (repr is valid GVariant text: no single quotes in it).
        print(repr(json.dumps([json.dumps(e)])))
        break
' "$osk_layout_name" "$osk_dir/physicalLayouts.json")" || layout=''
    if [[ -n "$layout" ]]; then
        with_osk=1
        exts="$exts, '$osk_uuid'"
        osk_section="[org/gnome/shell/extensions/gjsosk]
custom-layout=$(sed -n 2p <<< "$layout")
layout-landscape=$(sed -n 1p <<< "$layout")
landscape-width-percent=42
landscape-height-percent=24
border-spacing-px=6
"
    else
        echo "skipping osk-ergonomic-split.png: no \"$osk_layout_name\" layout in gjs-osk's settings"
    fi
else
    echo "skipping osk-ergonomic-split.png: gjs-osk is not installed"
fi
cat > "$home/.config/glib-2.0/settings/keyfile" <<EOF
[org/gnome/shell]
enabled-extensions=[$exts]
disable-user-extensions=false
welcome-dialog-last-shown-version='999'

[org/gnome/desktop/interface]
color-scheme='prefer-dark'
accent-color='blue'
clock-show-weekday=true

[org/gnome/desktop/input-sources]
sources=[('xkb', 'us+colemak')]

$osk_section
EOF

env -i PATH="$PATH" LANG=C.UTF-8 HOME="$home" USER=user LOGNAME=user \
    XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
    XDG_CACHE_HOME="$home/.cache" XDG_STATE_HOME="$home/.local/state" \
    XDG_RUNTIME_DIR="$tmp/run" GSETTINGS_BACKEND=keyfile \
    GIO_USE_VFS=local GVFS_DISABLE_FUSE=1 \
    SHOTS_DIR="$dir" SHOTS_CONFIG="$config" SHOTS_REPORT="$report" SHOTS_OSK="$with_osk" \
    dbus-run-session -- bash "${BASH_SOURCE[0]}" --inner >/dev/null 2>&1 || true

made=0
for png in "$dir"/raw/*.png; do
    [[ -s "$png" ]] || continue
    name="$(basename "$png")"
    if command -v pngquant > /dev/null; then
        pngquant --force --skip-if-larger --quality 70-95 --output "$out/$name" "$png" ||
            cp "$png" "$out/$name"
    else
        cp "$png" "$out/$name"
        command -v optipng > /dev/null && optipng -quiet -o2 "$out/$name"
    fi
    echo "$out/$name ($(du -k "$out/$name" | cut -f1) KB)"
    made=$((made + 1))
done
if grep -qE '^(EVAL-FAILED|TIMEOUT|SHOT-)' "$dir/run.log" 2>/dev/null || [[ "$made" == 0 ]]; then
    echo "some shots failed:" >&2
    cat "$dir/run.log" >&2 2>/dev/null || true
    exit 1
fi
