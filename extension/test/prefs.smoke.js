// Smoke test for extension/prefsWindow.js: builds the real preferences window
// (never shown) against a temporary config file and drives its controller.
// Needs a display and libadwaita >= 1.7 (page banners, ButtonRow), so it runs
// on the desktop, not in CI. Run: gjs -m extension/test/prefs.smoke.js
// The only outside call is a read-only GetStats on the session bus.

import Adw from 'gi://Adw?version=1';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Gtk from 'gi://Gtk?version=4.0';
import System from 'system';

import {fillWindow, fmtRate, lexiconStatus, loadReport} from '../prefsWindow.js';
import {parseConfig} from '../config.js';
import {SORTS} from '../lib.js';
import {rowSubtitle} from '../wordsPage.js';

Adw.init();

let failures = 0;
let passed = 0;

function check(cond, what) {
    if (cond) {
        passed++;
    } else {
        failures++;
        printerr(`FAIL ${what}`);
    }
}

const dir = GLib.dir_make_tmp('correctiond-prefs-XXXXXX');
const read = path => {
    const [, bytes] = GLib.file_get_contents(path);
    return new TextDecoder().decode(bytes);
};
const config = path => parseConfig(read(path)).config;
const mode = path => Gio.File.new_for_path(path)
    .query_info('unix::mode', Gio.FileQueryInfoFlags.NONE, null)
    .get_attribute_uint32('unix::mode') & 0o777;
const apps = [{id: 'org.gnome.TextEditor', name: 'Text Editor & Notes', icon: null}];
const newWindow = () => new Adw.PreferencesWindow();

// An editable file in a directory that does not exist yet.
const path = GLib.build_filenamev([dir, 'correctiond', 'config.toml']);
let ctl = fillWindow(newWindow(), {configPath: path, restartArgv: ['true'], apps});
check(!ctl.readOnly, 'missing file is editable');
check(ctl.addApp('allowlist', 'org.gnome.TextEditor'), 'add app');
check(config(path).allowlist.join() === 'firefox,org.gnome.TextEditor', 'allowlist written');
check(mode(path) === 0o600, `file mode ${mode(path).toString(8)}`);
check(mode(GLib.path_get_dirname(path)) === 0o700, 'dir mode 0700');
check(ctl.banners.length === 3 && ctl.banners.every(b => b.revealed), 'restart banners shown');
check(!ctl.addApp('allowlist', 'ORG.GNOME.TEXTEDITOR'), 'duplicate refused');
check(!ctl.addApp('allowlist', 'two words'), 'bad id refused');
check(ctl.removeApp('allowlist', 'firefox'), 'remove app');
check(config(path).allowlist.join() === 'org.gnome.TextEditor', 'removal written');
check(ctl.addApp('broken', 'org.example.Broken'), 'add broken');
check(config(path).broken.join() === 'org.example.Broken', 'broken written');
check(ctl.oskRow.selected === 1, 'OSK layout shows the default (QWERTY)');
ctl.oskRow.selected = 0; // the widget path: pick Colemak
check(config(path).oskSource === 'correctiond:colemak', 'OSK layout written from the dropdown');
check(ctl.setOskSource(''), 'no switch');
check(config(path).oskSource === '', 'no switch written');
check(ctl.addLanguage('ceb', '/nonexistent/ceb.txt'), 'add language');
check(!ctl.addLanguage('ceb', '/x'), 'duplicate language refused');
check(!ctl.addLanguage('CEB!', '/x'), 'bad code refused');
check(config(path).languages.map(l => l.code).join() === 'en,tl,war,ceb', 'languages written');
check(ctl.removeLanguage('ceb'), 'remove language');
check(config(path).languages.length === 3, 'language removal written');

