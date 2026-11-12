// Correctiond companion: tells the correctiond IBus engine which app has
// focus and whether GJS OSK is on screen, and adds an Autocorrect quick
// toggle whose menu shows the engine's status, a "Learn new words" switch
// and the recently learned words. It also exports the companion object,
// which the Settings window calls: ConfirmForget, to forget every word, and
// RequestWordAccess, which (after the user allows it in a Shell prompt) lets
// that Settings window's connection call ListWords and ForgetWords, passed
// through to the engine.
//
// Every call goes out on Gio.DBus.session: only that connection owns
// org.gnome.Shell, which the engine requires for all but GetStats.
// Pure logic lives in lib.js (tested with `gjs -m extension/test/lib.test.js`).

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as Dialog from 'resource:///org/gnome/shell/ui/dialog.js';
import * as ModalDialog from 'resource:///org/gnome/shell/ui/modalDialog.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import * as QuickSettings from 'resource:///org/gnome/shell/ui/quickSettings.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

import * as Lib from './lib.js';
import {parseConfig} from './config.js';
import {OskWatcher} from './osk.js';
import {getInputSourceManager} from 'resource:///org/gnome/shell/ui/status/keyboard.js';

// Monotonic milliseconds, for the first-run wait.
const nowMs = () => GLib.get_monotonic_time() / 1000;

const BUS_NAME = 'org.nadelacruz.Correctiond1';
const OBJECT_PATH = '/org/nadelacruz/Correctiond';
const ICON = 'tools-check-spelling-symbolic';
const CALL_TIMEOUT_MS = 2000;
// Purge can wait behind an import, a flush or a keyring delete in the
// engine, so it does not use CALL_TIMEOUT_MS. This is GDBus's own default
// (25 s): on this proxy, -1 would mean its default timeout, CALL_TIMEOUT_MS.
const PURGE_TIMEOUT_MS = 25000;
const UNKNOWN_METHOD = 'org.freedesktop.DBus.Error.UnknownMethod';
// One GetStatus per focus change, not one per notify in a burst.
const SUBTITLE_DEBOUNCE_MS = 150;
// config.toml events settle this long before touch is re-derived.
const CONFIG_DEBOUNCE_MS = 200;
const FORGET_FAILED_MS = 3000;
const COMPANION_PATH = '/org/nadelacruz/CorrectiondCompanion';
// GNOME's extension preferences service (our Settings window's process) owns
// this bus name. It is no Gtk.Application (dbusService.js owns the name with
// Gio.DBus.own_name), so its windows have no GTK app id; their Wayland app id,
// the wm_class, is its program name. scripts/extension-smoke.sh reads this
// value from the real service's window.
const SETTINGS_BUS_NAME = 'org.gnome.Shell.Extensions';
const SETTINGS_WM_CLASS = 'org.gnome.Shell.Extensions';
const ACCESS_DENIED = 'org.freedesktop.DBus.Error.AccessDenied';
// At most this many connections may list and forget words; the oldest goes.
const MAX_WORD_CLIENTS = 2;
// An open words request answers false after this long, before the caller's
// own 120 s timeout gives up on it.
const WORDS_REQUEST_MS = 115000;

const IFACE = `<node>
  <interface name="org.nadelacruz.Correctiond1">
    <method name="SetFocusedApp"><arg type="s" name="wm_class" direction="in"/></method>
    <method name="SetOskVisible"><arg type="b" name="visible" direction="in"/></method>
    <method name="SetEnabled"><arg type="b" name="enabled" direction="in"/></method>
    <method name="Purge"/>
    <method name="GetStats"><arg type="s" name="stats" direction="out"/></method>
    <method name="GetStatus"><arg type="s" name="status" direction="out"/></method>
    <method name="SetLearning"><arg type="b" name="learning" direction="in"/></method>
    <method name="RecentWords"><arg type="as" name="words" direction="out"/></method>
    <method name="ForgetWord">
      <arg type="s" name="word" direction="in"/>
      <arg type="b" name="forgotten" direction="out"/>
    </method>
    <method name="ListWords"><arg type="a(sutb)" name="words" direction="out"/></method>
    <method name="ForgetWords">
      <arg type="as" name="words" direction="in"/>
      <arg type="u" name="forgotten" direction="out"/>
    </method>
  </interface>
</node>`;
const CorrectiondProxy = Gio.DBusProxy.makeProxyWrapper(IFACE);

const COMPANION_IFACE = `<node>
  <interface name="org.nadelacruz.CorrectiondCompanion1">
    <method name="ConfirmForget"/>
    <method name="RequestWordAccess"><arg type="b" name="allowed" direction="out"/></method>
    <method name="ListWords"><arg type="a(sutb)" name="words" direction="out"/></method>
    <method name="ForgetWords">
      <arg type="as" name="words" direction="in"/>
      <arg type="u" name="forgotten" direction="out"/>
    </method>
  </interface>
</node>`;

/** Thrown by _call when the engine has no owner: normal, never logged. */
class EngineDown extends Error {}

