// Unit tests for extension/lib.js. Run: gjs -m extension/test/lib.test.js
// Exits non-zero when any test fails.

import System from 'system';
import GLib from 'gi://GLib';
import {
    parseOskSource,
    autoSwitchDecision,
    normalizeWmClass,
    focusedApp,
    isGjsOskActor,
    oskShowing,
    sourceKey,
    findSource,
    SourceRestore,
    Latch,
    purgeNotice,
    PURGE_PARTIAL,
    PURGE_PARTIAL_NOTICE,
    QWERTY_ENGINE,
    statusText,
    fillName,
    STATUS_ROWS,
    forgetFailureText,
    FORGET_FAILED,
    SORTS,
    SORT_LABELS,
    compareWords,
    matchesSearch,
    lastUsedText,
    PendingForget,
    GJS_OSK_UUID,
    effectiveTouch,
    touchAfterRead,
    gjsOskInstalledIn,
    componentDirs,
    componentInstalledIn,
    SETUP_WAIT_MS,
    setupState,
    SETUP_TEXT,
    correctsInLine,
    isCorrectiondSource,
} from '../lib.js';

let failures = 0;
let passed = 0;

function eq(actual, expected, what) {
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e)
        throw new Error(`${what}: expected ${e}, got ${a}`);
}

function test(name, fn) {
    try {
        fn();
        passed++;
    } catch (e) {
        failures++;
        printerr(`FAIL ${name}: ${e.message}`);
    }
}

// --- config: osk_source (and the older auto_switch_osk) ---

const COLEMAK = 'correctiond:colemak';

test('missing or empty config switches to the US engine (the old default)', () => {
    eq(parseOskSource(null), QWERTY_ENGINE, 'null');
    eq(parseOskSource(''), QWERTY_ENGINE, 'empty');
});

test('osk_source names the source, or "" for none', () => {
    eq(parseOskSource('osk_source = "correctiond:colemak"\n'), COLEMAK, 'colemak');
    eq(parseOskSource('osk_source="correctiond:us"'), QWERTY_ENGINE, 'no spaces');
    eq(parseOskSource('  osk_source  =  ""  \r\n'), '', 'none, padding, CRLF');
    eq(parseOskSource('osk_source = "correctiond:colemak" # tablet\n'), COLEMAK, 'comment');
});

test('osk_source wins over the older auto_switch_osk', () => {
    eq(parseOskSource('auto_switch_osk = true\nosk_source = "correctiond:colemak"\n'), COLEMAK, 'after');
    eq(parseOskSource('osk_source = ""\nauto_switch_osk = true\n'), '', 'before');
});

test('older configs: auto_switch_osk true means US, false means none', () => {
    eq(parseOskSource('auto_switch_osk = true\n'), QWERTY_ENGINE, 'true');
    eq(parseOskSource('auto_switch_osk = false\n'), '', 'false');
    eq(parseOskSource('auto_switch_osk = false # off\n'), '', 'comment');
    eq(parseOskSource('# auto_switch_osk = false\n'), QWERTY_ENGINE, 'commented out');
    eq(parseOskSource('auto_switch_osk = "false"\n'), QWERTY_ENGINE, 'not a boolean');
});

test('other keys, prefixes and odd values do not match', () => {
    eq(parseOskSource('xosk_source = ""\n'), QWERTY_ENGINE, 'prefix');
    eq(parseOskSource('osk_source_extra = ""\n'), QWERTY_ENGINE, 'suffix');
    eq(parseOskSource('osk_source = correctiond:colemak\n'), QWERTY_ENGINE, 'unquoted');
    eq(parseOskSource('osk_source = "a\\"b"\n'), QWERTY_ENGINE, 'escape');
    eq(parseOskSource('allowlist = ["firefox"]\nosk_source = ""\n'), '', 'after other key');
});

test('only top-level keys count, not keys inside a table', () => {
    eq(parseOskSource('[[languages]]\ncode = "en"\nosk_source = ""\n'), QWERTY_ENGINE, 'in table');
    eq(parseOskSource('osk_source = ""\n[[languages]]\ncode = "en"\n'), '', 'before table');
});

test('auto-switch decision: config on, global sources', () => {
    eq(autoSwitchDecision(COLEMAK, false), 'switch', 'switch');
});

