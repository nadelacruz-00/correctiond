#!/usr/bin/env bash
# Headless smoke of the companion GNOME Shell extension. Runs
# `gnome-shell --headless` with HOME, XDG_* and GSettings (keyfile backend)
# in a temp dir, on a private session bus AND a private system bus (so the
# shell never reaches logind, GDM or NetworkManager), with
# scripts/fake_correctiond.py standing in for the engine. Never touches the
# desktop session.
#
# Scenario 1: engine up first, GJS OSK enabled; open and close the OSK
#   through gjs-osk's own `opened` setting; disable and re-enable us.
# Scenario 2: no GJS OSK; the engine appears 4 s after the shell.
# Scenario 3 (touch-off): GJS OSK enabled, but config.toml says
#   [modules] touch = false: no OSK watcher, nothing reported.
# Scenario 4 (late-osk): GJS OSK is installed but enabled only 3 s after us
#   (load order): the keyboard is still seen.
# Scenario 5 (v010): the fake in `v0.1.0` mode, the method set of the v0.1.0
#   tag: the menu works, with no stack trace and no UnknownMethod line (the
#   D-Bus compatibility policy in docs/ARCHITECTURE.md).
# Scenarios 1 and 2 then drive the Quick Settings menu through org.gnome.Shell.Eval (the
# test-only helper extension scripts/smoke-unsafe@correctiond.test turns on
# unsafe mode, which Eval needs; it is installed only in the temp HOME):
# toggle, learning switch, a recent word's forget button, the companion's
# ConfirmForget dialog (Cancel; Return, which must cancel too; then Forget,
# whose Purge outlasts the 2 s call timeout), a second disable/enable (the
# pause is read back from the engine), and an engine stop and restart (the
# menu hides the switch and list while it is down; a restarted engine learns).
# Then the fake restarts in two failure modes: ForgetWord refused by a locked
# keyring (the row says so for about 3 s), and an engine from before
# GetStatus (counts shown, no "Engine not running", switch and list hidden).
# Then learned words access: scripts/smoke_words_client.js (test-only; like
# GNOME's preferences service it owns org.gnome.Shell.Extensions with
# Gio.DBus.own_name and shows a plain Adw window with that service's wm_class)
# asks for RequestWordAccess on private connections through the Words page's
# own calls, and the prompt is answered with Allow, Cancel, Return or a
# disable; ListWords and ForgetWords pass through for allowed connections only
# (at most 2, dropped when closed), across an engine restart and a locked
# keyring. gdbus (another process), an unfocused window and a window with
# another wm_class are refused with no prompt.
# Last, GNOME's REAL preferences service (/usr/share/gnome-shell/
# org.gnome.Shell.Extensions) opens our Settings window, the real Words page
# in the real ExtensionPrefsDialog; scripts/smoke_prefs.js (test-only,
# installed as prefs.js in the temp HOME, around the real prefs.js) lets the
# smoke press its buttons. Its wm_class is read and must be the one
# extension.js pins; Show learned words… and Allow list the words.
# Fails on any stack trace through the extension, a failed Eval or a missing call.
#
# Usage: bash scripts/extension-smoke.sh
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
uuid=correctiond-companion@nadelacruz-00.github.io
osk_uuid=gjsosk@vishram1123.com
osk_schemas=/usr/share/gnome-shell/extensions/$osk_uuid/schemas
unsafe_uuid=smoke-unsafe@correctiond.test
# The wm_class (Wayland app id) of GNOME's preferences service's windows: its
# program name. Measured below on the real service's window; extension.js pins it.
settings_wm_class=org.gnome.Shell.Extensions