const AutocorrectToggle = GObject.registerClass({
    GTypeName: 'CorrectiondCompanionToggle',
}, class AutocorrectToggle extends QuickSettings.QuickMenuToggle {
    _init(checked) {
        super._init({title: 'Autocorrect', iconName: ICON, toggleMode: true, checked});
        this.menu.setHeader(ICON, 'Autocorrect');
    }
});

const AutocorrectIndicator = GObject.registerClass({
    GTypeName: 'CorrectiondCompanionIndicator',
}, class AutocorrectIndicator extends QuickSettings.SystemIndicator {
    _init(checked) {
        super._init();
        this.toggle = new AutocorrectToggle(checked);
        this.quickSettingsItems.push(this.toggle);
    }
});

export default class CorrectiondCompanion extends Extension {
    constructor(metadata) {
        super(metadata);
        // Kept across disable/enable (the screen lock disables extensions),
        // so an "off" survives a lock; pushed whenever the engine (re)appears.
        // The learning pause is not kept here: the engine owns it (a restart
        // learns again) and the switch is read back from GetStatus.
        this._correctionOn = true;
        // Monotonic across enable cycles, so a reply from before a disable
        // is never applied to the next enable's menu.
        this._refreshSerial = 0;
        this._subtitleSerial = 0;
    }

    enable() {
        this._logged = new Set();
        this._app = new Lib.Latch(null);

        this._cancellable = new Gio.Cancellable();
        this._proxy = null;
        const cancellable = this._cancellable;
        CorrectiondProxy(Gio.DBus.session, BUS_NAME, OBJECT_PATH, (proxy, error) => {
            if (cancellable.is_cancelled())
                return;
            if (error) {
                this._logOnce('proxy', `cannot create the engine proxy: ${error.message}`);
                return;
            }
            proxy.set_default_timeout(CALL_TIMEOUT_MS);
            this._proxy = proxy;
            this._ownerId = proxy.connect('notify::g-name-owner', () => this._onOwner());
            this._onOwner();
        }, cancellable,
        Gio.DBusProxyFlags.DO_NOT_LOAD_PROPERTIES |
            Gio.DBusProxyFlags.DO_NOT_CONNECT_SIGNALS |
            Gio.DBusProxyFlags.DO_NOT_AUTO_START);

        // First-run guidance (spec 8): probes, wait clock and the one timer.
        this._componentProbe = null; // smoke hook: a function replaces the file check
        this._inputSettings = new Gio.Settings({schema_id: 'org.gnome.desktop.input-sources'});
        this._setupSince = null;
        this._sourcePrevCurrent = false;
        this._setupTimerId = 0;
        this._menuOpen = false;

        this._subtitleId = 0;
        this._focusWindow = null;
        this._wmClassId = 0;
        this._focusId = global.display.connect('notify::focus-window', () => this._onFocus());
        this._keyFocusId = global.stage.connect('notify::key-focus', () => this._sendApp());
        this._onFocus();

        // Touch support: the OSK watcher exists only while touch is on.
        this._osk = null;
        this._touch = null;
        this._configMonitor = null;
        this._configMonitorId = 0;
        this._configDebounceId = 0;
        this._gjsOskStateId = 0;
        const read = this._readConfig();
        this._touchKeyAbsent = !(read.kind === 'ok' && read.touch !== null);
        this._setTouch(Lib.touchAfterRead(read, null, this._gjsOskInstalled()));
        this._watchTouchSources();

        this._indicator = new AutocorrectIndicator(this._correctionOn);
        this._toggleId = this._indicator.toggle.connect('notify::checked', toggle => {
            this._correctionOn = toggle.checked;
            this._send('SetEnabled', this._correctionOn);
            this._queueSubtitle();
        });
        this._buildMenu(this._indicator.toggle.menu);
        Main.panel.statusArea.quickSettings.addExternalIndicator(this._indicator);

        // Unique names allowed to list and forget words, oldest first, each
        // with its name watch. Never stored: a new enable starts empty.
        this._wordClients = new Map();
        this._wordsRequest = null;
        this._wordsDialog = null;
        this._companion = Gio.DBusExportedObject.wrapJSObject(COMPANION_IFACE, {
            ConfirmForget: () => this._confirmForget(),
            RequestWordAccessAsync: (_params, invocation) => this._requestWordAccess(invocation),
            ListWordsAsync: (_params, invocation) => this._passWords(invocation, 'ListWords'),
            ForgetWordsAsync: (_params, invocation) => this._passWords(invocation, 'ForgetWords'),
        });
        try {
            this._companion.export(Gio.DBus.session, COMPANION_PATH);
        } catch (e) {
            this._companion = null;
            this._logOnce('companion', `cannot export the companion object: ${e.message}`);
        }
    }