test('auto-switch decision: config off never switches', () => {
    eq(autoSwitchDecision('', false), 'off', 'off');
    eq(autoSwitchDecision('', true), 'off', 'off with per-window');
});

test('auto-switch decision: per-window input sources skip the switch', () => {
    eq(autoSwitchDecision(COLEMAK, true), 'per-window', 'per-window');
});

// --- wm_class ---

test('wm_class is lowercased', () => {
    eq(normalizeWmClass('Firefox'), 'firefox', 'Firefox');
    eq(normalizeWmClass('org.gnome.Ptyxis'), 'org.gnome.ptyxis', 'Ptyxis');
});

test('no wm_class means empty (unidentified)', () => {
    eq(normalizeWmClass(null), '', 'null');
    eq(normalizeWmClass(undefined), '', 'undefined');
    eq(normalizeWmClass(''), '', 'empty');
});

test('a shell text entry with key focus means unidentified', () => {
    // e.g. the overview search entry while Firefox is still the focus window
    eq(focusedApp('Firefox', true), '', 'shell entry');
    eq(focusedApp('Firefox', false), 'firefox', 'window');
    eq(focusedApp(null, false), '', 'no window');
});

test('latch works for strings too', () => {
    const l = new Latch(null);
    eq(l.set(''), true, 'first empty is a change');
    eq(l.set(''), false, 'same');
    eq(l.set('firefox'), true, 'changed');
});

// --- gjs-osk actor matcher ---

function fakeActor({name = null, classes = [], children = [], visible = true, mapped = true} = {}) {
    return {
        visible,
        mapped,
        get_name: () => name,
        has_style_class_name: c => classes.includes(c),
        get_children: () => children,
    };
}

test('matches the keyboard by its dialog content style class', () => {
    // At child-added time the Dialog holds only its _dialog box.
    const kb = fakeActor({children: [fakeActor({classes: ['modal-dialog', 'db-keyboard-content']})]});
    eq(isGjsOskActor(kb), true, 'style class');
});

test('matches the keyboard by its key box name', () => {
    const kb = fakeActor({children: [fakeActor({name: 'osk-gjs'})]});
    eq(isGjsOskActor(kb), true, 'name');
});

test('a plain Clutter child without style classes is fine', () => {
    const plain = {get_name: () => 'osk-gjs', get_children: () => []};
    eq(isGjsOskActor({get_children: () => [plain]}), true, 'plain child');
});

test('does not match other actors', () => {
    eq(isGjsOskActor(null), false, 'null');
    eq(isGjsOskActor({}), false, 'no get_children');
    eq(isGjsOskActor(fakeActor()), false, 'no children');
    eq(isGjsOskActor(fakeActor({children: [fakeActor({classes: ['modal-dialog']})]})), false, 'other dialog');
    eq(isGjsOskActor(fakeActor({name: 'osk-gjs'})), false, 'the key box itself');
    // A grandchild does not count: only the keyboard's direct children.
    const deep = fakeActor({children: [fakeActor({children: [fakeActor({name: 'osk-gjs'})]})]});
    eq(isGjsOskActor(deep), false, 'grandchild');
});

test('showing means visible and mapped', () => {
    eq(oskShowing(fakeActor({visible: true, mapped: true})), true, 'both');
    eq(oskShowing(fakeActor({visible: false, mapped: true})), false, 'hidden');
    eq(oskShowing(fakeActor({visible: true, mapped: false})), false, 'unmapped');
    eq(oskShowing(null), false, 'no actor');
});

// --- input sources ---

const sources = {
    0: {type: 'ibus', id: 'correctiond:colemak'},
    1: {type: 'ibus', id: 'correctiond:us'},
    2: {type: 'xkb', id: 'us'},
};

test('the QWERTY engine name', () => {
    eq(QWERTY_ENGINE, 'correctiond:us', 'name');
});

test('source keys and lookup', () => {
    eq(sourceKey(sources[1]), 'ibus\ncorrectiond:us', 'key');
    eq(sourceKey(null), null, 'null');
    eq(findSource(sources, 'ibus\ncorrectiond:us'), sources[1], 'found');
    eq(findSource(sources, 'xkb\ncorrectiond:us'), null, 'type matters');
    eq(findSource(null, 'ibus\ncorrectiond:us'), null, 'no sources');
});

