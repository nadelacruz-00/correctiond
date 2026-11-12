// GJS OSK support of the Correctiond companion: watches gjs-osk's keyboard
// and reports whether it is up, and switches the input source while it is.
// extension.js builds an OskWatcher only when touch support is on, so with
// it off no OSK handler is connected.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {getInputSourceManager} from 'resource:///org/gnome/shell/ui/status/keyboard.js';

import * as Lib from './lib.js';

const OSK_UUID = Lib.GJS_OSK_UUID;
// gjs-osk shows a new keyboard for ~100 ms before its constructor's close()
// hides it, and a rebuild destroys and recreates an open keyboard: report
// the OSK state only once it has held this long.
const OSK_SETTLE_MS = 250;

export class OskWatcher {
    /**
     * @param {object} opts
     * @param {(shown: boolean) => void} opts.onChange the settled OSK state changed
     * @param {(key: string, msg: string) => void} opts.logOnce
     * @param {() => string} opts.readOskSource the engine to use while the
     *   OSK is up ('' = none), read each time
     */
    constructor({onChange, logOnce, readOskSource}) {
        this._onChange = onChange;
        this._logOnce = logOnce;
        this._readOskSource = readOskSource;
        this._shown = new Lib.Latch(false);
        this._restore = new Lib.SourceRestore();
        this._ism = null;
        this._inputSettings = null;
        this._actor = null;
        this._actorIds = [];
        this._timeoutId = 0;
        this._containers = [];
        this._extStateId = 0;
    }

    /** The settled OSK state, for the engine re-push. */
    get shown() {
        return this._shown.value;
    }

    enable() {
        this._ism = getInputSourceManager();
        this._inputSettings = new Gio.Settings({schema_id: 'org.gnome.desktop.input-sources'});
        this._containers = this._containerActors().map(actor => ({
            actor,
            ids: [
                actor.connect('child-added', (_a, child) => this._onChildAdded(child)),
                actor.connect('child-removed', (_a, child) => this._onChildRemoved(child)),
            ],
        }));
        this._extStateId = Main.extensionManager.connect('extension-state-changed', (_m, ext) => {
            if (ext?.uuid === OSK_UUID)
                this._scan();
        });
        this._scan();
    }

    /** Disconnects everything, removes the timer and restores a switched
     * input source.
     * @returns {boolean} whether the OSK was shown */
    disable() {
        if (this._extStateId) {
            Main.extensionManager.disconnect(this._extStateId);
            this._extStateId = 0;
        }
        for (const {actor, ids} of this._containers)
            ids.forEach(id => actor.disconnect(id));
        this._containers = [];
        this._detach();
        if (this._timeoutId) {
            GLib.source_remove(this._timeoutId);
            this._timeoutId = 0;
        }
        try {
            this._restoreSource();
        } catch (e) {
            this._logOnce('input-source-restore', `input source restore failed: ${e.message}`);
        }
        this._ism = null;
        this._inputSettings = null;
        return this._shown.value;
    }

    /** Where gjs-osk puts its keyboard: Main.uiGroup (Shell 50), or the
     * input panel group, which it moves to on Shell 51. */
    _containerActors() {
        const containers = [Main.layoutManager.uiGroup];
        const panelGroup = Main.layoutManager.keyboardBox?.get_parent();
        if (panelGroup && !containers.includes(panelGroup))
            containers.push(panelGroup);
        return containers;
    }

    _scan() {
        let found = null;
        for (const {actor} of this._containers) {
            found = actor.get_children().find(c => Lib.isGjsOskActor(c)) ?? null;
            if (found)
                break;
        }
        if (found !== this._actor) {
            this._detach();
            if (found)
                this._attach(found);
        }
        this._queueUpdate();
    }

    _onChildAdded(child) {
        if (child !== this._actor && Lib.isGjsOskActor(child)) {
            this._detach();
            this._attach(child);
            this._queueUpdate();
        }
    }

    _onChildRemoved(child) {
        if (child === this._actor)
            this._scan(); // a move to another container, or gone
    }

    _attach(actor) {
        this._actor = actor;
        this._actorIds = [
            actor.connect('notify::visible', () => this._queueUpdate()),
            actor.connect('notify::mapped', () => this._queueUpdate()),
            actor.connect('destroy', () => {
                this._detach();
                this._queueUpdate();
            }),
        ];
    }

    _detach() {
        if (this._actor)
            this._actorIds.forEach(id => this._actor.disconnect(id));
        this._actor = null;
        this._actorIds = [];
    }

    _queueUpdate() {
        if (this._timeoutId)
            GLib.source_remove(this._timeoutId);
        this._timeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, OSK_SETTLE_MS, () => {
            this._timeoutId = 0;
            this._update();
            return GLib.SOURCE_REMOVE;
        });
    }

    _update() {
        const shown = Lib.oskShowing(this._actor);
        if (!this._shown.set(shown))
            return;
        this._onChange(shown);
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

    _switchForOsk() {
        // Read now: both can change at runtime.
        const engine = this._readOskSource();
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
}