// The restart action runs the command and hides the banners.
const loop = new GLib.MainLoop(null, false);
ctl.restartEngine();
GLib.timeout_add(GLib.PRIORITY_DEFAULT, 1000, () => {
    loop.quit();
    return GLib.SOURCE_REMOVE;
});
loop.run();
check(ctl.banners.every(b => !b.revealed), 'banners hidden after restart');

// A file the window cannot fully read is never rewritten.
const odd = GLib.build_filenamev([dir, 'odd.toml']);
GLib.file_set_contents(odd, 'allowlist = ["firefox"]\ntypo_key = true\n');
ctl = fillWindow(newWindow(), {configPath: odd, restartArgv: ['true'], apps});
check(ctl.readOnly && ctl.error.includes('typo_key'), 'unknown key makes it read-only');
check(!ctl.addApp('allowlist', 'zen'), 'read-only refuses edits');
check(read(odd) === 'allowlist = ["firefox"]\ntypo_key = true\n', 'read-only file untouched');

// Lexicon status.
check(lexiconStatus('builtin:en').ok, 'builtin ok');
check(!lexiconStatus('/nonexistent/x.txt').ok, 'missing lexicon flagged');
const lex = GLib.build_filenamev([dir, 'x.txt']);
GLib.file_set_contents(lex, '# lexicon lang=x entries=2\nab\t3\ncd\t1\n');
check(lexiconStatus(lex).text === '2 words', `lexicon count: ${lexiconStatus(lex).text}`);

// Learning page: missing, unreadable, valid and stale reports (synthetic counts).
const reportPath = GLib.build_filenamev([dir, 'import-report.json']);
const learn = (extra = {}) => fillWindow(newWindow(), {
    configPath: path, restartArgv: ['true'], apps, reportPath, layout: 'qwerty', ...extra,
});
const val = (c, title) => c.learningRows.get(title)?.subtitle;
check(fmtRate(0.04219) === '4.2%' && fmtRate(null) === 'n/a' && fmtRate(undefined) === 'n/a', 'rate format');
check(loadReport(reportPath).state === 'missing', 'no report is missing');

ctl = learn();
check(ctl.learningRows.size === 1 && ctl.learningRows.has('No chat import yet'), 'missing report note');
check(ctl.banners.length === 3, 'Learning page adds no restart banner');

GLib.file_set_contents(reportPath, '{not json');
check(loadReport(reportPath).state === 'invalid', 'broken JSON is invalid');
GLib.file_set_contents(reportPath, '[1,2]');
check(loadReport(reportPath).state === 'invalid', 'non-object JSON is invalid');
ctl = learn();
check(ctl.learningRows.has('The import report could not be read'), 'unreadable report note');
check(!ctl.learningRows.has('Imported'), 'no numbers for an unreadable report');

const mkReport = over => JSON.stringify({
    imported_at: '2026-11-01T10:00:00Z', own_messages: 12345, threads: 67,
    words: 900, pairs: 4000, names_skipped: 31,
    gate: 'failed', failed_checks: ['chat_rate', 'typo_share'], lists_installed: false,
    tl_added: 50, war_added: 60,
    replay: {
        qwerty: {full: {
            baseline: {chat_changed_rate: 0.12, typo_fix_rate: 0.5, slang_changed_rate: 0.2},
            rebuild: {chat_changed_rate: 0.04219, typo_fix_rate: 0.9, slang_changed_rate: null},
        }},
        colemak: {full: {
            baseline: {chat_changed_rate: 0.3, typo_fix_rate: 0.4},
            rebuild: {chat_changed_rate: 0.25, typo_fix_rate: 0.8},
        }},
    },
    ...over,
});
GLib.file_set_contents(reportPath, mkReport({}));
ctl = learn();
check(val(ctl, 'Your messages') === '12,345' && val(ctl, 'Threads') === '67', 'chat counts');
check(/^2026-11-0[12]$/.test(val(ctl, 'Imported')), `imported date ${val(ctl, 'Imported')}`);
check(val(ctl, 'Waray and Taglish words changed') === '4.2% (before: 12.0%)', 'CHAT rate with baseline');
check(val(ctl, 'Typos fixed') === '90.0% (before: 50.0%)', 'typo fix rate with baseline');
check(val(ctl, 'Slang changed') === 'n/a (before: 20.0%)', 'null rate shows n/a');
check(val(ctl, 'Quality check') === 'Failed: chat_rate, typo_share', 'failed gate and checks');
check(val(ctl, 'Words imported') === '900' && val(ctl, 'Pairs imported') === '4,000', 'words and pairs');
check(val(ctl, 'Tagalog words added to the list') === '0' && val(ctl, 'Waray words added to the list') === '0',
    'list additions are 0 when not installed');