test('show switches to the target and hide restores the prior source', () => {
    const r = new SourceRestore();
    eq(r.shown('colemak', 'us'), 'us', 'switch');
    eq(r.pending, true, 'pending');
    eq(r.hidden('us'), 'colemak', 'restore');
    eq(r.pending, false, 'cleared');
});

test('already on the target: no switch and nothing to restore', () => {
    const r = new SourceRestore();
    eq(r.shown('us', 'us'), null, 'no switch');
    eq(r.hidden('us'), null, 'no restore');
});

test('no target (QWERTY variant not configured): nothing happens', () => {
    const r = new SourceRestore();
    eq(r.shown('colemak', null), null, 'no switch');
    eq(r.hidden('colemak'), null, 'no restore');
});

test('user picked another source while the OSK was up: do not restore', () => {
    const r = new SourceRestore();
    r.shown('colemak', 'us');
    eq(r.hidden('xkb-de'), null, 'kept user choice');
    eq(r.pending, false, 'cleared');
});

test('a second show keeps the first prior source', () => {
    const r = new SourceRestore();
    r.shown('colemak', 'us');
    eq(r.shown('us', 'us'), null, 'no second switch');
    eq(r.hidden('us'), 'colemak', 'original prior');
});

test('hide without show restores nothing', () => {
    eq(new SourceRestore().hidden('us'), null, 'nothing');
});

// --- latch ---

test('latch reports changes only, starting from false', () => {
    const l = new Latch(false);
    eq(l.set(false), false, 'same');
    eq(l.set(true), true, 'changed');
    eq(l.set(true), false, 'same again');
    eq(l.value, true, 'value');
    eq(l.set(false), true, 'back');
});

// --- purge notification ---

test('success', () => {
    eq(purgeNotice(null), {title: 'Autocorrect', body: 'Learned words forgotten.'}, 'ok');
});

test('PurgePartial with the key deletion pending uses the fixed text', () => {
    eq(PURGE_PARTIAL, 'org.nadelacruz.Correctiond1.Error.PurgePartial', 'name');
    eq(PURGE_PARTIAL_NOTICE,
        'Learned words deleted; the key will be removed when your keyring unlocks', 'text');
    const msg = 'learned data deleted; keyring item will be removed when the keyring is unlocked';
    eq(purgeNotice({name: PURGE_PARTIAL, message: msg}), {title: 'Autocorrect', body: PURGE_PARTIAL_NOTICE}, 'pending');
    eq(purgeNotice({name: PURGE_PARTIAL, message: 'something new'}), {title: 'Autocorrect', body: PURGE_PARTIAL_NOTICE}, 'unknown text');
});

test('PurgePartial when files could not be removed does not claim success', () => {
    const n = purgeNotice({name: PURGE_PARTIAL, message: 'learned data files could not be removed'});
    eq(n.title, 'Autocorrect', 'title');
    eq(n.body.includes('deleted;'), false, 'no success claim');
    eq(n.body, 'Some learned data could not be deleted. Try “Forget learned words” again.', 'text');
});

test('access denied and an absent engine', () => {
    eq(purgeNotice({name: 'org.freedesktop.DBus.Error.AccessDenied', message: 'x'}).body,
        'Could not forget learned words: the correction service refused the request.', 'denied');
    eq(purgeNotice({name: 'org.freedesktop.DBus.Error.ServiceUnknown', message: 'x'}).body,
        'Could not forget learned words: the correction service is not running.', 'absent');
    eq(purgeNotice({name: null, message: 'local failure'}).body,
        'Could not forget learned words: the correction service is not running.', 'local error');
});

const COUNTS = {corrections: 12, reverts: 3, words_learned: 9000};
const st = (mode, reason, extra = {}) =>
    ({app_mode: mode, app_reason: reason, enabled: true, learning: true, ...extra});

function readVectors() {
    const dir = GLib.path_get_dirname(GLib.filename_from_uri(import.meta.url)[0]);
    const [, bytes] = GLib.file_get_contents(GLib.build_filenamev([dir, '..', '..', 'data', 'status-vectors.tsv']));
    return new TextDecoder().decode(bytes).split('\n')
        .filter(l => l.trim() !== '' && !l.startsWith('#'))
        .map(l => {
            const [mode, reason, appLine, subtitle] = l.split('\t');
            return {mode, reason, appLine, subtitle};
        });
}

