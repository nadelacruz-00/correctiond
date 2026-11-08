// Unit tests for extension/config.js. Run: gjs -m extension/test/config.test.js
// Exits non-zero when any test fails.

import GLib from 'gi://GLib';
import System from 'system';
import {
    defaultConfig,
    DEFAULT_ALLOWLIST,
    parseConfig,
    serializeConfig,
    normalizeAppId,
    validLangCode,
    MAX_LANGUAGES,
} from '../config.js';
import {parseOskSource} from '../lib.js';

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

function bad(text, needle, what) {
    const r = parseConfig(text);
    eq(r.ok, false, `${what} ok`);
    if (!r.error.includes(needle))
        throw new Error(`${what}: error ${JSON.stringify(r.error)} lacks ${JSON.stringify(needle)}`);
}

const DEFAULT_LANGS = [
    {code: 'en', lexicon: 'builtin:en'},
];

const ENGINE_ALLOWLIST = ['firefox', 'firefox_firefox', 'firefox-esr', 'org.mozilla.firefox', 'org.gnome.TextEditor'];

// The model behind extension/test/fixtures/config.toml. engine/src/config.rs
// reads that same file, so the engine and this writer agree on the format.
const FIXTURE_MODEL = {
    languages: [
        {code: 'en', lexicon: 'builtin:en'},
        {code: 'tl', lexicon: '~/.local/share/correctiond/lexicons/tl.txt'},
        {code: 'war', lexicon: '/opt/lex/wa"r\\x.txt'},
    ],
    allowlist: ['firefox', 'zen', 'org.gnome.TextEditor'],
    broken: ['org.example.Broken'],
    oskSource: 'correctiond:colemak',
    modules: {touch: true},
    touchExplicit: true,
    allowAnyCaller: false,
};

test('defaults match the engine', () => {
    const d = defaultConfig();
    eq(d.languages, DEFAULT_LANGS, 'languages');
    eq(d.allowlist, ENGINE_ALLOWLIST, 'allowlist');
    eq(d.broken, [], 'broken');
    eq(d.oskSource, 'correctiond:us', 'osk');
    eq(d.allowAnyCaller, false, 'any caller');
    eq(MAX_LANGUAGES, 16, 'max');
});

test('default allowlist matches the engine', () => {
    eq(defaultConfig().allowlist, ENGINE_ALLOWLIST, 'allowlist');
    eq(DEFAULT_ALLOWLIST, ENGINE_ALLOWLIST, 'const');
});

test('defaultConfig follows default-langs-vectors.tsv', () => {
    const dir = GLib.path_get_dirname(GLib.filename_from_uri(import.meta.url)[0]);
    const [, bytes] = GLib.file_get_contents(GLib.build_filenamev([dir, '..', '..', 'data', 'default-langs-vectors.tsv']));
    const rows = new TextDecoder().decode(bytes).split('\n')
        .filter(l => l.trim() !== '' && !l.startsWith('#'));
    eq(rows.length, 4, 'row count');
    for (const row of rows) {
        const [tl, war, codes] = row.split('\t');
        const legacyLangs = [];
        if (tl === '1') legacyLangs.push('tl');
        if (war === '1') legacyLangs.push('war');
        eq(defaultConfig({legacyLangs}).languages.map(l => l.code).join(','), codes, row);
    }
});

test('missing or empty file gives defaults', () => {
    eq(parseConfig(null), {ok: true, config: defaultConfig()}, 'null');
    eq(parseConfig(''), {ok: true, config: defaultConfig()}, 'empty');
    eq(parseConfig('# only a comment\n\n'), {ok: true, config: defaultConfig()}, 'comment');
});