check(ctl.learningRows.has('Word lists were not installed'), 'not-installed note');
check(val(ctl, 'Names skipped') === '31', 'names skipped');
check(!ctl.learningRows.has('Learned words were forgotten after this import'), 'not stale without stats');
ctl.setWordsLearned(900);
check(!ctl.learningRows.has('Learned words were forgotten after this import'), 'not stale at equal words');

ctl.setWordsLearned(10);
check(ctl.learningRows.has('Learned words were forgotten after this import'), 'stale note when words are fewer');
ctl.setWordsLearned(null);
check(!ctl.learningRows.has('Learned words were forgotten after this import'), 'no stale note when engine is down');

// The layout picks the replay numbers; installed lists show their additions; a passed gate.
GLib.file_set_contents(reportPath, mkReport({gate: 'passed', failed_checks: [], lists_installed: true}));
ctl = learn({layout: 'colemak'});
check(val(ctl, 'Waray and Taglish words changed') === '25.0% (before: 30.0%)', 'colemak numbers');
check(val(ctl, 'Quality check') === 'Passed', 'passed gate');
check(val(ctl, 'Tagalog words added to the list') === '50' && val(ctl, 'Waray words added to the list') === '60',
    'list additions when installed');
check(!ctl.learningRows.has('Word lists were not installed'), 'no not-installed note');
ctl = learn({layout: 'osk'});
check(val(ctl, 'Waray and Taglish words changed') === 'n/a (before: n/a)', 'absent layout shows n/a');

// Forget all: the row calls ConfirmForget through the injected stub (never the session bus).
{
    let calls = 0;
    ctl = learn({companionCall: cb => { calls++; cb(null); }});
    check(ctl.forgetRow?.title === 'Forget all learned words\u2026', 'forget row title');
    ctl.forgetAll();
    check(calls === 1, 'forget row calls ConfirmForget');
    check(ctl.forgetRow.subtitle === '', 'no message after a good call');
    ctl = learn({companionCall: () => { throw new Error('no such object'); }});
    ctl.forgetAll();
    check(ctl.forgetRow.subtitle === 'Turn on the Autocorrect extension to use this',
        'forget row shows the disabled message when the call fails');
    ctl = learn({companionCall: cb => cb(new Error('ServiceUnknown'))});
    ctl.forgetAll();
    check(ctl.forgetRow.subtitle === 'Turn on the Autocorrect extension to use this',
        'forget row shows the message when the reply is an error');
    // It survives the report rebuild (a words-learned update).
    ctl.setWordsLearned(5);
    check(ctl.forgetRow.get_parent() !== null, 'forget row stays on the page');

    // One controller: a failure shows the message, the next good call clears it.
    let fail = true;
    ctl = learn({companionCall: cb => cb(fail ? new Error('ServiceUnknown') : null)});
    ctl.forgetAll();
    check(ctl.forgetRow.subtitle === 'Turn on the Autocorrect extension to use this',
        'failure then success: the failure shows the message');
    fail = false;
    ctl.forgetAll();
    check(ctl.forgetRow.subtitle === '', 'failure then success: the good call clears the message');
}

