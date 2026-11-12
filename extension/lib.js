// Pure logic of the Correctiond companion extension: no Shell imports, so
// `gjs -m extension/test/lib.test.js` can test it outside gnome-shell.

/** The US engine: the OSK switch target when config.toml does not name one. */
export const QWERTY_ENGINE = 'correctiond:us';

/** gjs-osk's keyboard is a `Dialog` (ui/dialog.js) built with this style
 * class on its inner `_dialog` box, and it names its key grid `osk-gjs`. */
export const OSK_CONTENT_CLASS = 'db-keyboard-content';
export const OSK_BOX_NAME = 'osk-gjs';

export const PURGE_PARTIAL = 'org.nadelacruz.Correctiond1.Error.PurgePartial';
export const PURGE_PARTIAL_NOTICE =
    'Learned words deleted; the key will be removed when your keyring unlocks';
const ACCESS_DENIED = 'org.freedesktop.DBus.Error.AccessDenied';
/** The engine's PurgePartial text when the files themselves are still there
 * (engine/src/persist.rs FILES_FAILED). */
const ENGINE_FILES_FAILED = 'learned data files could not be removed';

/**
 * Whether to switch input source when the OSK shows. With per-window input
 * sources the Shell would save the switched source as the focused window's
 * own, so a focus change while the OSK is up could leave a window on it
 * afterwards: no switch then (the engine still gets SetOskVisible and uses
 * the OSK tap model).
 *
 * @param {string} target - the OSK source: an engine name, '' for none
 * @param {boolean} perWindow - org.gnome.desktop.input-sources per-window
 * @returns {'switch'|'off'|'per-window'}
 */
export function autoSwitchDecision(target, perWindow) {
    if (!target)
        return 'off';
    return perWindow ? 'per-window' : 'switch';
}

/**
 * The CLASS part of WM_CLASS (the Wayland app_id), lowercased; no window or
 * no class ⇒ '' (the engine treats empty as unidentified).
 *
 * @param {string|null|undefined} wmClass
 * @returns {string}
 */
export function normalizeWmClass(wmClass) {
    return typeof wmClass === 'string' ? wmClass.toLowerCase() : '';
}

/**
 * The app that receives typed text: none while a shell text entry has the
 * key focus (the overview search, a shell dialog), even though the last
 * window still counts as focused.
 *
 * @param {string|null|undefined} wmClass - the focus window's WM_CLASS
 * @param {boolean} shellEntryFocused
 * @returns {string}
 */
export function focusedApp(wmClass, shellEntryFocused) {
    return shellEntryFocused ? '' : normalizeWmClass(wmClass);
}

/**
 * Whether `actor` is gjs-osk's keyboard: one of its direct children carries
 * the keyboard's dialog style class (there from the moment the actor is
 * added to its parent) or the key grid's name (set a little later).
 *
 * @param {object|null} actor
 * @returns {boolean}
 */
export function isGjsOskActor(actor) {
    if (!actor || typeof actor.get_children !== 'function')
        return false;
    return actor.get_children().some(c =>
        !!c &&
        ((typeof c.has_style_class_name === 'function' &&
            c.has_style_class_name(OSK_CONTENT_CLASS)) ||
         (typeof c.get_name === 'function' && c.get_name() === OSK_BOX_NAME)));
}

/**
 * gjs-osk shows its keyboard with `show()` and hides it with `hide()` after
 * the close animation; it is on screen only while also mapped.
 *
 * @param {object|null} actor
 * @returns {boolean}
 */
export function oskShowing(actor) {
    return !!actor && !!actor.visible && !!actor.mapped;
}

/**
 * A stable key for an input source (the Shell recreates the objects when the
 * source list changes). '\n' cannot appear in a type or an engine id.
 *
 * @param {{type: string, id: string}|null} source
 * @returns {string|null}
 */
export function sourceKey(source) {
    return source ? `${source.type}\n${source.id}` : null;
}

/**
 * @param {object|null} inputSources - InputSourceManager.inputSources
 * @param {string|null} key - from sourceKey()
 * @returns {object|null} the source with that key
 */
export function findSource(inputSources, key) {
    if (!inputSources || !key)
        return null;
    return Object.values(inputSources).find(s => sourceKey(s) === key) ?? null;
}

/**
 * Bookkeeping for switching input source while the OSK is up and back
 * afterwards. Works on opaque keys (sourceKey()).
 */
export class SourceRestore {
    constructor() {
        this._prior = null;
        this._target = null;
    }

