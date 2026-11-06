// Reads and writes ~/.config/correctiond/config.toml for the preferences
// window. Pure (no GLib, Gtk or Shell imports) so it is unit-tested with gjs.
//
// The parser accepts only the TOML the engine's Config uses: top-level
// `allowlist`/`broken` (arrays of strings), `osk_source` (string),
// `allow_any_caller` and the older `auto_switch_osk` (booleans), and
// `[[languages]]` tables with `code` and `lexicon` strings. Anything else is refused with a line number instead of
// guessed at, so the window never overwrites a file it did not understand.
// Keys left out keep the engine's defaults (engine/src/config.rs).

export const MAX_LANGUAGES = 16;

export const BUILTIN_EN = 'builtin:en';

/** Default OSK switch target: what `auto_switch_osk = true` always meant. */
export const US_ENGINE = 'correctiond:us';

/** The engine's defaults (engine/src/config.rs `Config::default`). */
export function defaultConfig() {
    return {
        languages: [
            {code: 'en', lexicon: BUILTIN_EN},
            {code: 'tl', lexicon: '~/.local/share/correctiond/lexicons/tl.txt'},
            {code: 'war', lexicon: '~/.local/share/correctiond/lexicons/war.txt'},
        ],
        allowlist: ['firefox'],
        broken: [],
        // IBus engine to switch to while GJS OSK is open; '' = no switch.
        oskSource: US_ENGINE,
        allowAnyCaller: false,
    };
}

/**
 * A window class / Wayland app id as typed or picked: trimmed, or null when
 * empty or when it holds whitespace, quotes, a backslash, '#' or a control
 * character (no real app id does).
 *
 * @param {string} s
 * @returns {string|null}
 */