test('hand-written config with comments and a multi-line array', () => {
    const text = `# correctiond config.
# GJS OSK note.
auto_switch_osk = false

allowlist = [
    "firefox",            # Firefox from the .deb
    "firefox_firefox",    # Firefox snap
    "zen",
    'org.gnome.Ptyxis',   # literal string
]
`;
    const r = parseConfig(text);
    eq(r.ok, true, 'ok');
    eq(r.config.allowlist, ['firefox', 'firefox_firefox', 'zen', 'org.gnome.Ptyxis'], 'allowlist');
    eq(r.config.oskSource, '', 'older auto_switch_osk = false means no switch');
    // Keys that are absent keep the engine defaults.
    eq(r.config.languages, DEFAULT_LANGS, 'languages default');
    eq(r.config.broken, [], 'broken default');
});

test('languages tables and string escapes', () => {
    const text = `allowlist = ["a"]
broken = []
allow_any_caller = true

[[languages]]
code = "en"
lexicon = "builtin:en"

[[languages]]   # Cebuano
code = 'ceb'
lexicon = "/opt/l\\u00e9x/\\"c\\"\\\\eb.txt"
`;
    const r = parseConfig(text);
    eq(r.ok, true, 'ok');
    eq(r.config.languages, [
        {code: 'en', lexicon: 'builtin:en'},
        {code: 'ceb', lexicon: '/opt/léx/"c"\\eb.txt'},
    ], 'languages');
    eq(r.config.allowAnyCaller, true, 'any caller kept');
});

test('serialize then parse round-trips', () => {
    for (const m of [defaultConfig(), FIXTURE_MODEL,
        {...defaultConfig(), allowlist: [], broken: [], allowAnyCaller: true}]) {
        const r = parseConfig(serializeConfig(m));
        eq(r, {ok: true, config: m}, 'round trip');
    }
});

test('the extension reads osk_source from written files', () => {
    for (const v of ['', 'correctiond:colemak', 'correctiond:us'])
        eq(parseOskSource(serializeConfig({...FIXTURE_MODEL, oskSource: v})), v, JSON.stringify(v));
});

test('older auto_switch_osk, and osk_source winning over it', () => {
    eq(parseConfig('auto_switch_osk = true\n').config.oskSource, 'correctiond:us', 'true');
    eq(parseConfig('auto_switch_osk = false\n').config.oskSource, '', 'false');
    eq(parseConfig('osk_source = "correctiond:colemak"\nauto_switch_osk = true\n').config.oskSource,
        'correctiond:colemak', 'osk_source first');
    eq(parseConfig('auto_switch_osk = false\nosk_source = "correctiond:us"\n').config.oskSource,
        'correctiond:us', 'osk_source second');
    // Written too, for an older running companion that only knows this key:
    // true only when the choice is the US engine (what true always meant).
    for (const [v, old] of [['correctiond:us', 'true'], ['correctiond:colemak', 'false'], ['', 'false']]) {
        if (!serializeConfig({...defaultConfig(), oskSource: v}).includes(`auto_switch_osk = ${old}\n`))
            throw new Error(`older key for ${JSON.stringify(v)} is not ${old}`);
    }
});

test('allow_any_caller is written only when on', () => {
    if (serializeConfig(defaultConfig()).includes('allow_any_caller'))
        throw new Error('written while off');
    if (!serializeConfig({...defaultConfig(), allowAnyCaller: true}).includes('allow_any_caller = true'))
        throw new Error('not written while on');
});

test('serialized output matches the shared fixture', () => {
    const path = GLib.build_filenamev([
        GLib.path_get_dirname(GLib.filename_from_uri(import.meta.url)[0]),
        'fixtures', 'config.toml']);
    const [, bytes] = GLib.file_get_contents(path);
    eq(serializeConfig(FIXTURE_MODEL), new TextDecoder().decode(bytes), 'fixture');
});