    /** True while a switch is waiting to be undone. */
    get pending() {
        return this._prior !== null;
    }

    /**
     * The OSK appeared. Returns the key to switch to, or null.
     *
     * @param {string|null} current
     * @param {string|null} target - null when the target is not an input source
     */
    shown(current, target) {
        if (this.pending || !target || current === target)
            return null;
        this._prior = current;
        this._target = target;
        return target;
    }

    /**
     * The OSK went away (or the extension is disabled). Returns the key to
     * restore, or null: nothing was switched, or the user has picked another
     * source since, which is kept.
     *
     * @param {string|null} current
     */
    hidden(current) {
        const prior = this._prior;
        const target = this._target;
        this._prior = null;
        this._target = null;
        return prior !== null && current === target ? prior : null;
    }
}

/** A boolean that reports whether a set changed it. */
export class Latch {
    constructor(initial = false) {
        this.value = initial;
    }

    set(value) {
        if (value === this.value)
            return false;
        this.value = value;
        return true;
    }
}

/**
 * The notification for a "Forget learned words" result.
 *
 * @param {{name: string|null, message: string|null}|null} error - null on success;
 *   `name` is the remote D-Bus error name (null for a local failure), `message`
 *   the remote text with the error-name prefix stripped
 * @returns {{title: string, body: string}}
 */
export function purgeNotice(error) {
    const title = 'Autocorrect';
    if (!error)
        return {title, body: 'Learned words forgotten.'};
    if (error.name === PURGE_PARTIAL) {
        if (error.message === ENGINE_FILES_FAILED)
            return {title, body: 'Some learned data could not be deleted. Try “Forget learned words” again.'};
        return {title, body: PURGE_PARTIAL_NOTICE};
    }
    if (error.name === ACCESS_DENIED)
        return {title, body: 'Could not forget learned words: the correction service refused the request.'};
    return {title, body: 'Could not forget learned words: the correction service is not running.'};
}

const T_CORRECT = 'Correcting in NAME';
const T_NO_REPLACE = 'Not correcting in NAME (this text field doesn\'t support it)';

/**
 * Menu texts per (app_mode, app_reason) pair, keyed `${mode}\t${reason}`.
 * NAME stands for the focused app's name. Hand-copied from
 * data/status-vectors.tsv; the extension test proves the two match.
 */
export const STATUS_ROWS = Object.freeze({
    'correct\tallowlisted': {appLine: T_CORRECT, subtitle: T_CORRECT},
    'learn_only\tnot_listed': {
        appLine: 'Not correcting in NAME (not in your app list)', subtitle: 'Learning only in NAME'},
    'learn_only\tbroken': {
        appLine: 'Not correcting in NAME (marked broken)', subtitle: 'Learning only in NAME'},
    'learn_only\tunidentified': {
        appLine: 'Not correcting here (app not identified)', subtitle: 'Learning only'},
    'learn_only\tno_replace': {appLine: T_NO_REPLACE, subtitle: 'Learning only in NAME'},
    'off\tprivate_field': {
        appLine: 'Off in this field (password or private)', subtitle: 'Off in this field'},
    'off\tterminal': {
        appLine: 'Off in this terminal (not in your app list, or marked broken)', subtitle: 'Off in this field'},
    'off\tno_replace': {appLine: T_NO_REPLACE, subtitle: 'Off in this field'},
    'none\t': {appLine: 'No text field yet', subtitle: 'Autocorrect'},
});

/**
 * Replace NAME in a status template. With no app name, " in NAME" is dropped.
 *
 * @param {string} template - text that may contain NAME
 * @param {string|null} appName - the focused app's name, or null if unknown
 * @returns {string}
 */
export function fillName(template, appName) {
    if (appName)
        return template.replaceAll('NAME', () => appName); // a function: no $& patterns
    return template.replaceAll(' in NAME', '');
}

/** statusText's `status` for an engine that predates GetStatus (the call
 * failed with UnknownMethod): it also lacks SetLearning, RecentWords and
 * ForgetWord. */
export const STATUS_UNSUPPORTED = 'unsupported';

/**
 * Texts for the Quick Settings status menu.
 *
 * @param {object|null} stats - parsed GetStats reply, or null when the engine is down
 * @param {object|string|null} status - parsed GetStatus reply; STATUS_UNSUPPORTED
 *   for an older engine without GetStatus; null when the engine is down
 * @param {string|null} appName - the focused app's name, or null if unknown
 * @returns {{subtitle: string, appLine: string, countsLine: string|null, controls: boolean}}
 *   `controls`: show the learning switch and the recent words (the engine is up
 *   and has GetStatus, SetLearning, RecentWords and ForgetWord)
 */