// The forget group is the Learning page's last group, also after a rebuild.
{
    const group = w => w?.get_ancestor(Adw.PreferencesGroup.$gtype);
    const isLast = c => {
        const forget = group(c.forgetRow);
        const report = group(c.learningRows.get('Names skipped'));
        return forget !== null && report !== null && forget.get_parent() === report.get_parent() &&
            forget.get_next_sibling() === null;
    };
    ctl = learn({companionCall: cb => cb(null)});
    check(isLast(ctl), 'forget group is the last group on the Learning page');
    ctl.setWordsLearned(10);
    check(isLast(ctl), 'forget group is still last after a rebuild');
    GLib.file_set_contents(reportPath, '{not json');
    ctl.setWordsLearned(null);
    const g = group(ctl.forgetRow);
    const note = group(ctl.learningRows.get('The import report could not be read'));
    check(g.get_parent() === note.get_parent() && g.get_next_sibling() === null,
        'forget group is last under an unreadable report');
}

// --- Words page: always a stub wordsCalls, never a bus ---

// Runs the main loop for `ms`, so stub promises settle and timers fire.
const spin = ms => {
    const l = new GLib.MainLoop(null, false);
    GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
        l.quit();
        return GLib.SOURCE_REMOVE;
    });
    l.run();
};
const NOW_S = Math.floor(Date.now() / 1000);
const stubRows = () => [
    {word: 'kadto', count: 41, lastSeen: NOW_S, personal: true},
    {word: 'sige', count: 12, lastSeen: NOW_S - 3 * 86400, personal: true},
    {word: 'Hello', count: 5, lastSeen: 0, personal: false},
];
// A wordsCalls stub that records its calls; `over(s)` replaces methods.
const stubCalls = (over = () => ({})) => {
    const s = {
        opens: 0, requests: 0, lists: 0, forgets: [], flushes: 0, closes: 0, probes: 0, order: [],
        conn: {stub: true},
        open: () => {
            s.opens++;
            return s.conn;
        },
        requestAccess: conn => {
            s.requests++;
            return Promise.resolve(conn === s.conn);
        },
        listWords: conn => {
            s.lists++;
            return conn === s.conn ? Promise.resolve(stubRows()) : Promise.reject(new Error('wrong conn'));
        },
        forgetWords: (conn, words) => {
            s.forgets.push([...words]);
            s.order.push('forget');
            return Promise.resolve(words.length);
        },
        flush: conn => {
            if (conn === s.conn)
                s.flushes++;
            s.order.push('flush');
            return Promise.resolve();
        },
        close: conn => {
            if (conn === s.conn)
                s.closes++;
            s.order.push('close');
        },
        companionPresent: () => {
            s.probes++;
            return Promise.resolve(true);
        },
    };
    Object.assign(s, over(s));
    return s;
};
// A window like GNOME's ExtensionPrefsDialog (extensionPrefsDialog.js): its
// destroy() shows an error page instead, and the service connects its own
// close-request handler (it releases its hold and returns false) before the
// extension fills the window. Here destroy() and close() only count.
const PrefsDialogLike = GObject.registerClass(class PrefsDialogLike extends Adw.PreferencesWindow {
    destroy() {
        this.destroyCalls++;
    }

    close() {
        this.closeCalls++;
    }
});
const dialogLike = () => {
    const win = new PrefsDialogLike();
    win.destroyCalls = 0;
    win.closeCalls = 0;
    win.releases = 0;
    win.connect('close-request', () => {
        win.releases++;
        return false;
    });
    return win;
};
const wordsWin = (calls, makeWindow = newWindow) => {
    const win = makeWindow();
    const c = fillWindow(win, {configPath: path, restartArgv: ['true'], apps, reportPath, layout: 'qwerty',
        companionCall: cb => cb(null), wordsCalls: calls});
    return {win, w: c.words};
};
const shown = w => {
    const out = [];
    for (let i = 0; i < w.listModel.get_n_items(); i++)
        out.push(w.listModel.get_item(i).string);
    return out;
};
const search = (w, text) => {
    w.searchEntry.text = text;
    w.searchEntry.emit('search-changed');
};
const remote = (name, message) => Gio.DBusError.new_for_dbus_error(name, message);
const FORGET_FAILED = 'org.nadelacruz.Correctiond1.Error.ForgetFailed';
const ACCESS_DENIED = 'org.freedesktop.DBus.Error.AccessDenied';
const UNKNOWN_METHOD = 'org.freedesktop.DBus.Error.UnknownMethod';
const loaded = (calls, makeWindow) => {
    const r = wordsWin(calls, makeWindow);
    r.w.requestAccess();
    spin(50);
    return r;
};