export function normalizeAppId(s) {
    const t = (s ?? '').trim();
    if (t === '' || /[\s"'\\#\u0000-\u001f\u007f]/.test(t))
        return null;
    return t;
}

/** Lowercase letters (and '_'), 2 to 8 characters: en, tl, war, pt_br. */
export function validLangCode(code) {
    return /^[a-z][a-z_]{1,7}$/.test(code ?? '');
}

// --- parsing ---

class ParseError extends Error {}

function fail(line, msg) {
    throw new ParseError(`line ${line}: ${msg}`);
}

const ESCAPES = {b: '\b', t: '\t', n: '\n', f: '\f', r: '\r', '"': '"', '\\': '\\'};

/** Splits TOML text into tokens: nl, [[, ]], [, ], ',', =, bare, str. */
function tokenize(text) {
    const toks = [];
    let line = 1;
    let i = 0;
    const push = (type, value) => toks.push({type, value, line});
    while (i < text.length) {
        const c = text[i];
        if (c === '\n') {
            push('nl');
            line++;
            i++;
        } else if (c === ' ' || c === '\t' || c === '\r') {
            i++;
        } else if (c === '#') {
            while (i < text.length && text[i] !== '\n')
                i++;
        } else if (text.startsWith('[[', i)) {
            push('[[');
            i += 2;
        } else if (text.startsWith(']]', i)) {
            push(']]');
            i += 2;
        } else if ('[],='.includes(c)) {
            push(c);
            i++;
        } else if (c === '"' || c === "'") {
            if (text.startsWith(c.repeat(3), i))
                fail(line, 'multi-line strings are not supported');
            let s = '';
            i++;
            for (;;) {
                if (i >= text.length || text[i] === '\n')
                    fail(line, 'unterminated string');
                const d = text[i];
                if (d === c) {
                    i++;
                    break;
                }
                if (c === '"' && d === '\\') {
                    const e = text[i + 1];
                    if (e in ESCAPES) {
                        s += ESCAPES[e];
                        i += 2;
                    } else if (e === 'u' || e === 'U') {
                        const n = e === 'u' ? 4 : 8;
                        const hex = text.slice(i + 2, i + 2 + n);
                        if (!new RegExp(`^[0-9a-fA-F]{${n}}$`).test(hex))
                            fail(line, 'bad \\u escape');
                        s += String.fromCodePoint(parseInt(hex, 16));
                        i += 2 + n;
                    } else {
                        fail(line, 'bad escape in string');
                    }
                } else {
                    s += d;
                    i++;
                }
            }
            push('str', s);
        } else {
            const m = /^[A-Za-z0-9_-]+/.exec(text.slice(i));
            if (!m)
                fail(line, `unexpected character ${JSON.stringify(c)}`);
            push('bare', m[0]);
            i += m[0].length;
        }
    }
    push('nl');
    return toks;
}

const TOP_KEYS = {
    allowlist: 'strings',
    broken: 'strings',
    osk_source: 'string',
    auto_switch_osk: 'bool',
    allow_any_caller: 'bool',
};
const LANGUAGE_KEYS = {code: 'string', lexicon: 'string'};

/**
 * @param {string|null} text - the file's contents; null when it is missing
 * @returns {{ok: true, config: object} | {ok: false, error: string}}
 */
export function parseConfig(text) {
    try {
        return {ok: true, config: parse(text ?? '')};
    } catch (e) {
        if (e instanceof ParseError)
            return {ok: false, error: e.message};
        throw e;
    }
}

function parse(text) {
    const config = defaultConfig();
    const toks = tokenize(text);
    let p = 0;
    const peek = () => toks[p];
    const next = () => toks[p++];
    const expect = (type, what) => {
        const t = next();
        if (t.type !== type)
            fail(t.line, `expected ${what}`);
        return t;
    };
    const endOfLine = () => {
        const t = next();
        if (t.type !== 'nl')
            fail(t.line, 'expected the end of the line');
    };

    let languages = null; // set by the first [[languages]]
    let legacyAutoSwitch = null;
    let table = null; // the [[languages]] entry being filled, or null at top level
    const seenTop = new Set();
    const tables = [];

    const value = (kind, line) => {
        const t = next();
        if (kind === 'bool') {
            if (t.type === 'bare' && (t.value === 'true' || t.value === 'false'))
                return t.value === 'true';
            fail(line, 'expected true or false');
        }
        if (kind === 'string') {
            if (t.type === 'str')
                return t.value;
            fail(line, 'expected a quoted string');
        }
        // An array of strings, possibly over several lines.
        if (t.type !== '[')
            fail(line, 'expected a list like ["a", "b"]');
        const out = [];
        for (;;) {
            while (peek().type === 'nl') {
                if (p === toks.length - 1)
                    fail(line, 'unterminated array');
                next();
            }
            const u = next();
            if (u.type === ']')
                return out;
            if (u.type !== 'str')
                fail(u.line, 'a list may only hold quoted strings');
            out.push(u.value);
            while (peek().type === 'nl') {
                if (p === toks.length - 1)
                    fail(line, 'unterminated array');
                next();
            }
            const sep = next();
            if (sep.type === ']')
                return out;
            if (sep.type !== ',')
                fail(sep.line, 'expected , or ] in the list');
        }
    };

    while (p < toks.length) {
        const t = next();
        if (t.type === 'nl')
            continue;
        if (t.type === '[[') {
            const name = expect('bare', 'a table name');
            expect(']]', ']]');
            endOfLine();
            if (name.value !== 'languages')
                fail(t.line, `unknown table [[${name.value}]]`);
            languages ??= [];
            table = {};
            tables.push({table, line: t.line});
            languages.push(table);
            continue;
        }
        if (t.type === '[')
            fail(t.line, 'only [[languages]] tables are supported');
        if (t.type !== 'bare')
            fail(t.line, 'expected a setting name');
        expect('=', '=');
        const key = t.value;
        if (table) {
            const kind = LANGUAGE_KEYS[key];
            if (!kind)
                fail(t.line, `unknown language setting '${key}'`);
            if (key in table)
                fail(t.line, `'${key}' is set twice in one language`);
            table[key] = value(kind, t.line);
        } else {
            const kind = TOP_KEYS[key];
            if (!kind)
                fail(t.line, `unknown setting '${key}'`);
            if (seenTop.has(key))
                fail(t.line, `'${key}' is set twice`);
            seenTop.add(key);
            const v = value(kind, t.line);
            if (key === 'allowlist')
                config.allowlist = v;
            else if (key === 'broken')
                config.broken = v;
            else if (key === 'osk_source')
                config.oskSource = v;
            else if (key === 'auto_switch_osk')
                legacyAutoSwitch = v;
            else
                config.allowAnyCaller = v;
        }
        endOfLine();
    }

    // The older boolean only counts when osk_source is absent.
    if (legacyAutoSwitch !== null && !seenTop.has('osk_source'))
        config.oskSource = legacyAutoSwitch ? US_ENGINE : '';

    if (languages) {
        for (const {table: l, line} of tables) {
            if (!('code' in l) || !('lexicon' in l))
                fail(line, 'each [[languages]] needs both code and lexicon');
        }
        config.languages = languages.map(l => ({code: l.code, lexicon: l.lexicon}));
    }
    return config;
}

// --- writing ---

function quote(s) {
    let out = '"';
    for (const ch of s) {
        const cp = ch.codePointAt(0);
        if (ch === '"' || ch === '\\')
            out += `\\${ch}`;
        else if (cp < 0x20 || cp === 0x7f)
            out += `\\u${cp.toString(16).padStart(4, '0')}`;
        else
            out += ch;
    }
    return `${out}"`;
}

function list(key, items) {
    if (items.length === 0)
        return `${key} = []\n`;
    return `${key} = [\n${items.map(i => `    ${quote(i)},\n`).join('')}]\n`;
}

/**
 * The whole file for `config`. Every key is written (so the file says what
 * the engine does), except allow_any_caller, which only appears when on.
 */
export function serializeConfig(config) {
    let s = '# correctiond settings, written by the Correctiond Companion preferences\n' +
        '# (Extensions > Correctiond Companion > Settings). The engine reads this file\n' +
        '# when it starts: restart it from that window to apply changes.\n' +
        '\n' +
        '# Input source while GJS OSK is open, so its keys type what they show\n' +
        '# ("" = keep the current one). The old source returns when it closes.\n' +
        `osk_source = ${quote(config.oskSource)}\n` +
        '# Older companions read only this: true = switch to correctiond:us.\n' +
        `auto_switch_osk = ${config.oskSource === US_ENGINE ? 'true' : 'false'}\n` +
        '\n' +
        '# Apps (window class / Wayland app id, case-insensitive) where typos are\n' +
        '# fixed. Every other app only learns. Terminals get prose-only fixes.\n' +
        list('allowlist', config.allowlist) +
        '\n' +
        '# Apps whose text editing breaks corrections: they only learn.\n' +
        list('broken', config.broken);
    if (config.allowAnyCaller) {
        s += '\n# Any session-bus client may use the control interface (testing only).\n' +
            'allow_any_caller = true\n';
    }
    for (const l of config.languages)
        s += `\n[[languages]]\ncode = ${quote(l.code)}\nlexicon = ${quote(l.lexicon)}\n`;
    return s;
}
