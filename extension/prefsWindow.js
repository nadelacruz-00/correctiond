// The preferences window: Extensions (or Extension Manager) > Correctiond
// Companion > Settings. It edits ~/.config/correctiond/config.toml through
// config.js and never touches Quick Settings. Kept apart from prefs.js (which
// needs the Extensions app's resources) so test/prefs.smoke.js can build it
// in plain gjs.

import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk';

import {
    BUILTIN_EN,
    MAX_LANGUAGES,
    normalizeAppId,
    parseConfig,
    serializeConfig,
    validLangCode,
} from './config.js';
import {effectiveTouch, gjsOskInstalledIn} from './lib.js';
import {addWordsPage} from './wordsPage.js';

const BUS_NAME = 'org.nadelacruz.Correctiond1';

/** Choices for the OSK layout (config.toml osk_source). */
const OSK_CHOICES = [
    {engine: 'correctiond:colemak', label: 'Colemak (English, autocorrect)'},
    {engine: 'correctiond:us', label: 'QWERTY (English US, autocorrect)'},
    {engine: '', label: 'Don\u2019t change the layout'},
];
const OBJECT_PATH = '/org/nadelacruz/Correctiond';

/** Layout the Learning page shows when the active input source is not one of ours. */
const DEFAULT_LAYOUT = 'qwerty';
const LAYOUT_BY_SOURCE = {'correctiond:colemak': 'colemak', 'correctiond:us': 'qwerty'};
const LAYOUT_NAMES = {qwerty: 'QWERTY', colemak: 'Colemak', osk: 'on-screen keyboard'};

export function defaultConfigPath() {
    return GLib.build_filenamev([GLib.get_user_config_dir(), 'correctiond', 'config.toml']);
}

function expandTilde(path) {
    if (path === '~')
        return GLib.get_home_dir();
    return path.startsWith('~/') ? GLib.build_filenamev([GLib.get_home_dir(), path.slice(2)]) : path;
}

/**
 * The codes from ['tl', 'war'] (in that order) whose lexicon file exists in
 * `dataDir`/correctiond/lexicons, so the defaults keep a user's old languages.
 */
export function legacyLangs(dataDir = GLib.get_user_data_dir()) {
    return ['tl', 'war'].filter(code => GLib.file_test(
        GLib.build_filenamev([dataDir, 'correctiond', 'lexicons', `${code}.txt`]),
        GLib.FileTest.IS_REGULAR));
}

export function defaultReportPath() {
    return GLib.build_filenamev([GLib.get_user_data_dir(), 'correctiond', 'import-report.json']);
}

/**
 * Reads the import report: {state: 'missing'} when there is no file,
 * {state: 'invalid'} when it cannot be read or is not a JSON object, else
 * {state: 'ok', report}.
 */
export function loadReport(path) {
    let bytes;
    try {
        [, bytes] = GLib.file_get_contents(path);
    } catch (e) {
        return {state: e.matches?.(GLib.FileError, GLib.FileError.NOENT) ? 'missing' : 'invalid'};
    }
    try {
        const report = JSON.parse(new TextDecoder().decode(bytes));
        if (report === null || typeof report !== 'object' || Array.isArray(report))
            return {state: 'invalid'};
        return {state: 'ok', report};
    } catch (e) {
        return {state: 'invalid'};
    }
}

/** True when the GJS OSK extension is installed in a user or system data dir. */
export function gjsOskOnDisk() {
    return gjsOskInstalledIn([GLib.get_user_data_dir(), ...GLib.get_system_data_dirs()],
        p => GLib.file_test(p, GLib.FileTest.IS_DIR));
}

/**
 * The active input source: `mru-sources[0]` (GNOME 50 ignores `current`),
 * else `sources[0]`, else null.
 * @param {Gio.Settings} settings org.gnome.desktop.input-sources
 * @returns {[string, string]|null}
 */
export function currentSource(settings) {
    const mru = settings.get_value('mru-sources').deepUnpack();
    if (mru.length > 0)
        return mru[0];
    return settings.get_value('sources').deepUnpack()[0] ?? null;
}

/**
 * The layout whose numbers the Learning page leads with: the active input
 * source (mru-sources[0]) mapped through LAYOUT_BY_SOURCE (correctiond:colemak -> colemak,
 * correctiond:us -> qwerty), else qwerty. Any failure reading GSettings
 * (schema missing, odd values) also gives qwerty.
 */