test('statusText matches every vectors row', () => {
    const rows = readVectors();
    eq(rows.length, 9, 'row count');
    for (const r of rows) {
        const out = statusText(COUNTS, st(r.mode, r.reason), 'Firefox');
        eq(out.appLine, fillName(r.appLine, 'Firefox'), `appLine ${r.mode}/${r.reason}`);
        eq(out.subtitle, fillName(r.subtitle, 'Firefox'), `subtitle ${r.mode}/${r.reason}`);
    }
    eq(Object.keys(STATUS_ROWS).sort(), rows.map(r => `${r.mode}\t${r.reason}`).sort(), 'keys');
});

test('statusText learning paused', () => {
    const base = statusText(COUNTS, st('correct', 'allowlisted'), 'Firefox');
    const out = statusText(COUNTS, st('correct', 'allowlisted', {learning: false}), 'Firefox');
    eq(out.subtitle, 'Learning paused', 'subtitle');
    eq(out.appLine, base.appLine, 'appLine unchanged');
});

test('statusText off wins over paused', () => {
    const out = statusText(COUNTS, st('correct', 'allowlisted', {enabled: false, learning: false}), 'Firefox');
    eq(out.subtitle, 'Off', 'subtitle');
    eq(out.appLine, 'Autocorrect is off', 'appLine');
    eq(out.controls, true, 'the switch and list stay while off');
});

test('statusText engine down', () => {
    const out = statusText(null, null, 'Firefox');
    eq(out, {subtitle: 'Engine not running', appLine: 'Engine not running', countsLine: null, controls: false},
        'down');
    eq(statusText(COUNTS, null, 'Firefox').countsLine, null, 'status null');
    eq(statusText(null, st('none', ''), 'Firefox').countsLine, null, 'stats null');
});

test('statusText older engine without GetStatus', () => {
    const out = statusText(COUNTS, 'unsupported', 'Firefox');
    eq(out, {
        subtitle: 'Autocorrect',
        appLine: 'No text field yet',
        countsLine: '12 corrections · 3 undone · 9,000 words learned',
        controls: false,
    }, 'unsupported');
    eq(statusText(null, 'unsupported', 'Firefox').appLine, 'Engine not running', 'stats missing: down');
    eq(statusText(COUNTS, st('correct', 'allowlisted'), 'Firefox').controls, true, 'controls with GetStatus');
    eq(statusText(null, null, 'Firefox').controls, false, 'no controls when down');
});

test('fillName inserts the app name literally', () => {
    eq(fillName('Correcting in NAME', 'A$&B$$'), 'Correcting in A$&B$$', 'dollar patterns');
    eq(fillName("Correcting in NAME", "$'x$`"), "Correcting in $'x$`", 'quote patterns');
});

test('statusText terminal row names both reasons', () => {
    const out = statusText(COUNTS, st('off', 'terminal'), 'Kitty');
    eq(out.appLine, 'Off in this terminal (not in your app list, or marked broken)', 'appLine');
    eq(out.subtitle, 'Off in this field', 'subtitle');
});

test('statusText unknown app name', () => {
    const out = statusText(COUNTS, st('correct', 'allowlisted'), null);
    eq(out.appLine, 'Correcting', 'appLine');
    eq(out.subtitle, 'Correcting', 'subtitle');
});

test('statusText counts', () => {
    eq(statusText(COUNTS, st('none', ''), null).countsLine,
        '12 corrections · 3 undone · 9,000 words learned', 'counts');
});

test('forget failure: a locked keyring asks to unlock it', () => {
    eq(forgetFailureText({name: FORGET_FAILED, message: 'unlock your keyring first'}),
        'Unlock your keyring first', 'locked');
});

test('forget failure: anything else is "Could not forget"', () => {
    eq(forgetFailureText({name: FORGET_FAILED, message: 'could not forget'}), 'Could not forget', 'failed');
    eq(forgetFailureText({name: 'org.freedesktop.DBus.Error.NoReply', message: 'unlock your keyring first'}),
        'Could not forget', 'other error name');
    eq(forgetFailureText({name: null, message: 'engine not running'}), 'Could not forget', 'local');
    eq(FORGET_FAILED, 'org.nadelacruz.Correctiond1.Error.ForgetFailed', 'error name');
});

const W = (word, lastSeen = 0) => ({word, count: 1, lastSeen, personal: true});
const sorted = (words, sort) => [...words].sort((a, b) => compareWords(a, b, sort)).map(w => w.word);