// States.
{
    const s = stubCalls();
    const {w} = wordsWin(s);
    check(w !== undefined, 'fillWindow returns the words controller');
    check(w?.showButton.get_ancestor(Adw.PreferencesPage.$gtype)?.title === 'Words', 'Words page');
    check(w?.statusLabel.label === 'Your learned words are hidden until you allow it.', 'hidden text');
    check(w?.showButton.label === 'Show learned words…', 'show button label');
    const listGroup = w?.searchEntry.get_ancestor(Adw.PreferencesGroup.$gtype);
    check(w?.listModel.get_n_items() === 0 && listGroup?.visible === false, 'no list while hidden');
    check(s.opens === 0 && s.requests === 0 && s.lists === 0, 'nothing called before the button');
}
{
    const s = stubCalls(() => ({requestAccess: () => Promise.resolve(false)}));
    const {w} = wordsWin(s);
    w.showButton.emit('clicked');
    spin(50);
    check(w.statusLabel.label === 'Not allowed', `refused: ${w.statusLabel.label}`);
    check(s.lists === 0 && w.listModel.get_n_items() === 0, 'refused: no list');
}
{
    const s = stubCalls(() => ({requestAccess: () => Promise.reject(remote('org.freedesktop.DBus.Error.UnknownObject', 'x'))}));
    const {w} = wordsWin(s);
    w.requestAccess();
    spin(50);
    check(w.statusLabel.label === 'Turn on the Autocorrect extension to use this', `no companion: ${w.statusLabel.label}`);
}
{
    let fail = true;
    const s = stubCalls(t => ({
        listWords: () => {
            t.lists++;
            return fail ? Promise.reject(remote('org.freedesktop.DBus.Error.ServiceUnknown', 'down'))
                : Promise.resolve(stubRows());
        },
    }));
    const {w} = wordsWin(s);
    w.requestAccess();
    spin(50);
    check(w.statusLabel.label === 'Couldn\'t load your learned words', `load failed: ${w.statusLabel.label}`);
    check(w.showButton.label === 'Retry', 'Retry button');
    check(w.listModel.get_n_items() === 0, 'no rows after a failed load');
    fail = false;
    w.showButton.emit('clicked');
    spin(50);
    check(s.requests === 2, `Retry asks for access again (${s.requests})`);
    check(w.listModel.get_n_items() === 2, 'Retry loads the list');
    check(s.opens === 1, 'one private connection');
}
{
    const s = stubCalls(t => ({
        listWords: () => {
            t.lists++;
            return Promise.resolve([]);
        },
    }));
    const {w} = wordsWin(s);
    w.requestAccess();
    spin(50);
    check(w.countLabel.label === '0 words', `empty store: ${w.countLabel.label}`);
    check(w.statusLabel.get_ancestor(Adw.PreferencesGroup.$gtype)?.visible === false, 'empty store is no error');
}

// Access lost on ListWords (the screen lock disables the extension, which
// clears the allowed set): back to the hidden state, ready to ask again.
const hiddenNow = w => w.statusLabel.get_ancestor(Adw.PreferencesGroup.$gtype)?.visible === true &&
    w.searchEntry.get_ancestor(Adw.PreferencesGroup.$gtype)?.visible === false;