if [[ "${1:-}" == --inner ]]; then
    # --- inside the private session bus ---
    sysaddr="unix:path=$XDG_RUNTIME_DIR/sysbus"
    dbus-daemon --session --nofork --address="$sysaddr" &
    sysbus=$!
    sleep 0.3
    export DBUS_SYSTEM_BUS_ADDRESS="$sysaddr"
    start_fake() { # start_fake DELAY [MODE]
        (sleep "$1"; exec python3 -I "$repo/scripts/fake_correctiond.py" "$SMOKE_DIR/calls.log" "${2:-normal}") &
        fake=$!
    }
    fake_mode=normal
    [[ "${SMOKE_SCENARIO:-full}" == v010 ]] && fake_mode=v0.1.0
    start_fake "$FAKE_DELAY" "$fake_mode"
    gnome-shell --headless --wayland --no-x11 --virtual-monitor 1280x800 \
        > "$SMOKE_DIR/shell.log" 2>&1 &
    shell=$!
    mark() { echo "--- $1" >> "$SMOKE_DIR/calls.log"; }
    # ext METHOD: the Shell's own org.gnome.Shell.Extensions object (the bus
    # name org.gnome.Shell.Extensions is the Settings process's: the words client's here).
    ext() {
        gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell \
            -m "org.gnome.Shell.Extensions.$1" "$uuid" >> "$SMOKE_DIR/calls.log" 2>&1
    }
    # ev NAME JS: evaluate JS in the shell. Eval answers (false, '') when
    # unsafe mode is off and (false, ERROR) when the JS throws: either is
    # logged as EVAL-FAILED, which fails the run; success as EVAL-OK NAME.
    ev() {
        local reply
        reply="$(gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell \
            -m org.gnome.Shell.Eval "$2" 2>&1)" || true
        if [[ "$reply" == "(true, "* ]]; then
            echo "EVAL-OK $1" >> "$SMOKE_DIR/calls.log"
        else
            echo "EVAL-FAILED $1: $reply" >> "$SMOKE_DIR/calls.log"
        fi
    }
    # js BODY...: BODY (the args joined) with our extension's instance `e`
    # and its toggle `t`, found in the Quick Settings grid (so the toggle
    # must really be in the menu).
    js() {
        printf '%s' "(() => { const e = Main.extensionManager.lookup('$uuid').stateObj; " \
            "const t = Main.panel.statusArea.quickSettings.menu._grid.get_children()" \
            ".find(c => c.constructor.\$gtype.name === 'CorrectiondCompanionToggle'); " \
            "if (!t) throw new Error('toggle not in Quick Settings'); $*; return 'ok'; })()"
    }
    # click LABEL: press a button of the open forget dialog, as a click does.
    click() {
        js "const d = e._forgetDialog; if (!d) throw new Error('no forget dialog'); " \
            "const b = d.buttonLayout.get_children().find(c => c.label === '$1'); " \
            "if (!b) throw new Error('no $1 button'); b.emit('clicked', 1)"
    }
    # Open the toggle's menu afresh (closing Quick Settings closes it too),
    # so open-state-changed refreshes it.
    open_menu='Main.panel.closeQuickSettings(); Main.panel.statusArea.quickSettings.menu.open(); t.menu.open()'
    # menu_is WANT: the menu, as [subtitle, app line, counts shown, separator
    # shown, switch shown, switch on, header shown, list shown, row texts],
    # equals WANT (a JS array literal); else the Eval throws what it saw.
    menu_is() {
        js "const got = JSON.stringify([t.subtitle, e._appItem.label.text, e._countsItem.visible, " \
            "e._learnSeparator.visible, e._learnSwitch.visible, e._learnSwitch.state, " \
            "e._recentHeader.visible, e._recentSection.actor.visible, " \
            "e._recentSection._getMenuItems().map(r => r.label?.text ?? '')]); " \
            "if (got !== JSON.stringify($1)) throw new Error(got)"
    }
    # key KEYVAL: press and release KEYVAL (a Clutter.KEY_* name) through a
    # virtual keyboard, as a real key press reaches the focused actor.
    key() {
        js "const C = imports.gi.Clutter; " \
            "const kb = global.stage.context.get_backend().get_default_seat()" \
            ".create_virtual_device(C.InputDeviceType.KEYBOARD_DEVICE); " \
            "const us = GLib.get_monotonic_time(); " \
            "kb.notify_keyval(us, C.KEY_$1, C.KeyState.PRESSED); " \
            "kb.notify_keyval(us + 20000, C.KEY_$1, C.KeyState.RELEASED)"
    }
    # The bodies of the notifications shown from now on, in globalThis.smokeNotes.
    watch_notes='globalThis.smokeNotes = []; '
    watch_notes+='const hook = s => s.connect("notification-added", (_s, n) => globalThis.smokeNotes.push(n.body)); '
    watch_notes+='Main.messageTray.getSources().forEach(hook); '
    watch_notes+='Main.messageTray.connect("source-added", (_t, s) => hook(s))'
    confirm_forget() {
        gdbus call --session -d org.gnome.Shell -o /org/nadelacruz/CorrectiondCompanion \
            -m org.nadelacruz.CorrectiondCompanion1.ConfirmForget > /dev/null 2>> "$SMOKE_DIR/calls.log" ||
            echo "COMPANION-FAILED" >> "$SMOKE_DIR/calls.log"
    }
    # companion METHOD [ARG...]: call the companion from gdbus (another
    # process, no window), logging `GDBUS METHOD => REPLY`.
    companion() {
        local reply
        reply="$(gdbus call --session -d org.gnome.Shell -o /org/nadelacruz/CorrectiondCompanion \
            -m "org.nadelacruz.CorrectiondCompanion1.$1" "${@:2}" 2>&1)" || true
        echo "GDBUS $1 => $reply" >> "$SMOKE_DIR/calls.log"
    }
    no_prompt="$(js "if (e._wordsDialog) throw new Error('a words prompt is open')")"
    # click_words LABEL: press a button of the open words prompt, as a click does.
    click_words() {
        js "const d = e._wordsDialog; if (!d) throw new Error('no words prompt'); " \
            "const b = d.buttonLayout.get_children().find(c => c.label === '$1'); " \
            "if (!b) throw new Error('no $1 button'); b.emit('clicked', 1)"
    }
    # The Words page stand-in (scripts/smoke_words_client.js), driven through
    # a FIFO on fd 3; its reply lines (`CLIENT ...`) go to calls.log.
    start_client() { # start_client [WM_CLASS]: the Settings wm_class by default
        rm -f "$SMOKE_DIR/client.fifo"; mkfifo "$SMOKE_DIR/client.fifo"
        WAYLAND_DISPLAY=wayland-0 GDK_BACKEND=wayland GSK_RENDERER=cairo GTK_A11Y=none \
            gjs -m "$repo/scripts/smoke_words_client.js" "${1:-$settings_wm_class}" < "$SMOKE_DIR/client.fifo" \
            >> "$SMOKE_DIR/calls.log" 2>> "$SMOKE_DIR/client.log" &
        client=$!
        exec 3> "$SMOKE_DIR/client.fifo"
        wait_line "CLIENT ready $client" 0
    }
    stop_client() { kill "$client" 2>/dev/null || true; wait "$client" 2>/dev/null || true; exec 3>&-; }
    send() { echo "$*" >&3; }
    # wait_line TEXT BEFORE: until calls.log has more than BEFORE lines with
    # TEXT (up to 10 s); else logs CLIENT-TIMEOUT, which fails the run.
    wait_line() {
        for _ in $(seq 40); do
            (( $(grep -cF -- "$1" "$SMOKE_DIR/calls.log" || true) > $2 )) && return 0
            sleep 0.25
        done
        echo "CLIENT-TIMEOUT $1" >> "$SMOKE_DIR/calls.log"
    }
    # wait_reply COMMAND: until the client has answered COMMAND (sent earlier).
    wait_reply() { wait_line "CLIENT $* => " 0; }
    # cmd COMMAND: send it and wait for its (new) reply line.
    cmd() {
        local before; before=$(grep -cF -- "CLIENT $* => " "$SMOKE_DIR/calls.log" || true)
        send "$@"
        wait_line "CLIENT $* => " "$before"
    }
    # focus_pid NAME PID: activate PID's window; the Evals fail unless it has focus.
    focus_pid() {
        ev "focus-$1" "$(js "const w = global.get_window_actors().map(a => a.meta_window)" \
            ".find(w => w.get_pid() === $2); if (!w) throw new Error('no window of $2'); " \
            "Main.activateWindow(w)")"
        sleep 0.5
        ev "focused-$1" "$(js "const p = global.display.focus_window?.get_pid(); " \
            "if (p !== $2) throw new Error('focus: ' + p)")"
    }
    focus_client() { focus_pid "$1" "$client"; }
    # evlog NAME JS: log Eval's raw reply to JS as `EVAL-OUT NAME REPLY`.
    evlog() {
        local reply
        reply="$(gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell \
            -m org.gnome.Shell.Eval "$2" 2>&1)" || true
        echo "EVAL-OUT $1 $reply" >> "$SMOKE_DIR/calls.log"
    }
    # GNOME's real preferences service, and the test-only driver in our
    # Settings window (scripts/smoke_prefs.js): prefs_call METHOD [ARG...]
    # logs `PREFS METHOD => REPLY`.
    start_prefs() {
        WAYLAND_DISPLAY=wayland-0 GDK_BACKEND=wayland GSK_RENDERER=cairo GTK_A11Y=none \
            /usr/bin/gjs -m /usr/share/gnome-shell/org.gnome.Shell.Extensions \
            >> "$SMOKE_DIR/prefs.log" 2>&1 &
        prefs=$!
        for _ in $(seq 40); do
            gdbus call --session -d org.freedesktop.DBus -o /org/freedesktop/DBus \
                -m org.freedesktop.DBus.NameHasOwner org.gnome.Shell.Extensions 2>/dev/null |
                grep -q true && return 0
            sleep 0.25
        done
        echo "PREFS-TIMEOUT service" >> "$SMOKE_DIR/calls.log"
    }
    prefs_call() {
        local reply
        reply="$(gdbus call --session -d org.gnome.Shell.Extensions -o /org/nadelacruz/CorrectiondSmokePrefs \
            -m "org.nadelacruz.CorrectiondSmokePrefs.$1" "${@:2}" 2>&1)" || true
        echo "PREFS $1 => $reply" >> "$SMOKE_DIR/calls.log"
    }
    wait_prefs() { # wait_prefs TEXT: until prefs.log has TEXT (up to 10 s)
        for _ in $(seq 40); do
            grep -qF -- "$1" "$SMOKE_DIR/prefs.log" 2>/dev/null && return 0
            sleep 0.25
        done
        echo "PREFS-TIMEOUT $1" >> "$SMOKE_DIR/calls.log"
    }
    # wait_gone PID: until PID has exited (up to 8 s); else logs PREFS-ALIVE.
    wait_gone() {
        for _ in $(seq 32); do
            kill -0 "$1" 2>/dev/null || return 0
            sleep 0.25
        done
        echo "PREFS-ALIVE $1" >> "$SMOKE_DIR/calls.log"
    }
    stop_engine() { kill "$fake"; wait "$fake" 2>/dev/null || true; }
    start_engine() { # start_engine [MODE]
        local owned; owned=$(grep -c '^owned ' "$SMOKE_DIR/calls.log" || true)
        start_fake 0 "${1:-normal}"
        for _ in $(seq 20); do
            (( $(grep -c '^owned ' "$SMOKE_DIR/calls.log" || true) > owned )) && break
            sleep 0.25
        done
    }
    toggle_osk() {
        gsettings --schemadir "$osk_schemas" \
            set org.gnome.shell.extensions.gjsosk.indicator opened true 2>/dev/null || true
    }
    # Wait for the extension's first state push (startup time varies), then
    # give gjs-osk time to build its keyboard.
    if [[ "${SMOKE_SCENARIO:-full}" == e1 ]]; then
        # The engine is not started yet: drop the delayed fake (its sleep too).
        kill "$fake" 2>/dev/null || true; pkill -f 'sleep 997' || true
        sleep 8
    else
        for _ in $(seq 60); do
            grep -qs SetEnabled "$SMOKE_DIR/calls.log" && break
            sleep 0.5
        done
        sleep 2
    fi
    case "${SMOKE_SCENARIO:-full}" in
    e1)
        # The Quick Settings texts (lib.js SETUP_TEXT.quick) as JS string literals.
        q_noengine='"Install the correctiond engine package"'
        q_nosource=$'"Add \'English (US, autocorrect)\' as an input source. If it isn\'t listed, run `ibus restart` first"'
        q_paused='"Paused: another input source is active (Super+Space to switch)"'
        q_starting='"Starting…"'
        q_failed=$'"The autocorrect engine isn\'t reachable. Run `ibus restart`; if it still fails, see `journalctl --user -b | grep correctiond`"'
        probe() { js "e._componentProbe = () => $1"; }
        # app_is NAME TEXT: after a fresh menu open, the app line is TEXT, the
        # counts and controls are hidden and the subtitle is Engine not running.
        app_is() {
            ev "$1-open" "$(js "$open_menu")"; sleep 1
            ev "$1" "$(js "if (t.subtitle !== 'Engine not running') throw new Error(t.subtitle); " \
                "if (e._countsItem.visible || e._learnSwitch.visible || e._recentHeader.visible) " \
                "throw new Error('counts or controls shown'); " \
                "const got = e._appItem.label.text; if (got !== $2) throw new Error(got)")"
        }
        ev e1-no-engine-probe "$(probe false)"
        app_is e1-no-engine "$q_noengine"
        ev e1-no-engine-close "$(js 'Main.panel.closeQuickSettings()')"
        ev e1-no-source-probe "$(probe true)"
        app_is e1-no-source "$q_nosource"
        ev e1-no-source-close "$(js 'Main.panel.closeQuickSettings()')"
        # An xkb source is current: paused.
        ev e1-sources "$(js "e._inputSettings.set_value('sources', " \
            "new GLib.Variant('a(ss)', [['xkb', 'us'], ['ibus', 'correctiond:us']])); " \
            "e._inputSettings.set_uint('current', 0)")"; sleep 1
        app_is e1-paused "$q_paused"
        ev e1-paused-close "$(js 'Main.panel.closeQuickSettings()')"
        # The correctiond source becomes current; the fake never starts.
        # Activating the ibus source would make the Shell's ibus-daemon start the
        # installed engine: report it as current instead (the shell's own getter).
        ev e1-current "$(js "const m = Main.panel.statusArea.keyboard._inputSourceManager; " \
            "Object.defineProperty(m, 'currentSource', {configurable: true, " \
            "get: () => ({type: 'ibus', id: 'correctiond:us'})})")"; sleep 1
        mark e1-starting
        ev e1-starting-open "$(js "$open_menu")"; sleep 1
        ev e1-starting "$(js "const got = e._appItem.label.text; " \
            "if (got !== $q_starting) throw new Error(got)")"
        ev e1-timer-one "$(js "if (!e._setupTimerId) throw new Error('no timer'); " \
            "globalThis.smokeTimer = e._setupTimerId")"
        ev e1-refresh-again "$(js "e._refreshMenu()")"; sleep 1
        ev e1-timer-same "$(js "if (e._setupTimerId !== globalThis.smokeTimer) throw new Error('a second timer')")"
        sleep 6
        mark e1-failed
        ev e1-failed "$(js "const got = e._appItem.label.text; " \
            "if (got !== $q_failed) throw new Error(got); " \
            "if (e._setupTimerId) throw new Error('timer left')")"
        ev e1-failed-close "$(js 'Main.panel.closeQuickSettings()')"
        ev e1-closed "$(js "if (e._setupTimerId) throw new Error('timer after close')")"
        # Close removes a pending timer: reopen on a fresh start.
        ev e1-live-probe "$(probe true)"
        mark e1-live
        start_engine; sleep 2
        ev e1-live-menu "$(js "$open_menu")"; sleep 1
        ev e1-live "$(menu_is "['Correcting', 'Correcting', true, true, true, true, true, true, ['kadto', 'sige']]")"
        ev e1-live-close "$(js 'Main.panel.closeQuickSettings()')"
        ev e1-uncurrent "$(js "delete Main.panel.statusArea.keyboard._inputSourceManager.currentSource")"
        ev e1-disable "$(js "e._indicator.toggle.menu.open(); e.disable(); " \
            "if (e._setupTimerId) throw new Error('timer after disable'); e.enable()")"
        mark end
        kill -INT "$shell"; wait "$shell" || true
        kill "$fake" "$sysbus" 2>/dev/null || true
        exit 0 ;;
    touch-off)
        mark open; toggle_osk; sleep 2
        ev touch-off-null "$(js "if (e._osk !== null) throw new Error('an OSK watcher exists')")"
        mark close; toggle_osk; sleep 2
        mark end
        kill -INT "$shell"; wait "$shell" || true
        kill "$fake" "$sysbus" 2>/dev/null || true
        exit 0 ;;
    live-config)
        cfgdir="$HOME/.config/correctiond"
        # cfg TEXT: rewrite config.toml by atomic rename, as the Settings window does.
        cfg() { printf '%s\n' "$1" > "$cfgdir/config.toml.tmp"; mv "$cfgdir/config.toml.tmp" "$cfgdir/config.toml"; }
        # src N NAME: the current input source index is N.
        src() {
            ev "$2" "$(js "const i = Main.panel.statusArea.keyboard._inputSourceManager.currentSource.index; " \
                "if (i !== $1) throw new Error('source ' + i)")"
        }
        osk_on="if (e._osk === null) throw new Error('no watcher')"
        osk_off="if (e._osk !== null) throw new Error('a watcher is left')"
        ev lc-start "$(js "$osk_on")"
        src 0 lc-src-start
        mark toggle-open; toggle_osk; sleep 2
        src 1 lc-src-switched
        mark toggle-off; cfg $'[modules]\ntouch = false'; sleep 1
        ev lc-off-null "$(js "$osk_off")"
        src 0 lc-src-restored
        mark toggle-on; cfg $'[modules]\ntouch = true'; sleep 1
        ev lc-on-watcher "$(js "$osk_on")"
        mark toggle-reopen; toggle_osk; sleep 1; toggle_osk; sleep 2
        mark unrelated; src 1 lc-unrelated-src-before
        cfg $'allowlist = ["firefox"]\n[modules]\ntouch = true'; sleep 1
        src 1 lc-unrelated-src-after
        ev lc-unrelated-watcher "$(js "$osk_on")"
        mark double-flip
        cfg $'[modules]\ntouch = true'; sleep 0.05; cfg $'[modules]\ntouch = false'; sleep 1
        ev lc-double-null "$(js "$osk_off")"
        ev lc-double-clean "$(js "if (e._configDebounceId !== 0) throw new Error('timer left')")"
        src 0 lc-double-src
        mark deleted; rm -f "$cfgdir/config.toml"; sleep 1
        ev lc-deleted-watcher "$(js "$osk_on")"
        mark invalid; cfg '[[['; sleep 1
        ev lc-invalid-watcher "$(js "$osk_on")"
        ev lc-keep "$(js "globalThis.smokeExt = e")"
        mark disable; ext DisableExtension; sleep 1
        # js needs the toggle, which is gone: read the kept instance directly.
        ev lc-disabled "(() => { const e = globalThis.smokeExt; if (e._osk !== null || e._configMonitor !== null || e._configDebounceId !== 0 || e._gjsOskStateId !== 0 || e._touch !== null) throw new Error('left'); return 'ok'; })()"
        mark end
        kill -INT "$shell"; wait "$shell" || true
        kill "$fake" "$sysbus" 2>/dev/null || true
        exit 0 ;;
    no-config-dir)
        # A fresh install: no ~/.config/correctiond. Touch derives on from the
        # installed GJS OSK; the first Settings save creates the folder, then
        # the file. The switch written off must apply live.
        ev nocfg-start "$(js "if (e._osk === null || e._configMonitor === null) throw new Error('no watcher or no monitor')")"
        mark create; mkdir -p "$HOME/.config/correctiond"; sleep 1
        printf '[modules]\ntouch = false\n' > "$HOME/.config/correctiond/config.toml"; sleep 1.5
        evlog nocfg-osk "(() => String(Main.extensionManager.lookup('$uuid').stateObj._osk === null))()"
        ev nocfg-keep "$(js "globalThis.smokeExt = e")"
        mark disable; ext DisableExtension; sleep 1
        evlog nocfg-disabled "(() => { const e = globalThis.smokeExt; if (e._osk !== null || e._configMonitor !== null || e._configDebounceId !== 0 || e._gjsOskStateId !== 0 || e._touch !== null) throw new Error('left'); return 'ok'; })()"
        mark end
        kill -INT "$shell"; wait "$shell" || true
        kill "$fake" "$sysbus" 2>/dev/null || true
        exit 0 ;;
    live-gjs-osk)
        ev lg-start-null "$(js "if (e._osk !== null) throw new Error('a watcher exists')")"
        mark install
        mkdir -p "$HOME/.local/share/gnome-shell/extensions"
        cp -r "/usr/share/gnome-shell/extensions/$osk_uuid" "$HOME/.local/share/gnome-shell/extensions/"
        ev lg-load "$(js "const m = Main.extensionManager; " \
            "if (!m.lookup('$osk_uuid')) { m.loadExtension(m.createExtensionObject('$osk_uuid', " \
            "Gio.File.new_for_path('$HOME/.local/share/gnome-shell/extensions/$osk_uuid'), 1)); } " \
            "m.enableExtension('$osk_uuid')")"
        sleep 3
        ev lg-watcher "$(js "if (e._osk === null) throw new Error('no watcher')")"
        mark open; toggle_osk; sleep 2
        mark close; toggle_osk; sleep 2
        mark end
        kill -INT "$shell"; wait "$shell" || true
        kill "$fake" "$sysbus" 2>/dev/null || true
        exit 0 ;;
    v010)
        # An engine with exactly the method set of the v0.1.0 tag (the
        # compatibility policy in docs/ARCHITECTURE.md): the menu works as usual.
        mark toggle-off; ev toggle-off "$(js 't.checked = false')"; sleep 1
        mark toggle-on; ev toggle-on "$(js 't.checked = true')"; sleep 1
        mark menu-open; ev menu-open "$(js "$open_menu")"; sleep 1
        ev v010-menu "$(menu_is "['Correcting', 'Correcting', true, true, true, true, true, true, ['kadto', 'sige']]")"
        ev v010-close "$(js 'Main.panel.closeQuickSettings()')"
        mark end
        kill -INT "$shell"; wait "$shell" || true
        kill "$fake" "$sysbus" 2>/dev/null || true
        exit 0 ;;
    late-osk)
        sleep 1 # 3 s after the companion started, give or take
        ev late-osk-watcher "$(js "if (e._osk === null) throw new Error('no watcher: touch is off')")"
        mark enable-osk
        ev late-osk-enable "$(js "Main.extensionManager.enableExtension('$osk_uuid')")"; sleep 3
        mark open; toggle_osk; sleep 2
        mark close; toggle_osk; sleep 2
        mark end
        kill -INT "$shell"; wait "$shell" || true
        kill "$fake" "$sysbus" 2>/dev/null || true
        exit 0 ;;
    esac
    if [[ "${SMOKE_WITH_OSK:-0}" == 0 ]]; then
        ev osk-null "$(js "if (e._osk !== null) throw new Error('an OSK watcher exists')")"
    else
        ev osk-watcher "$(js "if (e._osk === null) throw new Error('no OSK watcher')")"
    fi
    mark open; toggle_osk; sleep 2
    mark close; toggle_osk; sleep 2
    mark disable; ext DisableExtension; sleep 1
    mark enable; ext EnableExtension; sleep 2
    mark toggle-off; ev toggle-off "$(js 't.checked = false')"; sleep 1
    mark toggle-on; ev toggle-on "$(js 't.checked = true')"; sleep 1
    mark menu-open; ev menu-open "$(js "$open_menu")"; sleep 1
    # The fake's replies, through statusText (no window has focus here).
    ev menu-counts "$(js "if (e._countsItem.label.text !== '0 corrections · 0 undone · 0 words learned') " \
        "throw new Error(e._countsItem.label.text)")"
    ev state-start "$(menu_is "['Correcting', 'Correcting', true, true, true, true, true, true, ['kadto', 'sige']]")"
    # setToggleState emits `toggled`, as a click does.
    mark learn-off; ev learn-off "$(js 'e._learnSwitch.setToggleState(false)')"; sleep 1
    mark forget-word
    ev forget-word "$(js "const row = e._recentSection._getMenuItems()[0]; " \
        "if (!row?.forgetButton) throw new Error('no recent word'); row.forgetButton.emit('clicked', 1)")"
    sleep 1
    ev menu-close "$(js 'Main.panel.closeQuickSettings()')"; sleep 1
    mark confirm-cancel; confirm_forget; sleep 1; ev cancel "$(click Cancel)"; sleep 1
    # Return must cancel: Cancel is the default, focused button; Forget is not.
    mark confirm-enter; confirm_forget; sleep 1
    ev enter-focus "$(js "const d = e._forgetDialog; if (!d) throw new Error('no forget dialog'); " \
        "const [c, f] = ['Cancel', 'Forget'].map(l => d.buttonLayout.get_children().find(b => b.label === l)); " \
        "if (global.stage.key_focus !== c) throw new Error('key focus: ' + global.stage.key_focus?.label); " \
        "if (!c.has_style_pseudo_class('default') || f.has_style_pseudo_class('default')) " \
        "throw new Error('Forget is the default button')")"
    ev enter "$(key Return)"; sleep 1
    ev enter-closed "$(js "if (e._forgetDialog) throw new Error('Return left the dialog open')")"
    ev watch-notes "$(js "$watch_notes")"
    mark confirm-forget; confirm_forget; sleep 1; ev forget "$(click Forget)"; sleep 4
    ev purge-notice "$(js "const n = JSON.stringify(globalThis.smokeNotes); " \
        "if (n !== JSON.stringify(['Learned words forgotten.'])) throw new Error(n)")"
    # Screen lock: the engine keeps running, so the pause is read back.
    mark relock-off; ext DisableExtension; sleep 1
    mark relock-on; ext EnableExtension; sleep 2
    ev ext-state "$(js "const x = Main.extensionManager.lookup('$uuid'); if (x.error) throw new Error(x.error)")"
    ev relock-menu "$(js "$open_menu")"; sleep 1
    ev state-relock "$(menu_is "['Learning paused', 'Correcting', true, true, true, false, true, true, ['kadto', 'sige']]")"
    ev relock-close "$(js 'Main.panel.closeQuickSettings()')"
    mark restart-engine; stop_engine; sleep 1
    # Down with the component missing: the toggle says Engine not running, the
    # app line is the setup text (spec 8.4).
    ev down-probe "$(js "e._componentProbe = () => false")"
    ev down-menu "$(js "$open_menu")"; sleep 1
    down="['Engine not running', 'Install the correctiond engine package', false, false, false, false, false, false, "
    ev state-down "$(menu_is "$down['(none yet this session)']]")"
    ev down-close "$(js 'Main.panel.closeQuickSettings()')"
    ev down-unprobe "$(js "e._componentProbe = null")"
    start_engine; sleep 2
    ev restarted-menu "$(js "$open_menu")"; sleep 1
    ev state-restarted "$(menu_is "['Correcting', 'Correcting', true, true, true, true, true, true, ['kadto', 'sige']]")"
    ev restarted-close "$(js 'Main.panel.closeQuickSettings()')"
    # A locked keyring: the row says so, then shows the word again.
    mark forget-fails; stop_engine; start_engine forget-fails; sleep 2
    ev locked-menu "$(js "$open_menu")"; sleep 1
    ev locked-forget "$(js "const row = e._recentSection._getMenuItems()[0]; " \
        "if (!row?.forgetButton) throw new Error('no recent word'); row.forgetButton.emit('clicked', 1)")"
    sleep 1
    ev locked-text "$(js "const l = e._recentSection._getMenuItems()[0].label.text; " \
        "if (l !== 'Unlock your keyring first') throw new Error(l)")"
    sleep 3
    ev locked-reset "$(js "const l = e._recentSection._getMenuItems()[0].label.text; " \
        "if (l !== 'kadto') throw new Error(l)")"
    ev locked-close "$(js 'Main.panel.closeQuickSettings()')"
    # An engine from before GetStatus: up, with counts, without the switch and list.
    mark old-engine; stop_engine; start_engine old; sleep 2
    ev old-menu "$(js "$open_menu")"; sleep 1
    old="['Autocorrect', 'No text field yet', true, false, false, true, false, false, "
    ev state-old "$(menu_is "$old['(none yet this session)']]")"
    ev old-counts "$(js "if (e._countsItem.label.text !== '0 corrections · 0 undone · 0 words learned') " \
        "throw new Error(e._countsItem.label.text)")"
    ev old-close "$(js 'Main.panel.closeQuickSettings()')"
    # --- learned words: the companion's RequestWordAccess, ListWords, ForgetWords ---
    mark words; stop_engine; start_engine; sleep 1
    prompt_text='Show your learned words in Settings? Settings can list and forget your learned words until you close it.'
    # Another process (gdbus), with no window: false at once, no prompt;
    # and never allowed, so the pass-through says AccessDenied.
    companion RequestWordAccess; sleep 0.5
    ev words-no-prompt "$no_prompt"
    companion ListWords
    companion ForgetWords "['kadto']"
    mark words-client; start_client
    cmd open a; focus_client a
    ev words-client-wmclass "$(js "const w = global.display.focus_window; " \
        "const got = JSON.stringify([w.get_wm_class(), w.get_gtk_application_id()]); " \
        "if (got !== JSON.stringify(['$settings_wm_class', null])) throw new Error(got)")"
    mark words-allow; send request a 1; sleep 1
    ev words-prompt "$(js "const d = e._wordsDialog; if (!d) throw new Error('no words prompt'); " \
        "const c = d.contentLayout.get_children()[0]; const got = c.title + ' ' + c.description; " \
        "if (got !== '$prompt_text') throw new Error(got)")"
    ev words-allow "$(click_words Allow)"; wait_reply "request a 1"
    cmd list a
    cmd probe a
    cmd forget a kadto
    # Review Focus 4: the engine goes away and comes back while allowed.
    mark words-down; stop_engine
    cmd list a
    mark words-up; start_engine; sleep 1
    cmd list a
    mark words-locked; stop_engine; start_engine forget-fails; sleep 1
    cmd forget a kadto
    # At most 2 allowed names, oldest replaced.
    mark words-two; stop_engine; start_engine; sleep 1
    cmd open b; focus_client b; send request b 2; sleep 1
    ev words-allow-b "$(click_words Allow)"; wait_reply "request b 2"
    cmd open c; focus_client c; send request c 3; sleep 1
    ev words-allow-c "$(click_words Allow)"; wait_reply "request c 3"
    ev words-two "$(js "if (e._wordClients.size !== 2) throw new Error(String(e._wordClients.size))")"
    cmd list a
    cmd list c
    # Closing a connection drops its name; the same process's next one is not allowed.
    mark words-close; cmd close b; cmd close c; sleep 1
    ev words-dropped "$(js "if (e._wordClients.size !== 0) throw new Error(String(e._wordClients.size))")"
    cmd open d; cmd list d
    mark words-cancel; focus_client d1; send request d 4; sleep 1
    ev words-cancel "$(click_words Cancel)"; wait_reply "request d 4"
    # Return must cancel: Cancel is the default, focused button; Allow is not.
    mark words-enter; focus_client d2; send request d 5; sleep 1
    ev words-enter-focus "$(js "const d = e._wordsDialog; if (!d) throw new Error('no words prompt'); " \
        "const [c, a] = ['Cancel', 'Allow'].map(l => d.buttonLayout.get_children().find(b => b.label === l)); " \
        "if (!a) throw new Error('no Allow button'); " \
        "if (global.stage.key_focus !== c) throw new Error('key focus: ' + global.stage.key_focus?.label); " \
        "if (!c.has_style_pseudo_class('default') || a.has_style_pseudo_class('default')) " \
        "throw new Error('Allow is the default button')")"
    ev words-enter "$(key Return)"; wait_reply "request d 5"
    ev words-enter-closed "$no_prompt"
    # A second request while the prompt is open: false at once; the first goes on.
    mark words-busy; focus_client d3; send request d 6; sleep 1
    cmd request d 7
    mark words-busy-allow; ev words-busy-allow "$(click_words Allow)"; wait_reply "request d 6"
    cmd list d
    # Disable during the prompt: false; and the allowed set is cleared.
    mark words-disable; focus_client d4; send request d 8; sleep 1
    ev words-disable-prompt "$(js "if (!e._wordsDialog) throw new Error('no words prompt')")"
    ext DisableExtension; wait_reply "request d 8"
    # With nothing exported at the companion's path, GDBus answers UnknownMethod
    # too (as an engine without ListWords does): the page's probe tells them apart.
    mark words-off; cmd list d; cmd probe d
    # (Not js: the toggle is gone while disabled.)
    off_js="(() => { const e = Main.extensionManager.lookup('$uuid').stateObj; "
    off_js+="if (e._wordClients !== null || e._wordsRequest !== null || e._wordsDialog !== null) "
    off_js+="throw new Error('word access state kept after disable'); return 'ok'; })()"
    ev words-off-cleared "$off_js"
    mark words-on; ext EnableExtension; sleep 2
    cmd list d
    # The prompt lives 115 s at most (the caller waits 120 s), and closes,
    # with no grant, as soon as the caller's connection goes.
    mark words-vanish; cmd open e; focus_client e; send request e 9; sleep 1
    ev words-timer "$(js "const r = e._wordsRequest; if (!r || !e._wordsDialog) throw new Error('no words prompt'); " \
        "const s = GLib.MainContext.default().find_source_by_id(r.timeoutId); " \
        "if (!s) throw new Error('no prompt timer'); " \
        "const left = (s.get_ready_time() - GLib.get_monotonic_time()) / 1000; " \
        "if (left < 112000 || left > 115000) throw new Error('timer: ' + left); " \
        "if (!r.watchId) throw new Error('the caller is not watched'); " \
        "globalThis.smokeTimer = r.timeoutId")"
    cmd close e; sleep 1
    ev words-vanish-closed "$(js "if (e._wordsDialog || e._wordsRequest) throw new Error('the prompt is still open'); " \
        "if (GLib.MainContext.default().find_source_by_id(globalThis.smokeTimer)) throw new Error('timer left'); " \
        "if (e._wordClients.size !== 0) throw new Error(String(e._wordClients.size))")"
    # The right process without focus, and another process with the window focused.
    mark words-unfocused
    ev words-unfocus "$(js 'global.display.unset_input_focus(global.get_current_time())')"; sleep 0.5
    cmd request d 10
    ev words-unfocused-none "$no_prompt"
    mark words-other-pid; focus_client d5
    companion RequestWordAccess; sleep 0.5
    ev words-other-pid-none "$no_prompt"
    # A window of the right process with another wm_class.
    mark words-plain; stop_client; start_client smoke-words-client
    cmd open p; focus_client p
    ev words-plain-wmclass "$(js "const c = global.display.focus_window?.get_wm_class(); " \
        "if (c !== 'smoke-words-client') throw new Error(c)")"
    cmd request p 11
    ev words-plain-none "$no_prompt"
    stop_client
    # --- the real Settings window, in GNOME's real preferences service ---
    mark prefs; start_prefs
    # Through the menu's own "Autocorrect Settings…" item (_openSettings).
    ev prefs-menu-item "$(js "const i = t.menu._getMenuItems()" \
        ".find(m => m.label?.text === 'Autocorrect Settings…'); " \
        "if (!i) throw new Error('no Settings item'); i.activate(null)")"
    wait_prefs 'SMOKE-PREFS ready'; sleep 1
    focus_pid prefs "$prefs"
    evlog prefs-wm-class 'global.display.focus_window?.get_wm_class() ?? null'
    evlog prefs-app-id 'global.display.focus_window?.get_gtk_application_id() ?? null'
    mark prefs-show; prefs_call ShowWords; sleep 1
    ev prefs-prompt "$(js "if (!e._wordsDialog) throw new Error('no words prompt')")"
    ev prefs-allow "$(click_words Allow)"; sleep 1.5
    prefs_call State
    ev prefs-allowed "$(js "if (e._wordClients.size !== 1) throw new Error(String(e._wordClients.size))")"
    # A delete waits 5 s for Undo; closing first must still send it, once,
    # without holding the close (GNOME's window turns destroy() into an error page).
    mark prefs-forget; prefs_call Forget "'kadto'"; sleep 0.5
    prefs_call State
    mark prefs-close; prefs_call Close
    wait_gone "$prefs"
    ev prefs-dropped "$(js "if (e._wordClients.size !== 0) throw new Error(String(e._wordClients.size))")"
    kill "$prefs" 2>/dev/null || true
    # Disable, enable and disable with no wait: no watcher, no timer left.
    mark rapid; ext DisableExtension; ext EnableExtension; ext DisableExtension; sleep 1
    rapid_js="(() => { const e = Main.extensionManager.lookup('$uuid').stateObj; "
    rapid_js+="if (e._osk !== null) throw new Error('an OSK watcher is left'); return 'ok'; })()"
    ev rapid-osk-null "$rapid_js"; sleep 1
    mark end
    kill -INT "$shell"; wait "$shell" || true
    kill "$fake" "$sysbus" "${prefs:-}" 2>/dev/null || true
    exit 0