test('sorts: constants', () => {
    eq(SORTS, ['az', 'za', 'newest', 'oldest'], 'SORTS');
    eq(SORT_LABELS, {az: 'A to Z', za: 'Z to A', newest: 'Newest first', oldest: 'Oldest first'}, 'labels');
});

test('sorts: az, za, and non-ASCII', () => {
    const ws = ['b', 'A', 'a2'].map(w => W(w));
    eq(sorted(ws, 'az'), ['A', 'a2', 'b'], 'az');
    eq(sorted(ws, 'za'), ['b', 'a2', 'A'], 'za');
    eq(sorted(['é', 'Ünï', 'z', 'Z', '日本'].map(w => W(w)), 'az').length, 5, 'non-ASCII');
});

test('sorts: newest and oldest, ties A to Z', () => {
    const ws = [W('b', 5), W('a', 5), W('c', 9), W('d', 1)];
    eq(sorted(ws, 'newest'), ['c', 'a', 'b', 'd'], 'newest');
    eq(sorted(ws, 'oldest'), ['d', 'a', 'b', 'c'], 'oldest');
});

test('matchesSearch: literal, case-insensitive', () => {
    eq(matchesSearch('Kadto', 'ad'), true, 'substring');
    eq(matchesSearch('kadto', 'AD'), true, 'case');
    eq(matchesSearch('a.b', 'a.b'), true, 'dot literal');
    eq(matchesSearch('axb', 'a.b'), false, 'dot not wildcard');
    eq(matchesSearch('x', '('), false, 'paren');
    eq(matchesSearch('x', ''), true, 'empty');
});

test('lastUsedText: local calendar days', () => {
    const now = new Date(2026, 10, 6, 12, 0).getTime();
    const s = d => Math.floor(d.getTime() / 1000);
    eq(lastUsedText(s(new Date(2026, 10, 6, 0, 5)), now), 'today', 'today');
    eq(lastUsedText(s(new Date(2026, 10, 5, 23, 59)), now), 'yesterday', 'yesterday');
    eq(lastUsedText(s(new Date(2026, 2, 4, 9, 0)), now), '4 Mar', 'this year');
    eq(lastUsedText(s(new Date(2025, 11, 31, 9, 0)), now), '31 Dec 2025', 'last year');
    eq(lastUsedText(0, now), 'never', 'never');
});

test('PendingForget: add, take, undo', () => {
    const p = new PendingForget();
    eq(p.add(['a', 'b']), 2, 'first');
    eq(p.add(['b', 'c']), 3, 'dupes ignored');
    eq(p.size, 3, 'size');
    eq(p.take(), ['a', 'b', 'c'], 'take');
    eq(p.take(), [], 'take again');
    p.add(['x']);
    eq(p.undo(), ['x'], 'undo');
    eq(p.size, 0, 'cleared');
});

// --- touch module and first-run helpers ---

function tmpDir() {
    return GLib.dir_make_tmp('lib-test-XXXXXX');
}
const isDir = p => GLib.file_test(p, GLib.FileTest.IS_DIR);
const isFile = p => GLib.file_test(p, GLib.FileTest.IS_REGULAR);

test('effectiveTouch: explicit value wins, else installed', () => {
    eq(effectiveTouch(null, true), true, 'null+installed');
    eq(effectiveTouch(null, false), false, 'null+not');
    eq(effectiveTouch(false, true), false, 'explicit false');
    eq(effectiveTouch(true, false), true, 'explicit true');
});

test('touchAfterRead: missing, invalid and ok reads', () => {
    eq(touchAfterRead({kind: 'missing'}, null, true), true, 'missing+installed');
    eq(touchAfterRead({kind: 'missing'}, true, false), false, 'missing+not');
    eq(touchAfterRead({kind: 'invalid'}, true, false), true, 'invalid keeps current');
    eq(touchAfterRead({kind: 'invalid'}, null, true), true, 'invalid at enable uses installed');
    eq(touchAfterRead({kind: 'invalid'}, null, false), false, 'invalid at enable, not installed');
    eq(touchAfterRead({kind: 'ok', touch: false}, null, true), false, 'ok false');
    eq(touchAfterRead({kind: 'ok', touch: null}, null, true), true, 'ok null derives');
});