for (const name of [ACCESS_DENIED, 'org.freedesktop.DBus.Error.UnknownObject']) {
    let deny = false;
    const s = stubCalls(t => ({
        listWords: () => {
            t.lists++;
            return deny ? Promise.reject(remote(name, 'not allowed')) : Promise.resolve(stubRows());
        },
    }));
    const {w} = loaded(s);
    check(shown(w).length === 2, `${name}: loaded first`);
    deny = true;
    w.reload();
    spin(50);
    check(hiddenNow(w) && w.statusLabel.label === 'Not allowed', `${name} on ListWords: ${w.statusLabel.label}`);
    check(w.showButton.visible && w.showButton.label === 'Show learned words…',
        `${name} on ListWords: the Show button (${w.showButton.label})`);
    deny = false;
    w.showButton.emit('clicked');
    spin(50);
    check(s.requests === 2 && shown(w).length === 2 && !hiddenNow(w), `${name}: Show asks again and loads`);
}

// An engine from before ListWords: the companion passes its UnknownMethod
// through. Say so, with no Retry (it would prompt again for nothing).
{
    const s = stubCalls(t => ({
        listWords: () => {
            t.lists++;
            return Promise.reject(remote(UNKNOWN_METHOD, 'Unknown method \'ListWords\''));
        },
    }));
    const {w} = loaded(s);
    check(w.statusLabel.label === 'Update correctiond to use this', `old engine: ${w.statusLabel.label}`);
    check(hiddenNow(w) && !w.showButton.visible, 'old engine: no list and no Retry');
    check(s.probes === 1, `old engine: the companion is probed once (${s.probes})`);
}
// UnknownMethod with no companion object (GDBus says so for an unexported
// path, as after the extension is turned off): not the engine's fault.
{
    const s = stubCalls(t => ({
        listWords: () => {
            t.lists++;
            return Promise.reject(remote(UNKNOWN_METHOD, 'Object does not exist at path'));
        },
        companionPresent: () => {
            t.probes++;
            return Promise.resolve(false);
        },
    }));
    const {w} = loaded(s);
    check(w.statusLabel.label === 'Turn on the Autocorrect extension to use this',
        `companion gone: ${w.statusLabel.label}`);
    check(hiddenNow(w) && w.showButton.visible && w.showButton.label === 'Show learned words…',
        'companion gone: the Show button');
}

// Loaded with the 3 stub rows.
{
    const s = stubCalls();
    const {w} = loaded(s);
    check(shown(w).length === 2 && !shown(w).includes('Hello'), `personal rows only: ${shown(w)}`);
    check(w.countLabel.label === '2 words', `count: ${w.countLabel.label}`);
    check(w.sortRow.selected === SORTS.indexOf('newest'), 'default sort is Newest first');
    w.allSwitch.active = true;
    check(shown(w).length === 3 && w.countLabel.label === '3 words', 'switch on shows all rows');
    check(s.lists === 1, 'switch makes no second ListWords');
    search(w, 'AD');
    check(shown(w).join() === 'kadto' && w.countLabel.label === '1 word', `search AD: ${shown(w)}`);
    search(w, '');
    const orders = {az: 'Hello,kadto,sige', za: 'sige,kadto,Hello', newest: 'kadto,sige,Hello', oldest: 'Hello,sige,kadto'};
    for (const sort of SORTS) {
        w.sortRow.selected = SORTS.indexOf(sort);
        check(shown(w).join() === orders[sort], `sort ${sort}: ${shown(w)}`);
    }
    w.reload();
    spin(50);
    check(s.lists === 2 && shown(w).length === 3, 'reload calls ListWords again');
    check(rowSubtitle(stubRows()[0], Date.now()) === 'used 41 times · last used today', 'row subtitle');
    check(rowSubtitle(stubRows()[2], Date.now()) === 'used 5 times · last used never', 'row subtitle never');
}