export function currentLayout() {
    try {
        const src = Gio.SettingsSchemaSource.get_default();
        if (!src?.lookup('org.gnome.desktop.input-sources', true))
            return DEFAULT_LAYOUT;
        const settings = new Gio.Settings({schema_id: 'org.gnome.desktop.input-sources'});
        const entry = currentSource(settings);
        return LAYOUT_BY_SOURCE[entry?.[1]] ?? DEFAULT_LAYOUT;
    } catch (e) {
        return DEFAULT_LAYOUT;
    }
}

const isNum = v => typeof v === 'number' && Number.isFinite(v);
/** A fraction as a percentage with one decimal; anything else is "n/a". */
export const fmtRate = v => (isNum(v) ? `${(v * 100).toFixed(1)}%` : 'n/a');
const fmtCount = v => (isNum(v) ? v.toLocaleString() : 'n/a');
const fmtDate = v => {
    const d = typeof v === 'string' ? GLib.DateTime.new_from_iso8601(v, null) : null;
    return d ? d.to_local().format('%Y-%m-%d') : 'n/a';
};

/** "<rebuild> (before: <baseline>)" for one metric of a replay mode. */
const rateWithBaseline = (mode, key) =>
    `${fmtRate(mode?.rebuild?.[key])} (before: ${fmtRate(mode?.baseline?.[key])})`;

/** "Built in", "N words", or a missing/unreadable note for one lexicon. */
export function lexiconStatus(lexicon) {
    if (lexicon === BUILTIN_EN)
        return {ok: true, text: 'Built in'};
    const path = expandTilde(lexicon);
    let bytes;
    try {
        [, bytes] = GLib.file_get_contents(path);
    } catch (e) {
        return {ok: false, text: 'File not found: words in this language may be corrected into English'};
    }
    const text = new TextDecoder().decode(bytes);
    const header = /^# lexicon lang=\S+ entries=(\d+)/.exec(text);
    const n = header
        ? Number(header[1])
        : text.split('\n').filter(l => l && !l.startsWith('#')).length;
    return {ok: true, text: `${n.toLocaleString()} words`};
}

/** Installed apps with the id the engine matches on (StartupWMClass, else the desktop id). */
function installedApps() {
    const apps = [];
    const seen = new Set();
    for (const info of Gio.AppInfo.get_all()) {
        if (!info.should_show())
            continue;
        // Desktop-file apps have StartupWMClass; checked by method so the
        // deprecated Gio.DesktopAppInfo class is never looked up.
        const wm = typeof info.get_startup_wm_class === 'function' ? info.get_startup_wm_class() : null;
        const id = normalizeAppId(wm || (info.get_id() ?? '').replace(/\.desktop$/, ''));
        if (!id || seen.has(id.toLowerCase()))
            continue;
        seen.add(id.toLowerCase());
        apps.push({id, name: info.get_display_name() ?? id, icon: info.get_icon()});
    }
    apps.sort((a, b) => a.name.localeCompare(b.name));
    return apps;
}

/**
 * Fills `window` (an Adw.PreferencesWindow) and returns the controller the
 * widgets drive; the smoke test drives it directly.
 *
 * @param {Adw.PreferencesWindow} window
 * @param {{configPath?: string, restartArgv?: string[], apps?: object[], reportPath?: string,
 *   layout?: string, gjsOskInstalled?: boolean, dataDir?: string, companionCall?: (callback: (err: Error|null) => void) => void,
 *   wordsCalls?: object}} opts - wordsCalls: see addWordsPage in wordsPage.js
 */