    disable() {
        // An open words prompt answers false (finish removes its timer and
        // watch); nobody stays allowed.
        this._wordsRequest?.finish(false);
        this._wordsRequest = null;
        this._wordsDialog = null;
        for (const id of this._wordClients.values())
            Gio.bus_unwatch_name(id);
        this._wordClients.clear();
        this._wordClients = null;
        this._companion?.unexport();
        this._companion = null;
        this._forgetDialog?.close();
        this._forgetDialog = null;

        this._refreshSerial++;
        this._subtitleSerial++;
        this._indicator.toggle.menu.disconnect(this._menuOpenId);
        this._menuOpen = false;
        this._clearSetupTimer();
        this._inputSettings = null;
        this._learnSwitch.disconnect(this._learnId);
        this._appItem = this._countsItem = this._learnSwitch = this._recentSection = null;
        this._learnSeparator = this._recentHeader = null;
        this._indicator.toggle.disconnect(this._toggleId);
        this._indicator.quickSettingsItems.forEach(item => item.destroy());
        this._indicator.destroy();
        this._indicator = null;

        // No re-derive may run from here on.
        this._unwatchTouchSources();
        // Restores a switched input source; the engine hears the OSK is gone.
        this._setTouch(false);
        this._touch = null;

        global.display.disconnect(this._focusId);
        global.stage.disconnect(this._keyFocusId);
        this._setFocusWindow(null);

        // Without the extension the engine must see what "not running" means:
        // no app identified, no OSK (sent by _setTouch(false) above). The on/off choice is left as it is.
        this._send('SetFocusedApp', '');

        this._cancellable.cancel();
        if (this._proxy)
            this._proxy.disconnect(this._ownerId);
        this._proxy = null;
        // Last: every handler that can queue it (toggle, switch, focus,
        // key focus, owner) is disconnected by now.
        if (this._subtitleId) {
            GLib.source_remove(this._subtitleId);
            this._subtitleId = 0;
        }
        this._logged = null;
    }

    // --- D-Bus ---

    /** The engine (re)appeared: it starts unidentified, OSK hidden, enabled,
     * learning. The pause is not pushed (a restart learns again); the switch
     * follows GetStatus. Also runs when it goes away, for the subtitle. */
    _onOwner() {
        this._queueSubtitle();
        if (!this._proxy?.g_name_owner) {
            this._setupSince = nowMs();
            return;
        }
        this._logged.clear();
        this._app = new Lib.Latch(null);
        this._sendApp();
        this._send('SetOskVisible', this._osk?.shown ?? false);
        this._send('SetEnabled', this._correctionOn);
    }

    /** A call whose reply matters: resolves to the out args. */
    _call(method, ...args) {
        const proxy = this._proxy;
        if (!proxy?.g_name_owner)
            return Promise.reject(new EngineDown());
        return proxy[`${method}Async`](...args);
    }

    /** Fire and forget; failures are logged once and never thrown. */
    _send(method, ...args) {
        const proxy = this._proxy;
        if (!proxy?.g_name_owner)
            return; // not running: state is pushed again when it appears
        try {
            proxy[`${method}Async`](...args).catch(e => this._logCallError(method, e));
        } catch (e) {
            this._logCallError(method, e);
        }
    }

    _logCallError(method, e) {
        if (e instanceof EngineDown)
            return;
        const name = (e instanceof GLib.Error && Gio.DBusError.get_remote_error(e)) || 'local';
        this._logOnce(`${method}:${name}`, `${method} failed: ${name}`);
    }

    /**
     * What openPreferences() does, but with the failure caught. In GNOME
     * Shell 50, openPreferences() returns nothing and the shell makes this
     * call without a handler, so a failure is only an "Unhandled promise
     * rejection" stack trace in the journal, without the error.
     */
    _openSettings() {
        Gio.DBus.session.call(
            'org.gnome.Shell.Extensions',
            '/org/gnome/Shell/Extensions',
            'org.gnome.Shell.Extensions',
            'OpenExtensionPrefs',
            new GLib.Variant('(ssa{sv})', [this.uuid, '', {}]),
            null,
            Gio.DBusCallFlags.NONE,
            -1,
            null
        ).catch(e => {
            const name = (e instanceof GLib.Error && Gio.DBusError.get_remote_error(e)) || 'local';
            this._logOnce(`OpenExtensionPrefs:${name}`,
                `cannot open Settings: ${name}: ${e.message}`);
        });
    }

    _logOnce(key, msg) {
        if (!this._logged || this._logged.has(key))
            return;
        this._logged.add(key);
        console.warn(`correctiond-companion: ${msg}`);
    }

    // --- focused app ---

    _onFocus() {
        this._setFocusWindow(global.display.focus_window);
        this._sendApp();
        this._queueSubtitle();
    }

    _setFocusWindow(win) {
        if (this._focusWindow)
            this._focusWindow.disconnect(this._wmClassId);
        this._focusWindow = win;
        this._wmClassId = win ? win.connect('notify::wm-class', () => this._sendApp()) : 0;
    }

    _sendApp() {
        const app = Lib.focusedApp(
            this._focusWindow?.get_wm_class(),
            global.stage.key_focus instanceof Clutter.Text);
        if (this._app.set(app))
            this._send('SetFocusedApp', app);
    }

    // --- touch support (GJS OSK) ---

