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
import {getInputSourceManager} from 'resource:///org/gnome/shell/ui/status/keyboard.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

import * as Lib from './lib.js';

const BUS_NAME = 'org.nadelacruz.Correctiond1';
const OBJECT_PATH = '/org/nadelacruz/Correctiond';
const OSK_UUID = 'gjsosk@vishram1123.com';
const ICON = 'tools-check-spelling-symbolic';
// gjs-osk shows a new keyboard for ~100 ms before its constructor's close()
// hides it, and a rebuild destroys and recreates an open keyboard: report
// the OSK state only once it has held this long.
const OSK_SETTLE_MS = 250;
const CALL_TIMEOUT_MS = 2000;
// Purge can wait behind an import, a flush or a keyring delete in the
// engine, so it does not use CALL_TIMEOUT_MS. This is GDBus's own default
// (25 s): on this proxy, -1 would mean its default timeout, CALL_TIMEOUT_MS.
const PURGE_TIMEOUT_MS = 25000;
const UNKNOWN_METHOD = 'org.freedesktop.DBus.Error.UnknownMethod';
// One GetStatus per focus change, not one per notify in a burst.
const SUBTITLE_DEBOUNCE_MS = 150;
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
        this._oskShown = new Lib.Latch(false);
        this._restore = new Lib.SourceRestore();
        this._ism = getInputSourceManager();
        this._inputSettings = new Gio.Settings({schema_id: 'org.gnome.desktop.input-sources'});

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

        this._subtitleId = 0;
        this._focusWindow = null;
        this._wmClassId = 0;
        this._focusId = global.display.connect('notify::focus-window', () => this._onFocus());
        this._keyFocusId = global.stage.connect('notify::key-focus', () => this._sendApp());
        this._onFocus();

        this._osk = null;
        this._oskIds = [];
        this._oskTimeoutId = 0;
        this._containers = this._oskContainers().map(actor => ({
            actor,
            ids: [
                actor.connect('child-added', (_a, child) => this._onChildAdded(child)),
                actor.connect('child-removed', (_a, child) => this._onChildRemoved(child)),
            ],
        }));
        this._extStateId = Main.extensionManager.connect('extension-state-changed', (_m, ext) => {
            if (ext?.uuid === OSK_UUID)
                this._scanOsk();
        });
        this._scanOsk();

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
        this._learnSwitch.disconnect(this._learnId);
        this._appItem = this._countsItem = this._learnSwitch = this._recentSection = null;
        this._learnSeparator = this._recentHeader = null;
        this._indicator.toggle.disconnect(this._toggleId);
        this._indicator.quickSettingsItems.forEach(item => item.destroy());
        this._indicator.destroy();
        this._indicator = null;

        Main.extensionManager.disconnect(this._extStateId);
        for (const {actor, ids} of this._containers)
            ids.forEach(id => actor.disconnect(id));
        this._containers = [];
        this._detachOsk();
        if (this._oskTimeoutId) {
            GLib.source_remove(this._oskTimeoutId);
            this._oskTimeoutId = 0;
        }
        try {
            this._restoreSource();
        } catch (e) {
            this._logOnce('input-source-restore', `input source restore failed: ${e.message}`);
        }

        global.display.disconnect(this._focusId);
        global.stage.disconnect(this._keyFocusId);
        this._setFocusWindow(null);

        // Without the extension the engine must see what "not running" means:
        // no app identified, no OSK. The on/off choice is left as it is.
        if (this._oskShown.value)
            this._send('SetOskVisible', false);
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
        this._ism = null;
        this._inputSettings = null;
        this._logged = null;
    }

    // --- D-Bus ---

    /** The engine (re)appeared: it starts unidentified, OSK hidden, enabled,
     * learning. The pause is not pushed (a restart learns again); the switch
     * follows GetStatus. Also runs when it goes away, for the subtitle. */
    _onOwner() {
        this._queueSubtitle();
        if (!this._proxy?.g_name_owner)
            return;
        this._logged.clear();
        this._app = new Lib.Latch(null);
        this._sendApp();
        this._send('SetOskVisible', this._oskShown.value);
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

    // --- GJS OSK ---

    /** Where gjs-osk puts its keyboard: Main.uiGroup (Shell 50), or the
     * input panel group, which it moves to on Shell 51. */
    _oskContainers() {
        const containers = [Main.layoutManager.uiGroup];
        const panelGroup = Main.layoutManager.keyboardBox?.get_parent();
        if (panelGroup && !containers.includes(panelGroup))
            containers.push(panelGroup);
        return containers;
    }

    _scanOsk() {
        let found = null;
        for (const {actor} of this._containers) {
            found = actor.get_children().find(c => Lib.isGjsOskActor(c)) ?? null;
            if (found)
                break;
        }
        if (found !== this._osk) {
            this._detachOsk();
            if (found)
                this._attachOsk(found);
        }
        this._queueOskUpdate();
    }

    _onChildAdded(child) {
        if (child !== this._osk && Lib.isGjsOskActor(child)) {
            this._detachOsk();
            this._attachOsk(child);
            this._queueOskUpdate();
        }
    }

    _onChildRemoved(child) {
        if (child === this._osk)
            this._scanOsk(); // a move to another container, or gone
    }

    _attachOsk(actor) {
        this._osk = actor;
        this._oskIds = [
            actor.connect('notify::visible', () => this._queueOskUpdate()),
            actor.connect('notify::mapped', () => this._queueOskUpdate()),
            actor.connect('destroy', () => {
                this._detachOsk();
                this._queueOskUpdate();
            }),
        ];
    }

    _detachOsk() {
        if (this._osk)
            this._oskIds.forEach(id => this._osk.disconnect(id));
        this._osk = null;
        this._oskIds = [];
    }

    _queueOskUpdate() {
        if (this._oskTimeoutId)
            GLib.source_remove(this._oskTimeoutId);
        this._oskTimeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, OSK_SETTLE_MS, () => {
            this._oskTimeoutId = 0;
            this._updateOsk();
            return GLib.SOURCE_REMOVE;
        });
    }

    _updateOsk() {
        const shown = Lib.oskShowing(this._osk);
        if (!this._oskShown.set(shown))
            return;
        this._send('SetOskVisible', shown);
        try {
            if (shown)
                this._switchForOsk();
            else
                this._restoreSource();
        } catch (e) {
            this._logOnce('input-source', `input source switch failed: ${e.message}`);
        }
    }

    // --- input source auto-switch ---

    /** The engine to use while the OSK is up ('' = none), read each time. */
    _oskSource() {
        const path = GLib.build_filenamev([GLib.get_home_dir(), '.config', 'correctiond', 'config.toml']);
        try {
            const [, bytes] = GLib.file_get_contents(path);
            return Lib.parseOskSource(new TextDecoder().decode(bytes));
        } catch (e) {
            return Lib.parseOskSource(null); // missing or unreadable: the default
        }
    }

    _switchForOsk() {
        // Read now: both can change at runtime.
        const engine = this._oskSource();
        const decision = Lib.autoSwitchDecision(engine, this._inputSettings.get_boolean('per-window'));
        if (decision === 'per-window')
            this._logOnce('per-window', `per-window input sources are on; not switching to ${engine} ` +
                'while the OSK is up');
        if (decision !== 'switch')
            return;
        const sources = this._ism.inputSources;
        const target = Lib.findSource(sources, Lib.sourceKey({type: 'ibus', id: engine}));
        if (!target) {
            this._logOnce(`missing:${engine}`, `${engine} is not an input source; not switching`);
            return;
        }
        if (this._restore.shown(Lib.sourceKey(this._ism.currentSource), Lib.sourceKey(target)))
            target.activate(false);
    }

    _restoreSource() {
        if (!this._restore.pending)
            return;
        const key = this._restore.hidden(Lib.sourceKey(this._ism.currentSource));
        Lib.findSource(this._ism.inputSources, key)?.activate(false);
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
        menu.addAction('Autocorrect Settings…', () => this.openPreferences());

        this._menuOpenId = menu.connect('open-state-changed', (_m, open) => {
            if (open)
                this._refreshMenu();
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