test('gjsOskInstalledIn: finds the extension directory', () => {
    eq(GJS_OSK_UUID, 'gjsosk@vishram1123.com', 'uuid');
    const base = tmpDir();
    const other = tmpDir();
    GLib.mkdir_with_parents(`${base}/gnome-shell/extensions/${GJS_OSK_UUID}`, 0o755);
    eq(gjsOskInstalledIn([other, base], isDir), true, 'present');
    eq(gjsOskInstalledIn([other], isDir), false, 'absent');
    eq(gjsOskInstalledIn([], isDir), false, 'no dirs');
});

test('componentDirs: system dir plus non-empty path entries', () => {
    eq(componentDirs(null), ['/usr/share/ibus/component'], 'null');
    eq(componentDirs(':/opt/x:'), ['/usr/share/ibus/component', '/opt/x'], 'colons');
});

test('componentInstalledIn: finds correctiond.xml', () => {
    const a = tmpDir();
    const b = tmpDir();
    GLib.file_set_contents(`${b}/correctiond.xml`, '<component/>');
    eq(componentInstalledIn([a, b], isFile), true, 'present');
    eq(componentInstalledIn([a], isFile), false, 'absent');
    eq(componentInstalledIn([], isFile), false, 'none');
});

test('setupState covers every input combination', () => {
    eq(SETUP_WAIT_MS, 5000, 'wait');
    for (let i = 0; i < 32; i++) {
        const inp = {
            componentInstalled: !!(i & 1),
            sourceConfigured: !!(i & 2),
            sourceCurrent: !!(i & 4),
            engineOwned: !!(i & 8),
            ownerWaitExpired: !!(i & 16),
        };
        let want;
        if (inp.engineOwned) want = null;
        else if (!inp.componentInstalled) want = 'no-engine';
        else if (!inp.sourceConfigured) want = 'no-source';
        else if (!inp.sourceCurrent) want = 'paused';
        else if (!inp.ownerWaitExpired) want = 'starting';
        else want = 'failed';
        eq(setupState(inp), want, JSON.stringify(inp));
    }
    const base = {componentInstalled: true, sourceConfigured: true, sourceCurrent: true,
        engineOwned: false, ownerWaitExpired: false};
    eq(setupState({...base, engineOwned: true}), null, 'owned');
    eq(setupState({...base, componentInstalled: false}), 'no-engine', 'no-engine');
    eq(setupState({...base, sourceConfigured: false}), 'no-source', 'no-source');
    eq(setupState({...base, sourceCurrent: false, ownerWaitExpired: true}), 'paused', 'paused');
    eq(setupState(base), 'starting', 'starting');
    eq(setupState({...base, ownerWaitExpired: true}), 'failed', 'failed');
});

test('setup texts are the spec strings', () => {
    const quick = {
        'no-engine': 'Install the correctiond engine package',
        'no-source': "Add 'English (US, autocorrect)' as an input source. If it isn't listed, run `ibus restart` first",
        'paused': 'Paused: another input source is active (Super+Space to switch)',
        'starting': 'Starting…',
        'failed': "The autocorrect engine isn't reachable. Run `ibus restart`; if it still fails, see `journalctl --user -b | grep correctiond`",
    };
    const settings = {
        'no-engine': 'Install the correctiond engine package',
        'no-source': "Add 'English (US, autocorrect)' as an input source. If it isn't listed, press Restart below.",
        'paused': 'Paused: another input source is active (Super+Space to switch)',
        'starting': 'Starting…',
        'failed': "The autocorrect engine isn't reachable. Press Restart below; if it still fails, see `journalctl --user -b | grep correctiond`",
    };
    eq(SETUP_TEXT.quick, quick, 'quick');
    eq(SETUP_TEXT.settings, settings, 'settings');
    eq(correctsInLine(['Kate', 'Firefox']), 'Corrects in: Kate, Firefox. Add apps on the Apps page.', 'extra line');
});

test('isCorrectiondSource', () => {
    eq(isCorrectiondSource(['ibus', 'correctiond:us']), true, 'ours');
    eq(isCorrectiondSource(['ibus', 'other']), false, 'other ibus');
    eq(isCorrectiondSource(['xkb', 'correctiond:us']), false, 'xkb');
});

print(`${passed} passed, ${failures} failed`);
if (failures > 0)
    System.exit(1);