    /** config.toml as {kind:'missing'} | {kind:'invalid'} |
     * {kind:'ok', touch, oskSource}. Never logs the file's contents. */
    _readConfig() {
        const path = GLib.build_filenamev([GLib.get_home_dir(), '.config', 'correctiond', 'config.toml']);
        let text;
        try {
            [, text] = GLib.file_get_contents(path);
            text = new TextDecoder().decode(text);
        } catch (e) {
            return {kind: 'missing'}; // missing or unreadable
        }
        try {
            const parsed = parseConfig(text);
            if (!parsed.ok)
                return {kind: 'invalid'};
            return {kind: 'ok', touch: parsed.config.modules.touch, oskSource: parsed.config.oskSource};
        } catch (e) {
            return {kind: 'invalid'};
        }
    }

    _gjsOskInstalled() {
        const dirs = [GLib.get_user_data_dir(), ...GLib.get_system_data_dirs()];
        return Lib.gjsOskInstalledIn(dirs,
            path => GLib.file_test(path, GLib.FileTest.IS_DIR));
    }

    /** Watches config.toml (200 ms debounce) and gjs-osk's extension state,
     * so the touch value follows both live. GIO does not see a file appear
     * under a folder that does not exist yet (checked in the no-config-dir
     * smoke scenario), so while ~/.config/correctiond is missing the parent
     * folder is watched, and the file monitor attaches when it appears. */
    _watchTouchSources() {
        try {
            this._attachConfigMonitor();
        } catch (e) {
            this._dropConfigMonitors();
            this._logOnce('config-monitor',
                `cannot watch config.toml (${e.message}); touch changes apply at the next enable`);
        }
        // While [modules] touch is absent (missing file, invalid file or no
        // key), a gjs-osk install or enable re-derives touch.
        this._gjsOskStateId = Main.extensionManager.connect('extension-state-changed', (_m, ext) => {
            if (ext?.uuid === Lib.GJS_OSK_UUID && this._touchKeyAbsent)
                this._recomputeTouch();
        });
    }