export function statusText(stats, status, appName) {
    if (!stats || !status) {
        return {subtitle: 'Engine not running', appLine: 'Engine not running', countsLine: null, controls: false};
    }
    const n = v => Number(v ?? 0).toLocaleString('en-US');
    const countsLine =
        `${n(stats.corrections)} corrections · ${n(stats.reverts)} undone · ${n(stats.words_learned)} words learned`;
    if (status === STATUS_UNSUPPORTED) {
        const none = STATUS_ROWS['none\t'];
        return {subtitle: none.subtitle, appLine: none.appLine, countsLine, controls: false};
    }
    if (status.enabled === false)
        return {subtitle: 'Off', appLine: 'Autocorrect is off', countsLine, controls: true};
    const row = STATUS_ROWS[`${status.app_mode}\t${status.app_reason ?? ''}`] ??
        STATUS_ROWS['none\t'];
    const appLine = fillName(row.appLine, appName);
    let subtitle = fillName(row.subtitle, appName);
    if (status.learning === false)
        subtitle = 'Learning paused';
    return {subtitle, appLine, countsLine, controls: true};
}

export const FORGET_FAILED = 'org.nadelacruz.Correctiond1.Error.ForgetFailed';

/**
 * The text a recent word's row shows for 3 s after its ✕ failed.
 *
 * @param {{name: string|null, message: string|null}} error - `name` is the remote
 *   D-Bus error name (null for a local failure), `message` the remote text with
 *   the error-name prefix stripped
 * @returns {string}
 */
export function forgetFailureText(error) {
    if (error?.name === FORGET_FAILED && error.message === 'unlock your keyring first')
        return 'Unlock your keyring first';
    return 'Could not forget';
}

export const SORTS = ['az', 'za', 'newest', 'oldest'];
export const SORT_LABELS = {az: 'A to Z', za: 'Z to A', newest: 'Newest first', oldest: 'Oldest first'};

const cmpText = (x, y) => (x < y ? -1 : x > y ? 1 : 0);
const cmpAz = (a, b) => cmpText(a.word.toLowerCase(), b.word.toLowerCase()) || cmpText(a.word, b.word);

/**
 * Comparator for the Words page rows.
 *
 * @param {{word: string, count: number, lastSeen: number, personal: boolean}} a
 * @param {{word: string, count: number, lastSeen: number, personal: boolean}} b
 * @param {string} sort - one of SORTS; unknown values sort like 'az'
 * @returns {number} negative, zero or positive
 */
export function compareWords(a, b, sort) {
    switch (sort) {
    case 'za':
        return cmpAz(b, a);
    case 'newest':
        return (b.lastSeen - a.lastSeen) || cmpAz(a, b);
    case 'oldest':
        return (a.lastSeen - b.lastSeen) || cmpAz(a, b);
    default:
        return cmpAz(a, b);
    }
}

/**
 * Literal, case-insensitive substring match (never a RegExp). An empty query
 * matches everything.
 *
 * @param {string} word
 * @param {string} query
 * @returns {boolean}
 */
export function matchesSearch(word, query) {
    return word.toLowerCase().includes(query.toLowerCase());
}

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/**
 * When a word was last used, by local calendar day.
 *
 * @param {number} lastSeen - unix seconds; 0 means never
 * @param {number} nowMs - current time in milliseconds
 * @returns {string} "today", "yesterday", "4 Mar", "31 Dec 2025" or "never"
 */
export function lastUsedText(lastSeen, nowMs) {
    if (!lastSeen)
        return 'never';
    const d = new Date(lastSeen * 1000);
    const now = new Date(nowMs);
    const day = x => Date.UTC(x.getFullYear(), x.getMonth(), x.getDate());
    const diff = Math.round((day(now) - day(d)) / 86400000);
    if (diff === 0)
        return 'today';
    if (diff === 1)
        return 'yesterday';
    const text = `${d.getDate()} ${MONTHS[d.getMonth()]}`;
    return d.getFullYear() === now.getFullYear() ? text : `${text} ${d.getFullYear()}`;
}

/** Words queued to be forgotten after an undo window. */
export class PendingForget {
    #words = new Set();

    /**
     * @param {string[]} words
     * @returns {number} the batch size after adding (duplicates ignored)
     */
    add(words) {
        for (const w of words)
            this.#words.add(w);
        return this.#words.size;
    }

    /** @returns {string[]} the queued words, cleared */
    undo() {
        return this.take();
    }