export function fillWindow(window, opts = {}) {
    const configPath = opts.configPath ?? defaultConfigPath();
    const restartArgv = opts.restartArgv ?? ['ibus', 'restart'];
    const apps = opts.apps ?? installedApps();
    const appById = new Map(apps.map(a => [a.id.toLowerCase(), a]));

    // --- model and file ---

    let text = null;
    const gjsOskInstalled = opts.gjsOskInstalled ?? gjsOskOnDisk();
    try {
        const [, bytes] = GLib.file_get_contents(configPath);
        text = new TextDecoder().decode(bytes);
    } catch (e) {
        if (!e.matches?.(GLib.FileError, GLib.FileError.NOENT))
            text = undefined; // unreadable: treated like unparsable below
    }
    const parsed = text === undefined
        ? {ok: false, error: 'the file cannot be read'}
        : parseConfig(text, {legacyLangs: legacyLangs(opts.dataDir)});
    const readOnly = !parsed.ok;
    const model = parsed.ok ? parsed.config : null;

    const toast = title => {
        try {
            window.add_toast(new Adw.Toast({title, timeout: 3, use_markup: false}));
        } catch (e) {
            log(`correctiond-companion prefs: ${title}`);
        }
    };

    const banners = [];
    const setRestartPending = on => {
        for (const b of banners)
            b.revealed = on;
    };

    const save = ({needsRestart = true} = {}) => {
        if (readOnly)
            return false;
        try {
            GLib.mkdir_with_parents(GLib.path_get_dirname(configPath), 0o700);
            Gio.File.new_for_path(configPath).replace_contents(
                new TextEncoder().encode(serializeConfig(model)), null, false,
                Gio.FileCreateFlags.REPLACE_DESTINATION | Gio.FileCreateFlags.PRIVATE, null);
        } catch (e) {
            toast('Could not save the settings file');
            logError(e, 'correctiond-companion prefs: save');
            return false;
        }
        if (needsRestart)
            setRestartPending(true);
        return true;
    };

    const controller = {
        configPath,
        readOnly,
        error: parsed.ok ? null : parsed.error,
        model,
        refreshers: [],
        refresh() {
            for (const r of this.refreshers)
                r();
        },
        addApp(listKey, raw) {
            if (readOnly)
                return false;
            const id = normalizeAppId(raw);
            if (!id) {
                toast('Not a valid app ID');
                return false;
            }
            const list = model[listKey];
            if (list.some(x => x.toLowerCase() === id.toLowerCase())) {
                toast('Already in the list');
                return false;
            }
            list.push(id);
            this.refresh();
            return save();
        },
        removeApp(listKey, id) {
            if (readOnly)
                return false;
            model[listKey] = model[listKey].filter(x => x !== id);
            this.refresh();
            return save();
        },
        setOskSource(engine) {
            if (readOnly)
                return false;
            if (model.oskSource === engine)
                return true;
            model.oskSource = engine;
            // The extension rereads this each time the OSK opens.
            return save({needsRestart: false});
        },
        setTouch(on) {
            if (readOnly)
                return false;
            model.modules.touch = on;
            model.touchExplicit = true;
            // The Shell extension watches the file and applies it live.
            const ok = save({needsRestart: false});
            updateOsk();
            return ok;
        },
        addLanguage(code, lexicon) {
            if (readOnly)
                return false;
            code = (code ?? '').trim();
            if (!validLangCode(code)) {
                toast('Use a short lowercase language code, like ceb');
                return false;
            }
            if (model.languages.some(l => l.code === code)) {
                toast(`“${code}” is already listed`);
                return false;
            }
            if (model.languages.length >= MAX_LANGUAGES) {
                toast(`At most ${MAX_LANGUAGES} languages`);
                return false;
            }
            model.languages.push({code, lexicon});
            this.refresh();
            return save();
        },
        removeLanguage(code) {
            if (readOnly)
                return false;
            if (model.languages.length <= 1) {
                toast('Keep at least one language');
                return false;
            }
            model.languages = model.languages.filter(l => l.code !== code);
            this.refresh();
            return save();
        },
        restartEngine() {
            try {
                const proc = Gio.Subprocess.new(restartArgv, Gio.SubprocessFlags.NONE);
                proc.wait_check_async(null, (p, res) => {
                    try {
                        p.wait_check_finish(res);
                        setRestartPending(false);
                        toast('Autocorrect engine restarted');
                        GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 3, () => {
                            this.refresh();
                            return GLib.SOURCE_REMOVE;
                        });
                    } catch (e) {
                        toast('Could not restart the engine');
                    }
                });
            } catch (e) {
                toast('Could not restart the engine');
            }
        },
    };

    // --- shared widgets ---

    const makeBanner = () => {
        const banner = new Adw.Banner({
            title: 'Restart the autocorrect engine to apply changes',
            button_label: 'Restart',
            revealed: false,
            use_markup: false,
        });
        banner.connect('button-clicked', () => controller.restartEngine());
        banners.push(banner);
        return banner;
    };

    const errorBanner = () => new Adw.Banner({
        title: `Settings file not editable here (${controller.error}). Fix it by hand in ${configPath}`,
        revealed: true,
        use_markup: false,
    });

    const removeButton = (tooltip, onClick) => {
        const b = new Gtk.Button({
            icon_name: 'user-trash-symbolic',
            tooltip_text: tooltip,
            valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        b.connect('clicked', onClick);
        return b;
    };

    const page = (title, icon, {withBanner = true} = {}) => {
        const p = new Adw.PreferencesPage({title, icon_name: icon});
        if (withBanner)
            p.set_banner(readOnly ? errorBanner() : makeBanner());
        window.add(p);
        return p;
    };

    // --- General ---

    const general = page('General', 'preferences-system-symbolic');

    const statusGroup = new Adw.PreferencesGroup({title: 'Status'});
    general.add(statusGroup);
    const statusRow = new Adw.ActionRow({title: 'Autocorrect engine', subtitle: 'Checking…', use_markup: false});
    const refreshBtn = new Gtk.Button({
        icon_name: 'view-refresh-symbolic',
        tooltip_text: 'Refresh',
        valign: Gtk.Align.CENTER,
        css_classes: ['flat'],
    });
    statusRow.add_suffix(refreshBtn);
    statusGroup.add(statusRow);
    const restartRow = new Adw.ActionRow({
        title: 'Restart engine',
        subtitle: 'Applies app and language changes. Typing pauses for a moment.',
        use_markup: false,
    });
    const restartBtn = new Gtk.Button({label: 'Restart', valign: Gtk.Align.CENTER});
    restartBtn.connect('clicked', () => controller.restartEngine());
    restartRow.add_suffix(restartBtn);
    restartRow.activatable_widget = restartBtn;
    statusGroup.add(restartRow);

    let updateOsk = () => {}; // set with the On-screen keyboard group below

    let wordsLearned = null; // from GetStats; null while the engine is not running
    const refreshStatus = () => {
        updateOsk();
        Gio.DBus.session.call(BUS_NAME, OBJECT_PATH, BUS_NAME, 'GetStats', null,
            new GLib.VariantType('(s)'), Gio.DBusCallFlags.NO_AUTO_START, 2000, null,
            (conn, res) => {
                try {
                    const [json] = conn.call_finish(res).deep_unpack();
                    const s = JSON.parse(json);
                    controller.setWordsLearned(s.words_learned);
                    const store = s.persistent
                        ? 'saved encrypted'
                        : 'kept in memory only (keyring locked or unavailable)';
                    statusRow.subtitle = `Running. ${s.words_learned} words learned, ${store}. ` +
                        `${s.corrections} corrections since start.`;
                } catch (e) {
                    controller.setWordsLearned(null);
                    statusRow.subtitle = 'Not running. Select an “autocorrect” input source.';
                }
            });
    };
    refreshBtn.connect('clicked', refreshStatus);
    controller.refreshers.push(refreshStatus);
    refreshStatus();

    const oskGroup = new Adw.PreferencesGroup({title: 'On-screen keyboard'});
    general.add(oskGroup);
    const touchRow = new Adw.SwitchRow({
        title: 'On-screen keyboard support',
        subtitle: "For the GJS OSK extension. GNOME's built-in keyboard isn't supported yet.",
        active: effectiveTouch(model?.modules.touch ?? null, gjsOskInstalled),
        sensitive: !readOnly,
        use_markup: false,
    });
    oskGroup.add(touchRow);
    controller.touchRow = touchRow;
    controller.oskGroup = oskGroup;
    const oskChoices = OSK_CHOICES.slice();
    const current = model?.oskSource ?? 'correctiond:us';
    if (!oskChoices.some(c => c.engine === current))
        oskChoices.push({engine: current, label: `Other: ${current}`});
    const oskRow = new Adw.ComboRow({
        title: 'Keyboard layout while GJS OSK is open',
        subtitle: 'Pick the layout your GJS OSK keyboard is built for, so each key types the ' +
            'letter it shows. Your own layout comes back when the OSK closes.',
        model: Gtk.StringList.new(oskChoices.map(c => c.label)),
        selected: oskChoices.findIndex(c => c.engine === current),
        sensitive: !readOnly,
        use_markup: false,
    });
    oskRow.connect('notify::selected', () => controller.setOskSource(oskChoices[oskRow.selected].engine));
    oskGroup.add(oskRow);
    controller.oskRow = oskRow;
    const oskRowSubtitle = oskRow.subtitle;
    updateOsk = () => {
        const touchOn = effectiveTouch(model?.modules.touch ?? null, gjsOskInstalled);
        oskGroup.visible = gjsOskInstalled || touchOn;
        oskGroup.description = touchOn && !gjsOskInstalled ? 'GJS OSK is not installed' : '';
        oskRow.sensitive = !readOnly && touchOn;
        oskRow.subtitle = touchOn ? oskRowSubtitle : 'Turn on on-screen keyboard support first';
    };
    updateOsk();
    touchRow.connect('notify::active', () => controller.setTouch(touchRow.active));

    const fileGroup = new Adw.PreferencesGroup({
        title: 'Settings file',
        description: '“Forget learned words” stays in the Autocorrect menu of Quick Settings.',
    });
    general.add(fileGroup);
    const fileRow = new Adw.ActionRow({title: 'config.toml', subtitle: configPath, use_markup: false});
    const openBtn = new Gtk.Button({
        icon_name: 'document-open-symbolic',
        tooltip_text: 'Open in the default editor',
        valign: Gtk.Align.CENTER,
        css_classes: ['flat'],
    });
    openBtn.connect('clicked', () => {
        try {
            Gio.AppInfo.launch_default_for_uri(GLib.filename_to_uri(configPath, null), null);
        } catch (e) {
            toast('No settings file yet: change a setting first');
        }
    });
    fileRow.add_suffix(openBtn);
    fileGroup.add(fileRow);

    // --- Apps ---

    const appsPage = page('Apps', 'application-x-executable-symbolic');

    const pickApp = onPick => {
        const list = new Gtk.ListBox({selection_mode: Gtk.SelectionMode.NONE, css_classes: ['boxed-list']});
        const search = new Gtk.SearchEntry({placeholder_text: 'Search apps', margin_bottom: 12});
        let query = '';
        for (const a of apps) {
            const row = new Adw.ActionRow({title: a.name, subtitle: a.id, activatable: true, use_markup: false});
            if (a.icon)
                row.add_prefix(new Gtk.Image({gicon: a.icon, pixel_size: 32}));
            row._appId = a.id;
            row._haystack = `${a.name} ${a.id}`.toLowerCase();
            list.append(row);
        }
        list.set_filter_func(row => !query || row._haystack.includes(query));
        search.connect('search-changed', () => {
            query = search.text.trim().toLowerCase();
            list.invalidate_filter();
        });
        const box = new Gtk.Box({
            orientation: Gtk.Orientation.VERTICAL,
            margin_top: 12, margin_bottom: 12, margin_start: 12, margin_end: 12,
        });
        box.append(search);
        box.append(new Gtk.ScrolledWindow({child: list, vexpand: true, hscrollbar_policy: Gtk.PolicyType.NEVER}));
        const view = new Adw.ToolbarView({content: box});
        view.add_top_bar(new Adw.HeaderBar());
        const dialog = new Adw.Dialog({title: 'Choose an app', content_width: 440, content_height: 560, child: view});
        list.connect('row-activated', (_l, row) => {
            onPick(row._appId);
            dialog.close();
        });
        dialog.present(window);
    };

    const appListGroup = (listKey, title, description) => {
        const group = new Adw.PreferencesGroup({title, description, sensitive: !readOnly});
        appsPage.add(group);
        let rows = [];
        const rebuild = () => {
            for (const r of rows)
                group.remove(r);
            rows = [];
            for (const id of model?.[listKey] ?? []) {
                const app = appById.get(id.toLowerCase());
                const row = new Adw.ActionRow({
                    title: app ? app.name : id,
                    subtitle: app ? id : 'Not an installed app ID',
                    use_markup: false,
                });
                if (app?.icon)
                    row.add_prefix(new Gtk.Image({gicon: app.icon, pixel_size: 32}));
                row.add_suffix(removeButton('Remove', () => controller.removeApp(listKey, id)));
                rows.push(row);
            }
            const pick = new Adw.ButtonRow({title: 'Add an installed app…', start_icon_name: 'list-add-symbolic'});
            pick.connect('activated', () => pickApp(id => controller.addApp(listKey, id)));
            rows.push(pick);
            const entry = new Adw.EntryRow({title: 'Or type an app ID (window class)', show_apply_button: true});
            entry.connect('apply', () => {
                if (controller.addApp(listKey, entry.text))
                    entry.text = '';
            });
            rows.push(entry);
            for (const r of rows)
                group.add(r);
        };
        controller.refreshers.push(rebuild);
        rebuild();
        return group;
    };

    controller.allowlistGroup = appListGroup('allowlist', 'Fix typos in these apps',
        'Every other app only learns your words. Terminals here get prose-only fixes and never learn.');
    controller.brokenGroup = appListGroup('broken', 'Learn only, never fix',
        'For apps whose text editing breaks corrections.');

    // --- Languages ---

    const langPage = page('Languages', 'preferences-desktop-locale-symbolic');
    const langGroup = new Adw.PreferencesGroup({
        title: 'Dictionaries',
        description: 'A word found in any of these is never corrected. A lexicon file ' +
            'holds one word, a tab and its count per line (see scripts/build-lexicon.py).',
        sensitive: !readOnly,
    });
    langPage.add(langGroup);
    let langRows = [];
    const rebuildLanguages = () => {
        for (const r of langRows)
            langGroup.remove(r);
        langRows = [];
        for (const l of model?.languages ?? []) {
            const status = lexiconStatus(l.lexicon);
            const row = new Adw.ActionRow({
                title: l.code,
                subtitle: `${l.lexicon === BUILTIN_EN ? 'English list in the engine' : l.lexicon}\n${status.text}`,
                subtitle_lines: 3,
                use_markup: false,
            });
            if (!status.ok)
                row.add_prefix(new Gtk.Image({icon_name: 'dialog-warning-symbolic', css_classes: ['warning']}));
            row.add_suffix(removeButton('Remove language', () => controller.removeLanguage(l.code)));
            langRows.push(row);
        }
        const codeRow = new Adw.EntryRow({title: 'New language code (for example ceb)'});
        const add = new Adw.ButtonRow({title: 'Choose its lexicon file…', start_icon_name: 'list-add-symbolic'});
        add.connect('activated', () => {
            const code = codeRow.text.trim();
            if (!validLangCode(code)) {
                toast('Type a short lowercase language code first, like ceb');
                return;
            }
            const fd = new Gtk.FileDialog({title: `Lexicon for “${code}”`});
            fd.open(window, null, (d, res) => {
                let file;
                try {
                    file = d.open_finish(res);
                } catch (e) {
                    return; // dismissed
                }
                if (file?.get_path())
                    controller.addLanguage(code, file.get_path());
            });
        });
        langRows.push(codeRow, add);
        for (const r of langRows)
            langGroup.add(r);
    };
    controller.refreshers.push(rebuildLanguages);
    rebuildLanguages();
    controller.langGroup = langGroup;

    // --- Learning ---

    // Counts only, read from the import report. Labels never use markup.
    const learnPage = page('Learning', 'view-list-bullet-symbolic', {withBanner: false});
    const reportPath = opts.reportPath ?? defaultReportPath();
    const layout = opts.layout ?? currentLayout();
    let learnGroups = [];
    controller.reportPath = reportPath;
    controller.learningRows = new Map(); // title -> row, for the smoke test

    const statRow = (group, title, value) => {
        const row = new Adw.ActionRow({title, subtitle: value, use_markup: false});
        controller.learningRows.set(title, row);
        group.add(row);
    };
    const learnGroup = (title, description) => {
        const g = new Adw.PreferencesGroup({title, description: description ?? ''});
        learnPage.add(g);
        learnGroups.push(g);
        return g;
    };

    // Forget all: the Shell's confirm dialog does the work (it holds the engine
    // call), so this row only asks the extension to open it.
    const defaultCompanionCall = callback => Gio.DBus.session.call(
        'org.gnome.Shell', '/org/nadelacruz/CorrectiondCompanion',
        'org.nadelacruz.CorrectiondCompanion1', 'ConfirmForget', null, null,
        Gio.DBusCallFlags.NONE, 2000, null, (conn, res) => {
            try {
                conn.call_finish(res);
                callback(null);
            } catch (e) {
                callback(e);
            }
        });
    const companionCall = opts.companionCall ?? defaultCompanionCall;
    const forgetGroup = new Adw.PreferencesGroup();
    // An ActionRow (ButtonRow has no subtitle for the failure message).
    const forgetRow = new Adw.ActionRow({
        title: 'Forget all learned words\u2026', use_markup: false, activatable: true,
    });
    forgetRow.add_suffix(new Gtk.Image({icon_name: 'edit-delete-symbolic'}));
    forgetGroup.add(forgetRow);
    // Added by rebuildLearning, after the report groups: it goes last.
    controller.forgetRow = forgetRow;
    controller.forgetAll = () => {
        const unavailable = () => {
            forgetRow.subtitle = 'Turn on the Autocorrect extension to use this';
        };
        forgetRow.subtitle = '';
        try {
            companionCall(err => {
                if (err)
                    unavailable();
            });
        } catch (e) {
            unavailable();
        }
    };
    forgetRow.connect('activated', () => controller.forgetAll());

    // The report groups; rebuildLearning puts the forget group after them.
    const addReportGroups = () => {
        const {state, report} = loadReport(reportPath);
        if (state !== 'ok') {
            const g = learnGroup('Chat import');
            statRow(g, state === 'missing' ? 'No chat import yet' : 'The import report could not be read',
                state === 'missing' ? 'Nothing has been imported from chat exports.' : reportPath);
            return;
        }

        const chat = learnGroup('Chat import');
        if (isNum(wordsLearned) && isNum(report.words) && wordsLearned < report.words)
            statRow(chat, 'Learned words were forgotten after this import',
                'Import again to bring them back.');
        statRow(chat, 'Imported', fmtDate(report.imported_at));
        statRow(chat, 'Your messages', fmtCount(report.own_messages));
        statRow(chat, 'Threads', fmtCount(report.threads));

        const mode = report.replay?.[layout]?.full;
        const flows = learnGroup('Waray just flows',
            `Typing on ${LAYOUT_NAMES[layout] ?? layout}, all words counted. Lower changed-rates are better.`);
        statRow(flows, 'Waray and Taglish words changed', rateWithBaseline(mode, 'chat_changed_rate'));
        statRow(flows, 'Typos fixed', rateWithBaseline(mode, 'typo_fix_rate'));
        statRow(flows, 'Slang changed', rateWithBaseline(mode, 'slang_changed_rate'));
        // gate is 'passed' or 'failed' (an object with a result field is also accepted).
        const gate = typeof report.gate === 'string' ? report.gate : report.gate?.result;
        const failed = report.failed_checks ?? report.gate?.failed_checks;
        const checks = Array.isArray(failed) && failed.length > 0
            ? `: ${failed.filter(c => typeof c === 'string').join(', ')}` : '';
        statRow(flows, 'Quality check',
            gate === 'passed' ? 'Passed' : gate === 'failed' ? `Failed${checks}` : 'n/a');

        const words = learnGroup('Your words');
        statRow(words, 'Words imported', fmtCount(report.words));
        statRow(words, 'Pairs imported', fmtCount(report.pairs));
        const installed = report.lists_installed === true;
        statRow(words, 'Tagalog words added to the list', installed ? fmtCount(report.tl_added) : '0');
        statRow(words, 'Waray words added to the list', installed ? fmtCount(report.war_added) : '0');
        if (!installed)
            statRow(words, 'Word lists were not installed',
                'The quality check failed or the lists were left as they were.');

        const left = learnGroup('Left out');
        statRow(left, 'Names skipped', fmtCount(report.names_skipped));
    };
    let forgetOnPage = false;
    const rebuildLearning = () => {
        for (const g of learnGroups)
            learnPage.remove(g);
        learnGroups = [];
        controller.learningRows.clear();
        if (forgetOnPage)
            learnPage.remove(forgetGroup);
        addReportGroups();
        learnPage.add(forgetGroup);
        forgetOnPage = true;
    };
    controller.refreshers.push(rebuildLearning);
    controller.setWordsLearned = n => {
        wordsLearned = isNum(n) ? n : null;
        rebuildLearning();
    };
    rebuildLearning();

    // --- Words ---

    controller.words = addWordsPage(window, opts);

    controller.banners = banners;
    return controller;
}