    _queueRecompute() {
        if (this._configDebounceId)
            GLib.source_remove(this._configDebounceId);
        this._configDebounceId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, CONFIG_DEBOUNCE_MS, () => {
            this._configDebounceId = 0;
            this._recomputeTouch();
            return GLib.SOURCE_REMOVE;
        });
    }

    _attachConfigMonitor() {
        const dir = GLib.build_filenamev([GLib.get_home_dir(), '.config', 'correctiond']);
        if (GLib.file_test(dir, GLib.FileTest.IS_DIR)) {
            this._configMonitor = Gio.File.new_for_path(GLib.build_filenamev([dir, 'config.toml']))
                .monitor_file(Gio.FileMonitorFlags.WATCH_MOVES, null);
            this._configMonitorId = this._configMonitor.connect('changed', () => this._queueRecompute());
            return;
        }
        // The folder is missing: watch its parent for the folder to appear.
        this._configMonitor = Gio.File.new_for_path(GLib.path_get_dirname(dir))
            .monitor_directory(Gio.FileMonitorFlags.WATCH_MOVES, null);
        this._configMonitorId = this._configMonitor.connect('changed', () => {
            if (!GLib.file_test(dir, GLib.FileTest.IS_DIR))
                return;
            try {
                this._dropConfigMonitors();
                this._attachConfigMonitor();
            } catch (e) {
                this._dropConfigMonitors();
                this._logOnce('config-monitor',
                    `cannot watch config.toml (${e.message}); touch changes apply at the next enable`);
            }
            // The file may be written before the file monitor exists.
            this._queueRecompute();
        });
    }

    _dropConfigMonitors() {
        if (this._configMonitor) {
            this._configMonitor.disconnect(this._configMonitorId);
            this._configMonitor.cancel();
            this._configMonitor = null;
            this._configMonitorId = 0;
        }
    }

    _unwatchTouchSources() {
        if (this._configDebounceId) {
            GLib.source_remove(this._configDebounceId);
            this._configDebounceId = 0;
        }
        this._dropConfigMonitors();
        if (this._gjsOskStateId) {
            Main.extensionManager.disconnect(this._gjsOskStateId);
            this._gjsOskStateId = 0;
        }
    }

    /** Re-reads config.toml and the disk; acts only when touch changes.
     * A failed OskWatcher construction is not retried here: touch stays "on"
     * with no watcher until the next enable, so a failing construct logs once. */
    _recomputeTouch() {
        const read = this._readConfig();
        this._touchKeyAbsent = !(read.kind === 'ok' && read.touch !== null);
        const next = Lib.touchAfterRead(read, this._touch, this._gjsOskInstalled());
        if (next !== this._touch)
            this._setTouch(next);
    }

    /** Builds or tears down the OSK watcher (synchronously). */
    _setTouch(on) {
        this._touch = on;
        if (on && !this._osk) {
            try {
                const osk = new OskWatcher({
                    onChange: shown => this._send('SetOskVisible', shown),
                    logOnce: (key, msg) => this._logOnce(key, msg),
                    // The US engine when config.toml is missing or invalid.
                    readOskSource: () => {
                        const read = this._readConfig();
                        return read.kind === 'ok' ? read.oskSource : Lib.QWERTY_ENGINE;
                    },
                });
                try {
                    osk.enable();
                } catch (e) {
                    try {
                        osk.disable(); // drop what enable() connected
                    } catch (_e) { /* the first error is the one to report */ }
                    throw e;
                }
                this._osk = osk;
            } catch (e) {
                this._osk = null;
                this._logOnce('osk-construct',
                    `osk.js could not be loaded (${e.message}); no on-screen keyboard support`);
                logError(e);
            }
        } else if (!on && this._osk) {
            const osk = this._osk;
            this._osk = null;
            if (osk.disable())
                this._send('SetOskVisible', false);
        }
    }

    // --- menu ---

    _buildMenu(menu) {
        this._appItem = new PopupMenu.PopupMenuItem('', {reactive: false});
        this._countsItem = new PopupMenu.PopupMenuItem('', {reactive: false});
        this._countsItem.hide();
        menu.addMenuItem(this._appItem);
        menu.addMenuItem(this._countsItem);
        this._learnSeparator = new PopupMenu.PopupSeparatorMenuItem();
        menu.addMenuItem(this._learnSeparator);

        // On until GetStatus says otherwise (the engine starts learning).
        this._learnSwitch = new PopupMenu.PopupSwitchMenuItem('Learn new words', true);
        this._syncingLearn = false;
        this._learnId = this._learnSwitch.connect('toggled', (_item, state) => {
            if (this._syncingLearn)
                return; // read back from the engine, not a user change
            this._send('SetLearning', state);
            this._queueSubtitle();
        });
        menu.addMenuItem(this._learnSwitch);

        this._recentHeader = new PopupMenu.PopupSeparatorMenuItem('Recently learned');
        menu.addMenuItem(this._recentHeader);
        this._recentSection = new PopupMenu.PopupMenuSection();
        menu.addMenuItem(this._recentSection);
        this._showRecent([]);

        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        menu.addAction('Autocorrect Settings…', () => this._openSettings());

        this._menuOpenId = menu.connect('open-state-changed', (_m, open) => {
            this._menuOpen = open;
            if (open)
                this._refreshMenu();
            else
                this._clearSetupTimer();
        });
    }

    /** The focused window's app name, or null. */
    _appName() {
        const win = global.display.focus_window;
        if (!win)
            return null;
        return Shell.WindowTracker.get_default().get_window_app(win)?.get_name() || null;
    }

    /** The JSON object in a settled `(s)` reply, or null. */
    _parseReply(method, result) {
        if (result.status !== 'fulfilled') {
            this._logCallError(method, result.reason);
            return null;
        }
        try {
            const value = JSON.parse(result.value[0]);
            return value && typeof value === 'object' ? value : null;
        } catch (e) {
            this._logOnce(`${method}:json`, `${method} reply is not JSON`);
            return null;
        }
    }

    /** GetStatus's settled reply for statusText: the object;
     * Lib.STATUS_UNSUPPORTED for an engine from before GetStatus (it answers
     * UnknownMethod); or null (down, or a bad reply). */
    _statusReply(result) {
        const e = result.reason;
        if (result.status === 'rejected' && e instanceof GLib.Error &&
            Gio.DBusError.get_remote_error(e) === UNKNOWN_METHOD) {
            this._logOnce('GetStatus:unsupported', 'the engine has no GetStatus (an older version)');
            return Lib.STATUS_UNSUPPORTED;
        }
        return this._parseReply('GetStatus', result);
    }

    /** _refresh, never rejecting (it runs from signal handlers). */
    _refreshMenu() {
        this._refresh().catch(() => this._logOnce('refresh', 'menu refresh failed'));
    }

    /** The first-run state (Lib.setupState) from the live probes. */
    _probeSetup() {
        const installed = typeof this._componentProbe === 'function'
            ? !!this._componentProbe()
            : Lib.componentInstalledIn(Lib.componentDirs(GLib.getenv('IBUS_COMPONENT_PATH')),
                path => GLib.file_test(path, GLib.FileTest.IS_REGULAR));
        let configured = false;
        try {
            configured = this._inputSettings.get_value('sources').deepUnpack()
                .some(src => Lib.isCorrectiondSource(src));
        } catch (e) {
            this._logOnce('sources', 'cannot read the input sources');
        }
        let current = false;
        try {
            const src = getInputSourceManager().currentSource;
            current = !!src && Lib.isCorrectiondSource([src.type, src.id]);
        } catch (e) {
            this._logOnce('currentSource', 'cannot read the current input source');
        }
        if (current && !this._sourcePrevCurrent)
            this._setupSince = nowMs();
        this._sourcePrevCurrent = current;
        return Lib.setupState({
            componentInstalled: installed,
            sourceConfigured: configured,
            sourceCurrent: current,
            engineOwned: !!this._proxy?.g_name_owner,
            ownerWaitExpired: this._setupSince !== null &&
                nowMs() - this._setupSince >= Lib.SETUP_WAIT_MS,
        });
    }

    /** While the menu is open and the state is 'starting': one timer that
     * refreshes the menu once the wait is over. Never a second. */
    _armSetupTimer(starting) {
        if (!starting || !this._menuOpen || this._setupTimerId)
            return;
        this._setupTimerId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, Lib.SETUP_WAIT_MS, () => {
            this._setupTimerId = 0;
            this._refreshMenu();
            return GLib.SOURCE_REMOVE;
        });
    }

    _clearSetupTimer() {
        if (this._setupTimerId)
            GLib.source_remove(this._setupTimerId);
        this._setupTimerId = 0;
    }

    /** Fill the menu: GetStats, GetStatus and RecentWords, once per open. */
    async _refresh() {
        const serial = ++this._refreshSerial;
        const [stats, status, words] = await Promise.allSettled([
            this._call('GetStats'), this._call('GetStatus'), this._call('RecentWords'),
        ]);
        if (serial !== this._refreshSerial || !this._indicator)
            return;
        const statsObj = this._parseReply('GetStats', stats);
        const statusObj = this._statusReply(status);
        const text = Lib.statusText(statsObj, statusObj, this._appName());
        const setup = this._probeSetup();
        if (setup !== null) {
            text.appLine = Lib.SETUP_TEXT.quick[setup];
            text.countsLine = null;
            text.controls = false;
        }
        this._armSetupTimer(setup === 'starting');
        this._indicator.toggle.subtitle = text.subtitle;
        this._appItem.label.text = text.appLine;
        this._countsItem.label.text = text.countsLine ?? '';
        this._countsItem.visible = text.countsLine !== null;
        // No controls when down (either reply missing) or on an older engine,
        // which has no SetLearning, RecentWords or ForgetWord.
        this._showEngine(text.controls ? statusObj : null);
        if (text.controls && words.status !== 'fulfilled')
            this._logCallError('RecentWords', words.reason);
        this._showRecent(text.controls && words.status === 'fulfilled' ? words.value[0] : []);
    }

    /** Engine up (its GetStatus object): show the switch and the list, with
     * the switch read back from `learning`, never echoed as SetLearning.
     * Down (null): hide them. The counts line is _refresh's. */
    _showEngine(status) {
        const up = status !== null;
        for (const item of [this._learnSeparator, this._learnSwitch, this._recentHeader])
            item.visible = up;
        this._recentSection.actor.visible = up;
        if (!up)
            return;
        this._syncingLearn = true;
        try {
            this._learnSwitch.setToggleState(status.learning !== false);
        } finally {
            this._syncingLearn = false;
        }
    }

    _queueSubtitle() {
        if (this._subtitleId)
            GLib.source_remove(this._subtitleId);
        this._subtitleId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, SUBTITLE_DEBOUNCE_MS, () => {
            this._subtitleId = 0;
            this._refreshSubtitle().catch(() => this._logOnce('subtitle', 'subtitle refresh failed'));
            return GLib.SOURCE_REMOVE;
        });
    }

    /** The toggle's subtitle from GetStatus alone. */
    async _refreshSubtitle() {
        const serial = ++this._subtitleSerial;
        const [result] = await Promise.allSettled([this._call('GetStatus')]);
        if (serial !== this._subtitleSerial || !this._indicator)
            return;
        const status = this._statusReply(result);
        // The counts are not shown here; {} only says the engine is up.
        const text = Lib.statusText(status && {}, status, this._appName());
        this._indicator.toggle.subtitle = text.subtitle;
        this._showEngine(text.controls ? status : null);
    }

    _showRecent(words) {
        this._recentSection.removeAll();
        if (!words.length) {
            this._recentSection.addMenuItem(
                new PopupMenu.PopupMenuItem('(none yet this session)', {reactive: false}));
            return;
        }
        for (const word of words.slice(0, 5))
            this._recentSection.addMenuItem(this._recentRow(word));
    }

    /** A recent word (row.label) with its ✕ (row.forgetButton). The row itself is not
     * activatable, so a click on the word neither forgets nor closes the menu. */
    _recentRow(word) {
        const row = new PopupMenu.PopupBaseMenuItem({reactive: false});
        const label = new St.Label({text: word, x_expand: true, y_align: Clutter.ActorAlign.CENTER});
        const button = new St.Button({
            style_class: 'icon-button',
            icon_name: 'window-close-symbolic',
            can_focus: true,
            accessible_name: 'Forget this word',
        });
        row.add_child(label);
        row.add_child(button);
        row.label = label;
        row.forgetButton = button;
        let resetId = 0;
        let alive = true;
        row.connect('destroy', () => {
            alive = false;
            if (resetId)
                GLib.source_remove(resetId);
            resetId = 0;
        });
        button.connect('clicked', async () => {
            const failure = await this._forgetWord(word);
            if (!alive)
                return;
            if (failure === null) {
                this._refreshMenu();
                return;
            }
            label.text = failure;
            if (resetId)
                GLib.source_remove(resetId);
            resetId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, FORGET_FAILED_MS, () => {
                resetId = 0;
                label.text = word;
                return GLib.SOURCE_REMOVE;
            });
        });
        return row;
    }

    /** ForgetWord: null when done (true, or false for already gone), else the
     * row's failure text. */
    async _forgetWord(word) {
        try {
            await this._call('ForgetWord', word);
            return null;
        } catch (e) {
            let name = null;
            if (e instanceof GLib.Error) {
                name = Gio.DBusError.get_remote_error(e);
                Gio.DBusError.strip_remote_error(e);
            }
            this._logOnce(`ForgetWord:${name ?? 'local'}`, `ForgetWord failed: ${name ?? 'local'}`);
            return Lib.forgetFailureText({name, message: e.message});
        }
    }

    // --- forget all (the companion's ConfirmForget) ---

    _confirmForget() {
        if (this._forgetDialog)
            return;
        const dialog = new ModalDialog.ModalDialog({destroyOnClose: true});
        dialog.contentLayout.add_child(new Dialog.MessageDialogContent({
            title: 'Forget learned words?',
            description: 'Autocorrect deletes every word it has learned from your typing. ' +
                'This cannot be undone.',
        }));
        // Cancel is the default and has the initial key focus, so a stray
        // Return or Space cancels: this dialog can open over any app.
        // Forget takes a click, or Tab then Return.
        dialog.setButtons([
            {label: 'Cancel', action: () => dialog.close(), key: Clutter.KEY_Escape, default: true},
            {
                label: 'Forget',
                action: () => {
                    dialog.close();
                    this._forget();
                },
            },
        ]);
        dialog.connect('destroy', () => {
            if (this._forgetDialog === dialog)
                this._forgetDialog = null;
        });
        this._forgetDialog = dialog;
        dialog.open();
    }

    async _forget() {
        let error = null;
        try {
            const proxy = this._proxy;
            if (!proxy?.g_name_owner)
                throw new Error('engine not running');
            await new Promise((resolve, reject) => {
                proxy.call('Purge', null, Gio.DBusCallFlags.NONE, PURGE_TIMEOUT_MS, null, (p, res) => {
                    try {
                        resolve(p.call_finish(res));
                    } catch (e) {
                        reject(e);
                    }
                });
            });
        } catch (e) {
            let name = null;
            if (e instanceof GLib.Error) {
                name = Gio.DBusError.get_remote_error(e);
                Gio.DBusError.strip_remote_error(e);
            }
            error = {name, message: e.message};
            this._logOnce(`Purge:${name ?? 'local'}`, `Purge failed: ${name ?? 'local'}`);
        }
        const {title, body} = Lib.purgeNotice(error);
        Main.notify(title, body);
    }

    // --- learned words access (the companion's RequestWordAccess) ---

    /** RequestWordAccess: true once the user allows the calling connection in
     * a prompt. False at once, with no prompt, unless the caller is the
     * process that owns org.gnome.Shell.Extensions and has the focused
     * window, whose wm_class is SETTINGS_WM_CLASS; and false while another
     * request is open. The prompt can wait for the user, so this replies
     * later: false after WORDS_REQUEST_MS (the caller's timeout is 120 s),
     * or as soon as the caller's connection goes. */
    _requestWordAccess(invocation) {
        const reply = allowed => invocation.return_value(new GLib.Variant('(b)', [allowed]));
        if (this._wordClients === null) { // a call dispatched after disable
            reply(false);
            return;
        }
        if (this._wordsRequest) {
            this._logOnce('words:busy', 'word access refused: a request is already open');
            reply(false);
            return;
        }
        const sender = invocation.get_sender();
        const request = {
            timeoutId: 0,
            watchId: 0,
            /** Answer once; closes the prompt if it is open. Every path that
             * ends the request (disable too) comes here. */
            finish: allowed => {
                if (this._wordsRequest !== request)
                    return;
                this._wordsRequest = null;
                if (request.timeoutId)
                    GLib.source_remove(request.timeoutId);
                request.timeoutId = 0;
                if (request.watchId)
                    Gio.bus_unwatch_name(request.watchId);
                request.watchId = 0;
                const dialog = this._wordsDialog;
                this._wordsDialog = null;
                dialog?.close();
                reply(allowed);
            },
        };
        this._wordsRequest = request;
        request.timeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, WORDS_REQUEST_MS, () => {
            request.timeoutId = 0;
            this._logOnce('words:timeout', 'word access refused: the prompt timed out');
            request.finish(false);
            return GLib.SOURCE_REMOVE;
        });
        request.watchId = Gio.bus_watch_name_on_connection(Gio.DBus.session, sender,
            Gio.BusNameWatcherFlags.NONE, null, () => {
                this._logOnce('words:gone', 'word access refused: the caller is gone');
                request.finish(false);
            });
        this._checkAndPrompt(request, sender).catch(() => {
            this._logOnce('words:prompt', 'word access prompt failed');
            request.finish(false);
        });
    }

    async _checkAndPrompt(request, sender) {
        const ok = await this._isFocusedSettings(sender);
        if (this._wordsRequest !== request)
            return; // disabled meanwhile: answered false
        if (!ok) {
            this._logOnce('words:checks', 'word access refused: not the focused Settings window');
            request.finish(false);
            return;
        }
        this._promptWords(request, sender);
    }

    /** Whether SENDER's process owns org.gnome.Shell.Extensions and has the
     * focused window, whose wm_class is SETTINGS_WM_CLASS. */
    async _isFocusedSettings(sender) {
        let ownerPid, callerPid;
        try {
            const [owner] = await this._busCall('GetNameOwner', SETTINGS_BUS_NAME, '(s)');
            [ownerPid] = await this._busCall('GetConnectionUnixProcessID', owner, '(u)');
            [callerPid] = await this._busCall('GetConnectionUnixProcessID', sender, '(u)');
        } catch (e) {
            return false; // no Settings window, or the caller is gone
        }
        const win = global.display.focus_window;
        return callerPid === ownerPid && win?.get_pid() === callerPid &&
            win.get_wm_class() === SETTINGS_WM_CLASS;
    }

    /** A call to the bus (org.freedesktop.DBus) with one string: its out args. */
    _busCall(method, arg, replyType) {
        return new Promise((resolve, reject) => {
            Gio.DBus.session.call('org.freedesktop.DBus', '/org/freedesktop/DBus',
                'org.freedesktop.DBus', method, new GLib.Variant('(s)', [arg]),
                new GLib.VariantType(replyType), Gio.DBusCallFlags.NONE, CALL_TIMEOUT_MS, null,
                (conn, res) => {
                    try {
                        resolve(conn.call_finish(res).deepUnpack());
                    } catch (e) {
                        reject(e);
                    }
                });
        });
    }

    _promptWords(request, sender) {
        const dialog = new ModalDialog.ModalDialog({destroyOnClose: true});
        dialog.contentLayout.add_child(new Dialog.MessageDialogContent({
            title: 'Show your learned words in Settings?',
            description: 'Settings can list and forget your learned words until you close it.',
        }));
        let allowed = false;
        // As in _confirmForget: Cancel is the default and has the key focus,
        // so a stray Return or Space cancels. Allow takes a click, or Tab
        // then Return.
        dialog.setButtons([
            {label: 'Cancel', action: () => request.finish(false), key: Clutter.KEY_Escape, default: true},
            {
                label: 'Allow',
                action: () => {
                    allowed = true;
                    this._wordsDialog = null;
                    dialog.close();
                    this._allowIfPresent(request, sender).catch(() => request.finish(false));
                },
            },
        ]);
        // Closed any other way (disable, or the Shell): false.
        dialog.connect('destroy', () => {
            if (this._wordsDialog === dialog)
                this._wordsDialog = null;
            if (!allowed)
                request.finish(false);
        });
        this._wordsDialog = dialog;
        if (!dialog.open()) {
            this._logOnce('words:modal', 'word access prompt could not open');
            this._wordsDialog = null;
            request.finish(false);
            dialog.destroy();
        }
    }

    /** After Allow: allow SENDER if it still has a bus connection. */
    async _allowIfPresent(request, sender) {
        let present = true;
        try {
            await this._busCall('GetNameOwner', sender, '(s)');
        } catch (e) {
            present = false;
        }
        if (this._wordsRequest !== request)
            return; // disabled meanwhile: answered false
        if (present)
            this._allowWordClient(sender);
        else
            this._logOnce('words:gone', 'word access refused: the caller is gone');
        request.finish(present);
    }

    /** Add NAME to the allowed set (oldest out past MAX_WORD_CLIENTS),
     * watched so it goes when its connection closes. */
    _allowWordClient(name) {
        if (this._wordClients.has(name))
            return;
        if (this._wordClients.size >= MAX_WORD_CLIENTS)
            this._dropWordClient(this._wordClients.keys().next().value);
        const id = Gio.bus_watch_name_on_connection(Gio.DBus.session, name,
            Gio.BusNameWatcherFlags.NONE, null, () => this._dropWordClient(name));
        this._wordClients.set(name, id);
    }

    _dropWordClient(name) {
        const id = this._wordClients.get(name);
        if (id === undefined)
            return;
        this._wordClients.delete(name);
        Gio.bus_unwatch_name(id);
    }

    /** ListWords / ForgetWords for an allowed caller, passed to the engine
     * as is: its reply, or its error by name and message. Anyone else gets
     * AccessDenied. The proxy's interface info checks the reply's type. */
    _passWords(invocation, method) {
        if (!this._wordClients?.has(invocation.get_sender())) { // null after disable
            invocation.return_dbus_error(ACCESS_DENIED, 'not allowed');
            return;
        }
        const proxy = this._proxy;
        if (!proxy?.g_name_owner) {
            invocation.return_dbus_error('org.freedesktop.DBus.Error.ServiceUnknown',
                'the engine is not running');
            return;
        }
        proxy.call(method, invocation.get_parameters(), Gio.DBusCallFlags.NONE, CALL_TIMEOUT_MS,
            null, (p, res) => {
                let result;
                try {
                    result = p.call_finish(res);
                } catch (e) {
                    const name = e instanceof GLib.Error ? Gio.DBusError.get_remote_error(e) : null;
                    this._logOnce(`${method}:${name ?? 'local'}`, `${method} failed: ${name ?? 'local'}`);
                    if (name) {
                        Gio.DBusError.strip_remote_error(e);
                        invocation.return_dbus_error(name, e.message);
                    } else {
                        invocation.return_dbus_error('org.freedesktop.DBus.Error.Failed',
                            'the engine call failed');
                    }
                    return;
                }
                invocation.return_value(result);
            });
    }
}