    /** @returns {string[]} the queued words, cleared (a second call returns []) */
    take() {
        const out = [...this.#words];
        this.#words.clear();
        return out;
    }

    get size() {
        return this.#words.size;
    }
}

// --- touch module and first-run guidance (pure; I/O lives in the callers) ---

export const GJS_OSK_UUID = 'gjsosk@vishram1123.com';

/**
 * The touch switch value: the explicit config value, else whether gjs-osk is installed.
 * @param {boolean|null} parsed
 * @param {boolean} installed
 * @returns {boolean}
 */
export function effectiveTouch(parsed, installed) {
    return parsed ?? installed;
}

/**
 * Touch value after (re)reading the config file.
 * @param {{kind: 'missing'}|{kind: 'invalid'}|{kind: 'ok', touch: boolean|null}} read
 * @param {boolean|null} current the value in force (null at enable())
 * @param {boolean} installed
 * @returns {boolean}
 */
export function touchAfterRead(read, current, installed) {
    if (read.kind === 'ok')
        return effectiveTouch(read.touch, installed);
    if (read.kind === 'invalid')
        return current ?? installed;
    return installed;
}

/**
 * @param {string[]} dataDirs
 * @param {(path: string) => boolean} isDir
 * @returns {boolean}
 */
export function gjsOskInstalledIn(dataDirs, isDir) {
    return dataDirs.some(d => isDir(`${d}/gnome-shell/extensions/${GJS_OSK_UUID}`));
}

/**
 * The directories IBus reads components from. When IBUS_COMPONENT_PATH is
 * set (even to an empty value), IBus reads only its entries and not the
 * system directory (ibus_registry_load in ibus src/ibusregistry.c).
 * Mirrors component_dirs in engine/src/doctor.rs.
 *
 * @param {string|null} ibusComponentPath colon-separated IBUS_COMPONENT_PATH
 * @returns {string[]}
 */
export function componentDirs(ibusComponentPath) {
    if (ibusComponentPath === null || ibusComponentPath === undefined)
        return ['/usr/share/ibus/component'];
    return ibusComponentPath.split(':').filter(e => e !== '');
}

/**
 * @param {string[]} dirs
 * @param {(path: string) => boolean} isFile
 * @returns {boolean}
 */
export function componentInstalledIn(dirs, isFile) {
    return dirs.some(d => isFile(`${d}/correctiond.xml`));
}

export const SETUP_WAIT_MS = 5000;

/**
 * First-run state, evaluated in order (spec 8.1).
 * @param {{componentInstalled: boolean, sourceConfigured: boolean, sourceCurrent: boolean,
 *   engineOwned: boolean, ownerWaitExpired: boolean}} p
 * @returns {null|'no-engine'|'no-source'|'paused'|'starting'|'failed'}
 */
export function setupState(p) {
    if (p.engineOwned)
        return null;
    if (!p.componentInstalled)
        return 'no-engine';
    if (!p.sourceConfigured)
        return 'no-source';
    if (!p.sourceCurrent)
        return 'paused';
    if (!p.ownerWaitExpired)
        return 'starting';
    return 'failed';
}

export const SETUP_TEXT = Object.freeze({
    quick: Object.freeze({
        'no-engine': 'Install the correctiond engine package',
        'no-source': "Add 'English (US, autocorrect)' as an input source. If it isn't listed, run `ibus restart` first",
        'paused': 'Paused: another input source is active (Super+Space to switch)',
        'starting': 'Starting…',
        'failed': "The autocorrect engine isn't reachable. Run `ibus restart`; if it still fails, see `journalctl --user -b | grep correctiond`",
    }),
    settings: Object.freeze({
        'no-engine': 'Install the correctiond engine package',
        'no-source': "Add 'English (US, autocorrect)' as an input source. If it isn't listed, press Restart below.",
        'paused': 'Paused: another input source is active (Super+Space to switch)',
        'starting': 'Starting…',
        'failed': "The autocorrect engine isn't reachable. Press Restart below; if it still fails, see `journalctl --user -b | grep correctiond`",
    }),
});

/**
 * Settings row extra line when setupState is null.
 * @param {string[]} appNames
 * @returns {string}
 */
export function correctsInLine(appNames) {
    return `Corrects in: ${appNames.join(', ')}. Add apps on the Apps page.`;
}

/**
 * @param {[string, string]} source an input-sources entry
 * @returns {boolean}
 */
export function isCorrectiondSource([type, id]) {
    return type === 'ibus' && id.startsWith('correctiond:');
}