test('unreadable files are refused, never guessed', () => {
    bad('allowlist = 3\n', 'line 1', 'number');
    bad('typo_key = true\n', 'typo_key', 'unknown key');
    bad('[settings]\nx = 1\n', 'line 1', 'plain table');
    bad('allowlist = ["a"\n', 'unterminated', 'open array');
    bad('allowlist = ["a]\n', 'line 1', 'open string');
    bad('auto_switch_osk = true\nauto_switch_osk = false\n', 'twice', 'duplicate');
    bad('[[languages]]\ncode = "en"\n', 'lexicon', 'missing lexicon');
    bad('[[languages]]\ncode = "en"\nlexicon = "x"\nname = "y"\n', 'name', 'unknown language key');
    bad('auto_switch_osk = "no"\n', 'line 1', 'string for bool');
    bad('osk_source = true\n', 'line 1', 'bool for string');
    bad('allowlist = ["a", 1]\n', 'line 1', 'mixed array');
});

test('[modules] touch parses', () => {
    const c = parseConfig('[modules]\ntouch = false\n').config;
    eq(c.modules.touch, false, 'touch');
    eq(c.touchExplicit, true, 'explicit');
});

test('no [modules] means null and not explicit', () => {
    const c = parseConfig('allowlist = ["x"]\n').config;
    eq(c.modules.touch, null, 'touch');
    eq(c.touchExplicit, false, 'explicit');
});

test('save without the switch keeps the key absent', () => {
    if (serializeConfig(parseConfig('allowlist = ["x"]\n').config).includes('touch'))
        throw new Error('touch written');
});

test('a flipped switch writes the key', () => {
    const c = parseConfig('allowlist = ["x"]\n').config;
    c.modules.touch = true;
    c.touchExplicit = true;
    const out = serializeConfig(c);
    const at = out.indexOf('[modules]\ntouch = true\n');
    if (at < 0 || at > out.indexOf('[[languages]]'))
        throw new Error(`bad output: ${out}`);
});

test('an explicit key survives saves', () => {
    const c = parseConfig('[modules]\ntouch = false\n').config;
    const d = parseConfig(serializeConfig(c)).config;
    eq(d.modules.touch, false, 'touch');
    eq(d.touchExplicit, true, 'explicit');
});

test('[modules] above osk_source is refused', () => {
    // TOML puts osk_source inside [modules]. The engine ignores that stray
    // key; this reader refuses the file so Settings stays read-only.
    bad('[modules]\ntouch = true\nosk_source = "x"\n', "unknown [modules] setting 'osk_source'", 'stray key');
});

test('touch inside [[languages]] is refused', () => {
    bad('[[languages]]\ncode = "en"\nlexicon = "x"\ntouch = true\n', "unknown language setting 'touch'", 'touch in language');
});

test('touch must be a bool', () => {
    bad('[modules]\ntouch = "yes"\n', 'expected true or false', 'string touch');
});

test('missing languages use legacyLangs', () => {
    const c = parseConfig('allowlist = ["x"]\n', {legacyLangs: ['tl']}).config;
    eq(c.languages.map(l => l.code).join(','), 'en,tl', 'codes');
});

test('an invalid file is ok:false', () => {
    eq(parseConfig('[[[', {legacyLangs: ['tl']}).ok, false, 'ok');
});

test('app ids', () => {
    eq(normalizeAppId('  org.gnome.TextEditor '), 'org.gnome.TextEditor', 'trim');
    eq(normalizeAppId('firefox_firefox'), 'firefox_firefox', 'snap');
    eq(normalizeAppId(''), null, 'empty');
    eq(normalizeAppId('   '), null, 'blank');
    eq(normalizeAppId('two words'), null, 'space');
    eq(normalizeAppId('a"b'), null, 'quote');
    eq(normalizeAppId('a\nb'), null, 'newline');
});

test('language codes', () => {
    for (const c of ['en', 'tl', 'war', 'ceb', 'pt_br'])
        eq(validLangCode(c), true, c);
    for (const c of ['', 'e', 'EN', 'en us', 'toolonglang', 'e"n'])
        eq(validLangCode(c), false, JSON.stringify(c));
});

print(`${passed} passed, ${failures} failed`);
if (failures > 0)
    System.exit(1);