fi

# A copy of /usr/share as symlinks without gjs-osk: XDG_DATA_DIRS for the
# scenarios where gjs-osk must not count as installed (touch derives off).
hide_osk_share() { # hide_osk_share DIR
    local sh=$1 d b
    mkdir -p "$sh/gnome-shell/extensions"
    for d in /usr/share/*; do
        b=${d##*/}; [[ "$b" == gnome-shell ]] || ln -s "$d" "$sh/$b"
    done
    for d in /usr/share/gnome-shell/*; do
        b=${d##*/}; [[ "$b" == extensions ]] || ln -s "$d" "$sh/gnome-shell/$b"
    done
    for d in /usr/share/gnome-shell/extensions/*; do
        b=${d##*/}; [[ "$b" == "$osk_uuid" ]] || ln -s "$d" "$sh/gnome-shell/extensions/$b"
    done
}

# run_one NAME WITH_OSK FAKE_DELAY [CONFIG_TOML [SCENARIO]]: leaves calls.log and
# shell.log in $out/NAME. WITH_OSK: 1 = gjs-osk enabled, 0 = not installed,
# 2 = installed, enabled by the scenario later.
run_one() {
    local name=$1 with_osk=$2 delay=$3 config=${4:-} scenario=${5:-full}
    local tmp; tmp="$(mktemp -d /tmp/ccx.XXXXXX)" # short: Wayland socket paths max 108 bytes
    local home="$tmp/home" dir="$out/$name"
    mkdir -p "$home/.config/glib-2.0/settings" "$home/.local/share/gnome-shell/extensions/$uuid" \
        "$home/.cache" "$home/.local/state" "$tmp/run" "$dir"
    chmod 700 "$tmp/run"
    local data_dirs=/usr/local/share:/usr/share
    if [[ "$with_osk" == 0 ]]; then
        hide_osk_share "$tmp/share"; data_dirs="$tmp/share:/usr/local/share"
    fi
    if [[ -n "$config" ]]; then
        mkdir -p "$home/.config/correctiond"; printf '%s\n' "$config" > "$home/.config/correctiond/config.toml"
    fi
    local extdir="$home/.local/share/gnome-shell/extensions/$uuid"
    install -m 0644 "$repo/extension/metadata.json" "$repo/extension/extension.js" \
        "$repo/extension/lib.js" "$repo/extension/config.js" "$repo/extension/osk.js" \
        "$repo/extension/prefsWindow.js" \
        "$repo/extension/wordsPage.js" "$repo/LICENSE-MIT" "$repo/LICENSE-APACHE" "$extdir/"
    # The Settings window's driver: temp HOME only, around the real prefs.js.
    install -m 0644 "$repo/extension/prefs.js" "$extdir/prefs-real.js"
    install -m 0644 "$repo/scripts/smoke_prefs.js" "$extdir/prefs.js"
    # The Eval helper: temp HOME only, never shipped.
    mkdir -p "$home/.local/share/gnome-shell/extensions/$unsafe_uuid"
    install -m 0644 "$repo/scripts/$unsafe_uuid/metadata.json" "$repo/scripts/$unsafe_uuid/extension.js" \
        "$home/.local/share/gnome-shell/extensions/$unsafe_uuid/"
    local exts="'$unsafe_uuid', '$uuid'"
    [[ "$with_osk" == 1 ]] && exts="$exts, '$osk_uuid'"
    printf '[org/gnome/shell]\nenabled-extensions=[%s]\ndisable-user-extensions=false\n' "$exts" \
        > "$home/.config/glib-2.0/settings/keyfile"
    if [[ "$scenario" == live-config ]]; then # two sources, so a switch and its restore can be seen
        printf "\n[org/gnome/desktop/input-sources]\nsources=[('xkb', 'us'), ('ibus', 'correctiond:us')]\n" \
            >> "$home/.config/glib-2.0/settings/keyfile"
    fi
    env -i PATH="$PATH" LANG=C.UTF-8 HOME="$home" \
        XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
        XDG_CACHE_HOME="$home/.cache" XDG_STATE_HOME="$home/.local/state" \
        XDG_DATA_DIRS="$data_dirs" SMOKE_SCENARIO="$scenario" SMOKE_WITH_OSK="$with_osk" \
        XDG_RUNTIME_DIR="$tmp/run" GSETTINGS_BACKEND=keyfile \
        GIO_USE_VFS=local GVFS_DISABLE_FUSE=1 \
        SMOKE_DIR="$dir" FAKE_DELAY="$delay" \
        dbus-run-session -- bash "${BASH_SOURCE[0]}" --inner >/dev/null 2>&1 || true
    # The shell's portal may have mounted a document FUSE fs in the runtime dir.
    fusermount3 -u -q "$tmp/run/doc" 2>/dev/null || true
    rm -rf "$tmp"
}

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
fail=0
check() { # check NAME DESCRIPTION CONDITION...
    local name=$1 what=$2; shift 2
    if "$@"; then echo "ok   $name: $what"; else echo "FAIL $name: $what"; fail=1; fi
}
# between NAME FROM TO PATTERN: PATTERN occurs in calls.log after mark FROM, before mark TO
between() {
    sed -n "/^--- $2\$/,/^--- $3\$/p" "$out/$1/calls.log" | grep -qF -- "$4"
}
before() { sed -n "1,/^--- $2\$/p" "$out/$1/calls.log" | grep -qF -- "$3"; }
# count NAME FROM TO PATTERN: how many lines between the marks match
count() { sed -n "/^--- $2\$/,/^--- $3\$/p" "$out/$1/calls.log" | grep -cF -- "$4" || true; }
# eval_ok NAME EVAL: the Eval named EVAL ran and succeeded
eval_ok() { grep -qxF "EVAL-OK $2" "$out/$1/calls.log"; }
# menu_checks NAME: the menu, companion and restart contract (R6)
menu_checks() {
    local n=$1
    check "$n" "every Eval and companion call succeeded" \
        bash -c "! grep -qE '^(EVAL-FAILED|COMPANION-FAILED)' '$out/$n/calls.log'"
    check "$n" "toggle off sends SetEnabled(false)" between "$n" toggle-off toggle-on "SetEnabled (False,)"
    check "$n" "toggle on sends SetEnabled(true)" between "$n" toggle-on menu-open "SetEnabled (True,)"
    check "$n" "menu open calls GetStatus" between "$n" menu-open learn-off "GetStatus"
    check "$n" "menu open calls GetStats" between "$n" menu-open learn-off "GetStats"
    check "$n" "menu open calls RecentWords" between "$n" menu-open learn-off "RecentWords"
    check "$n" "learning switch off sends SetLearning(false)" \
        between "$n" learn-off forget-word "SetLearning (False,)"
    check "$n" "the first x forgets the newest word" between "$n" forget-word confirm-cancel "ForgetWord ('kadto',)"
    check "$n" "a forget refreshes the menu" between "$n" forget-word confirm-cancel "RecentWords"
    check "$n" "Cancel does not purge" [ "$(count "$n" confirm-cancel confirm-enter Purge)" = 0 ]
    check "$n" "Cancel is the dialog's default, focused button" eval_ok "$n" enter-focus
    check "$n" "Return cancels the dialog" eval_ok "$n" enter-closed
    check "$n" "Return does not purge" [ "$(count "$n" confirm-enter confirm-forget Purge)" = 0 ]
    check "$n" "Forget purges exactly once" [ "$(count "$n" confirm-forget relock-off Purge)" = 1 ]
    check "$n" "a Purge slower than 2 s still reports success" eval_ok "$n" purge-notice
    check "$n" "menu shows the engine's state, switch on" eval_ok "$n" state-start
    check "$n" "re-enable sends no SetLearning" [ "$(count "$n" relock-off restart-engine SetLearning)" = 0 ]
    check "$n" "re-enable reads the pause back (switch off)" eval_ok "$n" state-relock
    check "$n" "engine down: switch and list hidden, Engine not running" eval_ok "$n" state-down
    check "$n" "engine restart re-pushes SetEnabled(true)" between "$n" restart-engine forget-fails "SetEnabled (True,)"
    check "$n" "engine restart sends no SetLearning (no pause push, no echo)" \
        [ "$(count "$n" restart-engine end SetLearning)" = 0 ]
    check "$n" "after a restart the switch follows the engine (on)" eval_ok "$n" state-restarted
    check "$n" "locked keyring: the x calls ForgetWord" between "$n" forget-fails old-engine "ForgetWord ('kadto',)"
    check "$n" "locked keyring: the row says Unlock your keyring first" eval_ok "$n" locked-text
    check "$n" "locked keyring: the row shows the word again after about 3 s" eval_ok "$n" locked-reset
    check "$n" "old engine: counts and No text field yet, switch and list hidden" eval_ok "$n" state-old
    check "$n" "old engine: the counts line" eval_ok "$n" old-counts
    check "$n" "re-enable exports the companion again" \
        bash -c "! grep -qi 'already exported' '$out/$n/shell.log' '$out/$n/calls.log'"
}
# words_checks NAME: learned words access through the companion
words_checks() {
    local n=$1 rows='kadto:41:seen:true sige:12:seen:true the:500:seen:false'
    local denied='error org.freedesktop.DBus.Error.AccessDenied: '
    check "$n" "words: the client started and every client call was answered" \
        bash -c "grep -q '^CLIENT ready ' '$out/$n/calls.log' && ! grep -q '^CLIENT-TIMEOUT' '$out/$n/calls.log'"
    check "$n" "words: another process with no window is refused at once" \
        between "$n" words words-client "GDBUS RequestWordAccess => (false,)"
    check "$n" "words: ... with no prompt" eval_ok "$n" words-no-prompt
    check "$n" "words: ListWords from a caller never allowed is AccessDenied" \
        between "$n" words words-client "GDBUS ListWords => Error: GDBus.Error:org.freedesktop.DBus.Error.AccessDenied"
    check "$n" "words: ForgetWords from a caller never allowed is AccessDenied" \
        between "$n" words words-client "GDBUS ForgetWords => Error: GDBus.Error:org.freedesktop.DBus.Error.AccessDenied"
    check "$n" "words: a caller never allowed reaches no engine method" \
        [ "$(count "$n" words words-client "ListWords (")$(count "$n" words words-client "ForgetWords (")" = 00 ]
    check "$n" "words: the prompt's text" eval_ok "$n" words-prompt
    check "$n" "words: Allow gives true" between "$n" words-allow words-down "CLIENT request a 1 => true"
    check "$n" "words: ListWords returns the 3 rows" between "$n" words-allow words-down "CLIENT list a => $rows"
    check "$n" "words: ForgetWords(['kadto']) gives 1" between "$n" words-allow words-down "CLIENT forget a kadto => 1"
    check "$n" "words: the page's probe finds the companion" between "$n" words-allow words-down "CLIENT probe a => true"
    check "$n" "words: extension off: GDBus gives UnknownMethod for the companion's path" \
        between "$n" words-off words-on "CLIENT list d => error org.freedesktop.DBus.Error.UnknownMethod"
    check "$n" "words: ... and the page's probe finds no companion" between "$n" words-off words-on "CLIENT probe d => false"
    check "$n" "words: ... and reaches the engine" between "$n" words-allow words-down "ForgetWords (['kadto'],)"
    check "$n" "words: engine down: ListWords fails with an error" \
        between "$n" words-down words-up "CLIENT list a => error "
    check "$n" "words: engine back: the allowed connection lists again" \
        between "$n" words-up words-locked "CLIENT list a => $rows"
    check "$n" "words: the engine's error passes through by name and message" \
        between "$n" words-locked words-two \
        "CLIENT forget a kadto => error org.nadelacruz.Correctiond1.Error.ForgetFailed: unlock your keyring first"
    check "$n" "words: two more allowed (b, c)" between "$n" words-two words-close "CLIENT request c 3 => true"
    check "$n" "words: at most 2 allowed names" eval_ok "$n" words-two
    check "$n" "words: the oldest allowed name is replaced" between "$n" words-two words-close "CLIENT list a => $denied"
    check "$n" "words: the newest allowed name lists" between "$n" words-two words-close "CLIENT list c => $rows"
    check "$n" "words: closed connections leave the allowed set" eval_ok "$n" words-dropped
    check "$n" "words: the same process's next connection is AccessDenied" \
        between "$n" words-close words-cancel "CLIENT list d => $denied"
    check "$n" "words: Cancel gives false" between "$n" words-cancel words-enter "CLIENT request d 4 => false"
    check "$n" "words: Cancel is the prompt's default, focused button" eval_ok "$n" words-enter-focus
    check "$n" "words: Return gives false" between "$n" words-enter words-busy "CLIENT request d 5 => false"
    check "$n" "words: Return closes the prompt" eval_ok "$n" words-enter-closed
    check "$n" "words: a request while the prompt is open gives false at once" \
        between "$n" words-busy words-busy-allow "CLIENT request d 7 => false"
    check "$n" "words: ... and the open prompt still answers" \
        between "$n" words-busy-allow words-disable "CLIENT request d 6 => true"
    check "$n" "words: ... which allows" between "$n" words-busy-allow words-disable "CLIENT list d => $rows"
    check "$n" "words: disable during the prompt gives false" \
        between "$n" words-disable words-vanish "CLIENT request d 8 => false"
    check "$n" "words: disable clears the allowed set" between "$n" words-disable words-vanish "CLIENT list d => $denied"
    check "$n" "words: the prompt has a 115 s timer and watches the caller" eval_ok "$n" words-timer
    check "$n" "words: the caller's connection closing closes the prompt, with no grant or timer left" \
        eval_ok "$n" words-vanish-closed
    check "$n" "words: disable drops the word access state (null)" eval_ok "$n" words-off-cleared
    check "$n" "words: ... and says so in the log" grep -qF 'word access refused: the caller is gone' "$out/$n/shell.log"
    check "$n" "words: the right process without focus gives false" \
        between "$n" words-unfocused words-other-pid "CLIENT request d 10 => false"
    check "$n" "words: ... with no prompt" eval_ok "$n" words-unfocused-none
    check "$n" "words: another process while the window has focus gives false" \
        between "$n" words-other-pid words-plain "GDBUS RequestWordAccess => (false,)"
    check "$n" "words: ... with no prompt" eval_ok "$n" words-other-pid-none
    check "$n" "words: the stand-in's window has the Settings wm_class and no GTK app id" \
        eval_ok "$n" words-client-wmclass
    check "$n" "words: a window with another wm_class gives false" \
        between "$n" words-plain prefs "CLIENT request p 11 => false"
    check "$n" "words: ... with no prompt" eval_ok "$n" words-plain-none
    check "$n" "words: refusals log fixed texts" \
        grep -qF 'word access refused: not the focused Settings window' "$out/$n/shell.log"
    check "$n" "words: a busy refusal logs a fixed text" \
        grep -qF 'word access refused: a request is already open' "$out/$n/shell.log"
    check "$n" "words: no extension log line contains a word" \
        bash -c "! grep 'correctiond-companion' '$out/$n/shell.log' | grep -qE 'kadto|sige'"
}
# prefs_checks NAME: our Settings window in GNOME's real preferences service
prefs_checks() {
    local n=$1 log="$out/$1/prefs.log"
    echo "measured $n: $(grep '^EVAL-OUT prefs-' "$out/$n/calls.log" | tr '\n' ' ')"
    check "$n" "prefs: every service wait succeeded" bash -c "! grep -qE '^PREFS-(TIMEOUT|ALIVE)' '$out/$n/calls.log'"
    check "$n" "prefs: the menu's Settings item ran" eval_ok "$n" prefs-menu-item
    check "$n" "prefs: GNOME's service opened our Settings window" grep -q '^SMOKE-PREFS ready' "$log"
    check "$n" "prefs: the real window's wm_class is $settings_wm_class" \
        grep -qxF "EVAL-OUT prefs-wm-class (true, '\"$settings_wm_class\"')" "$out/$n/calls.log"
    check "$n" "prefs: ... and it has no GTK app id" grep -qxF "EVAL-OUT prefs-app-id (true, 'null')" "$out/$n/calls.log"
    check "$n" "prefs: extension.js pins that wm_class" \
        grep -qF "const SETTINGS_WM_CLASS = '$settings_wm_class';" "$repo/extension/extension.js"
    check "$n" "prefs: Show learned words… opens the Shell prompt" eval_ok "$n" prefs-prompt
    check "$n" "prefs: Allow lets the window's connection in" eval_ok "$n" prefs-allowed
    check "$n" "prefs: the page lists the personal words" \
        between "$n" prefs-show prefs-forget '"count":"2 words","rows":["kadto","sige"]'
    check "$n" "prefs: ... through the Shell to the engine" between "$n" prefs-show prefs-forget "ListWords () from"
    check "$n" "prefs: the row's Forget button takes the row out" \
        between "$n" prefs-forget prefs-close '"count":"1 word","rows":["sige"]'
    check "$n" "prefs: ... and waits for Undo" [ "$(count "$n" prefs-forget prefs-close "ForgetWords (")" = 0 ]
    check "$n" "prefs: closing sends the pending word once" \
        [ "$(count "$n" prefs-close end "ForgetWords (['kadto'],)")" = 1 ]
    check "$n" "prefs: closing leaves no hold (the service quits)" \
        bash -c "! grep -q '^PREFS-ALIVE' '$out/$n/calls.log'"
    check "$n" "prefs: the closed window's connection leaves the allowed set" eval_ok "$n" prefs-dropped
    check "$n" "prefs: never an error page" bash -c "! grep -q 'Extension Error' '$log'"
    check "$n" "prefs: no stack trace through the extension in the service" \
        bash -c "! grep -q 'extensions/$uuid/' '$log'"
    check "$n" "prefs: no Settings log line contains a word" \
        bash -c "! grep 'correctiond-companion' '$log' | grep -qE 'kadto|sige'"
}
nocfg_ok() {
    grep -qF "EVAL-OUT nocfg-osk (true, '\"true\"')" "$out/no-config-dir/calls.log"
}
nocfg_clean() {
    grep -qF "EVAL-OUT nocfg-disabled (true, '\"ok\"')" "$out/no-config-dir/calls.log"
}
no_trace() { ! grep -q "extensions/$uuid/" "$out/$1/shell.log"; }
loaded() { [[ -s "$out/$1/calls.log" ]] && grep -q 'GNOME Shell started' "$out/$1/shell.log"; }

run_one osk 1 0
check osk "shell started, engine reached" loaded osk
check osk "state pushed at start" before osk open "SetEnabled (True,)"
check osk "OSK open reported" between osk open close "SetOskVisible (True,)"
check osk "OSK close reported" between osk close disable "SetOskVisible (False,)"
check osk "disable leaves the app unidentified" between osk disable enable "SetFocusedApp ('',)"
check osk "re-enable pushes state" between osk enable toggle-off "SetEnabled (True,)"
menu_checks osk
words_checks osk
prefs_checks osk
check osk "touch derives on: the watcher exists" eval_ok osk osk-watcher
check osk "rapid disable, enable, disable leaves no watcher" eval_ok osk rapid-osk-null
check osk "no SetOskVisible(true) after the rapid cycle" \
    [ "$(count osk rapid end 'SetOskVisible (True,)')" = 0 ]
check osk "at most one SetOskVisible(false) after the rapid cycle" \
    [ "$(count osk rapid end 'SetOskVisible (False,)')" -le 1 ]
check osk "no stack trace through the extension" no_trace osk

run_one no-osk 0 4
check no-osk "shell started, engine reached" loaded no-osk
check no-osk "state pushed when the engine appears" before no-osk open "SetEnabled (True,)"
check no-osk "never SetOskVisible(true) without gjs-osk" \
    bash -c "! grep -qF 'SetOskVisible (True,)' '$out/no-osk/calls.log'"
check no-osk "no OSK watcher without gjs-osk" eval_ok no-osk osk-null
menu_checks no-osk
words_checks no-osk
prefs_checks no-osk
check no-osk "no stack trace through the extension" no_trace no-osk

run_one touch-off 1 0 $'[modules]\ntouch = false' touch-off
check touch-off "shell started, engine reached" loaded touch-off
check touch-off "no OSK watcher with touch = false" eval_ok touch-off touch-off-null
check touch-off "never SetOskVisible(true) with touch = false" \
    bash -c "! grep -qF 'SetOskVisible (True,)' '$out/touch-off/calls.log'"
check touch-off "every Eval succeeded" bash -c "! grep -q '^EVAL-FAILED' '$out/touch-off/calls.log'"
check touch-off "no stack trace through the extension" no_trace touch-off

run_one live-config 1 0 $'[modules]\ntouch = true' live-config
check live-config "shell started, engine reached" loaded live-config
check live-config "touch on at start: the watcher exists" eval_ok live-config lc-start
check live-config "OSK open switches the input source (the watcher is live)" eval_ok live-config lc-src-switched
check live-config "OSK open reported" between live-config toggle-open toggle-off "SetOskVisible (True,)"
check live-config "toggle off: SetOskVisible(false) sent with the OSK open" \
    between live-config toggle-off toggle-on "SetOskVisible (False,)"
check live-config "toggle off: no watcher" eval_ok live-config lc-off-null
check live-config "toggle off: the input source is restored" eval_ok live-config lc-src-restored
check live-config "toggle on: the watcher exists" eval_ok live-config lc-on-watcher
check live-config "toggle on: opening the OSK sends SetOskVisible(true)" \
    between live-config toggle-reopen unrelated "SetOskVisible (True,)"
check live-config "unrelated save: no SetOskVisible call" \
    [ "$(count live-config unrelated double-flip 'SetOskVisible')" = 0 ]
check live-config "unrelated save: the input source is unchanged" \
    bash -c "grep -qx 'EVAL-OK lc-unrelated-src-before' '$out/live-config/calls.log' && grep -qx 'EVAL-OK lc-unrelated-src-after' '$out/live-config/calls.log'"
check live-config "unrelated save: the watcher stays" eval_ok live-config lc-unrelated-watcher
check live-config "double flip: no watcher, no timer, source restored" \
    bash -c "grep -qx 'EVAL-OK lc-double-null' '$out/live-config/calls.log' && grep -qx 'EVAL-OK lc-double-clean' '$out/live-config/calls.log' && grep -qx 'EVAL-OK lc-double-src' '$out/live-config/calls.log'"
check live-config "double flip: never SetOskVisible(true)" \
    [ "$(count live-config double-flip deleted 'SetOskVisible (True,)')" = 0 ]
check live-config "config.toml deleted: derived as missing (gjs-osk on disk: on)" eval_ok live-config lc-deleted-watcher
check live-config "invalid TOML keeps the watcher" eval_ok live-config lc-invalid-watcher
check live-config "disable removes watcher, monitor, timer and listener" eval_ok live-config lc-disabled
check live-config "every Eval succeeded" bash -c "! grep -q '^EVAL-FAILED' '$out/live-config/calls.log'"
check live-config "no stack trace through the extension" no_trace live-config

run_one no-config-dir 1 0 '' no-config-dir
check no-config-dir "shell started, engine reached" loaded no-config-dir
check no-config-dir "touch = false written after the folder is created applies live" \
    nocfg_ok
check no-config-dir "no monitor failure was logged" \
    bash -c "! grep -q 'cannot watch config.toml' '$out/no-config-dir/shell.log'"
check no-config-dir "disable releases both monitors, the timer and the listener" nocfg_clean
check no-config-dir "no stack trace through the extension" no_trace no-config-dir

run_one live-gjs-osk 0 0 '' live-gjs-osk
check live-gjs-osk "shell started, engine reached" loaded live-gjs-osk
check live-gjs-osk "no watcher while gjs-osk is not on disk" eval_ok live-gjs-osk lg-start-null
check live-gjs-osk "gjs-osk installed and enabled live: the watcher exists" eval_ok live-gjs-osk lg-watcher
check live-gjs-osk "OSK open reported" between live-gjs-osk open close "SetOskVisible (True,)"
check live-gjs-osk "every Eval succeeded" bash -c "! grep -q '^EVAL-FAILED' '$out/live-gjs-osk/calls.log'"
check live-gjs-osk "no stack trace through the extension" no_trace live-gjs-osk

run_one late-osk 2 0 '' late-osk
check late-osk "shell started, engine reached" loaded late-osk
check late-osk "touch is on at start (gjs-osk is installed)" eval_ok late-osk late-osk-watcher
check late-osk "OSK enabled after us is reported" between late-osk open close "SetOskVisible (True,)"
check late-osk "OSK close reported" between late-osk close end "SetOskVisible (False,)"
check late-osk "every Eval succeeded" bash -c "! grep -q '^EVAL-FAILED' '$out/late-osk/calls.log'"
check late-osk "no stack trace through the extension" no_trace late-osk

run_one v010 0 0 '' v010
check v010 "shell started, engine reached (v0.1.0 mode owned the name)" \
    bash -c "grep -qx 'owned v0.1.0' '$out/v010/calls.log'"
check v010 "toggle off sends SetEnabled(false)" between v010 toggle-off toggle-on "SetEnabled (False,)"
check v010 "toggle on sends SetEnabled(true)" between v010 toggle-on menu-open "SetEnabled (True,)"
check v010 "menu open calls GetStatus" between v010 menu-open end "GetStatus"
check v010 "menu open calls RecentWords" between v010 menu-open end "RecentWords"
check v010 "the menu shows the full state" eval_ok v010 v010-menu
check v010 "every Eval succeeded" bash -c "! grep -q '^EVAL-FAILED' '$out/v010/calls.log'"
check v010 "the extension logs no missing method (older-engine fallback, UnknownMethod, failed call)" \
    bash -c "! grep -qE 'an older version|UnknownMethod|correctiond-companion: .* failed' '$out/v010/shell.log'"
check v010 "no stack trace through the extension" no_trace v010

run_one e1 0 997 '' e1
check e1 "shell started" loaded e1
check e1 "every Eval succeeded" bash -c "! grep -q '^EVAL-FAILED' '$out/e1/calls.log'"
for ev_name in e1-no-engine e1-no-source e1-paused e1-starting e1-timer-one e1-timer-same \
    e1-failed e1-closed e1-live e1-disable; do
    check e1 "$ev_name" eval_ok e1 "$ev_name"
done
check e1 "no stack trace through the extension" no_trace e1

if [[ "$fail" != 0 ]]; then
    for n in e1 osk no-osk touch-off late-osk v010 live-config no-config-dir live-gjs-osk; do
        echo "== $n/calls.log"; cat "$out/$n/calls.log" 2>/dev/null || true
        echo "== $n/shell.log (extension lines)"
        grep -n -B4 "correctiond\|$uuid" "$out/$n/shell.log" 2>/dev/null || true
        echo "== $n/client.log"; cat "$out/$n/client.log" 2>/dev/null || true
        echo "== $n/prefs.log"; cat "$out/$n/prefs.log" 2>/dev/null || true
    done
    exit 1
fi
echo "extension smoke passed"