// Delete with Undo, the 5 s batch and a locked keyring: one 5.5 s main loop for all three.
const undoS = stubCalls();
const undo = loaded(undoS).w;
const batchS = stubCalls();
const batch = loaded(batchS).w;
const lockedS = stubCalls(t => ({
    forgetWords: (_c, words) => {
        t.forgets.push([...words]);
        return Promise.reject(remote(FORGET_FAILED, 'unlock your keyring first'));
    },
}));
const locked = loaded(lockedS).w;
const deniedS = stubCalls(t => ({
    forgetWords: (_c, words) => {
        t.forgets.push([...words]);
        return Promise.reject(remote(ACCESS_DENIED, 'not allowed'));
    },
}));
const denied = loaded(deniedS).w;
denied.forgetWord('kadto');
denied.forgetWord('sige');
check(shown(denied).length === 0, 'denied: both rows pending');
for (const w of [undo, batch])
    w.allSwitch.active = true;
batch.forgetWord('kadto');
check(batch.toast?.title === 'Forgot kadto' && batch.toast?.button_label === 'Undo', 'toast Forgot kadto');
check(batch.toast?.timeout === 0, 'batch toast has no timeout');
check(!shown(batch).includes('kadto'), 'deleted row leaves the list');
check(batchS.forgets.length === 0, 'no ForgetWords yet');
const firstToast = batch.toast;
batch.forgetWord('sige');
batch.forgetWord('Hello');
check(batch.toast === firstToast && batch.toast.title === 'Forgot 3 words', `one toast: ${batch.toast?.title}`);
check(shown(batch).length === 0 && batch.countLabel.label === '0 words', 'all three rows gone');
for (const word of ['kadto', 'sige', 'Hello'])
    undo.forgetWord(word);
undo.toast.emit('button-clicked');
check(shown(undo).length === 3, 'Undo restores all 3');
check(undo.toast === null, 'Undo dismisses the toast');
locked.forgetWord('kadto');
spin(5500);
check(batchS.forgets.length === 1 && batchS.forgets[0].slice().sort().join() === 'Hello,kadto,sige',
    `one ForgetWords with 3 words: ${JSON.stringify(batchS.forgets.map(f => f.length))}`);
check(batch.toast === null && shown(batch).length === 0, 'batch sent, toast gone');
check(undoS.forgets.length === 0, 'Undo sends nothing');
check(lockedS.forgets.length === 1 && shown(locked).includes('kadto'), 'locked: the row comes back');
check(locked.lastToast?.title === 'Unlock your keyring first', `locked toast: ${locked.lastToast?.title}`);
check(deniedS.forgets.length === 1 && hiddenNow(denied) && denied.statusLabel.label === 'Not allowed',
    `denied ForgetWords: hidden, ${denied.statusLabel.label}`);
check(shown(denied).slice().sort().join() === 'kadto,sige', `denied: the batch is back in the list: ${shown(denied)}`);
check(denied.showButton.visible && denied.showButton.label === 'Show learned words…', 'denied: the Show button');

// Multi-select across a search change, the confirm, and closing with pending words.
const multiS = stubCalls();
const multiWin = loaded(multiS, dialogLike);
const multi = multiWin.w;
multi.select('kadto', true);
multi.select('sige', true);
check(multi.selected.size === 2, 'two selected');
search(multi, 'sig');
check(shown(multi).join() === 'sige', 'search hides kadto');
let dlg = multi.forgetSelected();
check(dlg?.heading === 'Forget 2 words?', `confirm: ${dlg?.heading}`);
check(dlg?.default_response === 'cancel' && dlg?.close_response === 'cancel', 'Cancel is the default');
check(dlg?.get_response_appearance('forget') === Adw.ResponseAppearance.DESTRUCTIVE, 'Forget is destructive');
check(dlg?.get_response_label('cancel') === 'Cancel' && dlg?.get_response_label('forget') === 'Forget',
    'confirm buttons');
dlg.emit('response', 'cancel');
dlg.force_close(); // a real response closes the dialog
check(multi.toast === null && multi.selected.size === 2 && multiS.forgets.length === 0, 'Cancel does nothing');
dlg = multi.forgetSelected();
dlg.emit('response', 'forget');
dlg.force_close();
check(multi.toast?.title === 'Forgot 2 words', `selected batch toast: ${multi.toast?.title}`);
check(multi.selected.size === 0, 'forgotten words leave the selection');
search(multi, '');
check(shown(multi).length === 0, 'both selected rows left, the hidden one too');
check(multi.clearSelection !== undefined, 'clearSelection exists');
// Closing never holds (a held close-request would leave a window that only
// destroy() could close, which ExtensionPrefsDialog turns into an error page):
// the batch goes at once, then a flush, then the connection closes.
multiS.order.length = 0;
check(multiWin.win.emit('close-request') === false, 'close is never held, even with words pending');
check(multiS.forgets.length === 1 && multiS.forgets[0].slice().sort().join() === 'kadto,sige',
    'close sends the batch of 2 at once');
check(multiWin.win.releases === 1, `the service's close-request handler ran once (${multiWin.win.releases})`);
spin(50);
check(multiS.order.join() === 'forget,flush,close', `forget, flush, then close: ${multiS.order}`);
check(multiS.flushes === 1 && multiS.closes === 1, `one flush, close(conn) once (${multiS.flushes}, ${multiS.closes})`);
check(multiWin.win.destroyCalls === 0 && multiWin.win.closeCalls === 0,
    `our code calls neither destroy() nor close() (${multiWin.win.destroyCalls}, ${multiWin.win.closeCalls})`);
check(multiWin.win.releases === 1, 'the release count stays at 1');
check(multiWin.win.emit('close-request') === false && multiS.forgets.length === 1 && multiS.closes === 1,
    'a second close-request sends and closes nothing more');

// clearSelection, and a flush that never finishes on close.
const stuckS = stubCalls(t => ({
    flush: () => {
        t.flushes++;
        return new Promise(() => {});
    },
}));
const stuckWin = loaded(stuckS, dialogLike);
stuckWin.w.select('kadto', true);
stuckWin.w.clearSelection();
check(stuckWin.w.selected.size === 0 && stuckWin.w.forgetSelected() === null, 'Clear empties the selection');
stuckWin.w.forgetWord('sige');
check(stuckWin.win.emit('close-request') === false, 'stuck flush: the close is not held');
check(stuckS.forgets.length === 1, 'stuck flush: ForgetWords sent at once');
spin(300);
check(stuckS.flushes === 1 && stuckS.closes === 0, 'stuck flush: the connection waits for the flush');
spin(1000);
check(stuckS.closes === 1, `stuck flush: close(conn) after the safety timeout (${stuckS.closes})`);
check(stuckWin.win.destroyCalls === 0 && stuckWin.win.closeCalls === 0, 'stuck flush: no destroy() or close()');

// Nothing pending: the close goes ahead and closes the connection once.
{
    const s = stubCalls();
    const {win} = loaded(s, dialogLike);
    check(win.emit('close-request') === false, 'close goes ahead with nothing pending');
    spin(50);
    check(s.closes === 1 && s.forgets.length === 0, 'connection closed, nothing sent');
    check(win.destroyCalls === 0 && win.closeCalls === 0 && win.releases === 1, 'nothing pending: no destroy() or close()');
}

void Gtk;
GLib.spawn_command_line_sync(`rm -rf ${GLib.shell_quote(dir)}`);
print(`${passed} passed, ${failures} failed`);
if (failures > 0)
    System.exit(1);
