// Test-only windows for scripts/readme-screenshots.sh. Never packed or
// installed. It runs as a client of that script's private headless GNOME
// Shell (WAYLAND_DISPLAY there), on its private session bus, in a temp HOME.
//
//   gjs -m readme_screens.js editor
//     A plain "Text Editor" window (app id org.gnome.TextEditor, so the
//     Shell names it after the system's Text Editor) with a few invented
//     lines, to have focus behind Quick Settings and the OSK.
//   gjs -m readme_screens.js prefs CONFIG REPORT
//     The real Settings window (fillWindow in extension/prefsWindow.js) on
//     the synthetic CONFIG and REPORT, with stub wordsCalls that allow at once
//     and list invented words, so the Words page needs no Shell prompt. Reads
//     `PAGE [HEIGHT]` lines from stdin (PAGE: General, Apps, Languages,
//     Learning or Words) and shows each, the window HEIGHT px tall.
//
// Each prints `SCREENS ready <pid>` once its window is presented, and
// `SCREENS page <name>` once a page is shown. It stays alive until killed.

import Adw from 'gi://Adw?version=1';
import Gio from 'gi://Gio';
import GioUnix from 'gi://GioUnix';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk?version=4.0';
import System from 'system';

import {fillWindow} from '../extension/prefsWindow.js';

const [mode, configPath, reportPath] = System.programArgs;
const say = line => print(`SCREENS ${line}`);
const pid = () => new Gio.Credentials().get_unix_pid();

const DAY = 86400;
// Invented words: common public Tagalog and Waray words, no names.
// [word, count, days since last used]
const WORDS = [
    ['kadto', 412, 0], ['sige', 388, 0], ['ayaw', 251, 1], ['kuya', 197, 1],
    ['grabe', 164, 2], ['salamat', 143, 3], ['maupay', 96, 4], ['gihapon', 71, 6],
    ['talaga', 58, 9], ['ngani', 44, 12], ['bahala', 29, 17], ['ambot', 17, 24],
];

function* descendants(widget) {
    for (let c = widget.get_first_child(); c; c = c.get_next_sibling()) {
        yield c;
        yield* descendants(c);
    }
}

function readLines(onLine) {
    const stdin = new Gio.DataInputStream({
        base_stream: new GioUnix.InputStream({fd: 0, close_fd: false}),
    });
    const next = () => stdin.read_line_async(GLib.PRIORITY_DEFAULT, null, (s, res) => {
        const [line] = s.read_line_finish_utf8(res);
        if (line === null)
            return; // EOF: stay alive until killed
        onLine(line.trim());
        next();
    });
    next();
}

function editor() {
    const app = new Adw.Application({application_id: 'org.gnome.TextEditor'});
    app.connect('activate', () => {
        const buffer = new Gtk.TextBuffer({
            text: 'Kadto kami ha merkado buwas.\n' +
                'Sige, ayaw kalimti an listahan.\n' +
                'Grabe an uran kahapon, kuya.\n\n' +
                'Meeting notes: send the draft on Friday.\n',
        });
        const view = new Gtk.TextView({
            buffer, wrap_mode: Gtk.WrapMode.WORD_CHAR,
            top_margin: 24, bottom_margin: 24, left_margin: 32, right_margin: 32,
        });
        view.add_css_class('inline');
        const toolbar = new Adw.ToolbarView({
            content: new Gtk.ScrolledWindow({child: view, vexpand: true}),
        });
        toolbar.add_top_bar(new Adw.HeaderBar({
            title_widget: new Adw.WindowTitle({title: 'Notes', subtitle: 'Draft'}),
        }));
        const win = new Adw.ApplicationWindow({
            application: app, title: 'Notes', default_width: 900, default_height: 620, content: toolbar,
        });
        win.present();
        // The cursor at the end of the last line, as when typing.
        buffer.place_cursor(buffer.get_end_iter());
        view.grab_focus();
        say(`ready ${pid()}`);
    });
    app.run([]);
}

function prefs() {
    Adw.init();
    Adw.StyleManager.get_default().color_scheme = Adw.ColorScheme.FORCE_DARK;
    const now = Math.floor(Date.now() / 1000);
    const words = WORDS.map(([w, c, days]) => [w, c, now - days * DAY - 3600, true]);
    const conn = {};
    const wordsCalls = {
        open: () => Promise.resolve(conn),
        requestAccess: () => Promise.resolve(true),
        listWords: () => Promise.resolve(words),
        forgetWords: (_c, ws) => Promise.resolve(ws.length),
        companionPresent: () => Promise.resolve(true),
        flush: () => Promise.resolve(),
        close: () => Promise.resolve(),
    };
    const icon = name => Gio.ThemedIcon.new_with_default_fallbacks(name);
    const apps = [
        {id: 'firefox', name: 'Firefox', icon: icon('firefox')},
        {id: 'org.gnome.TextEditor', name: 'Text Editor', icon: icon('org.gnome.TextEditor')},
        {id: 'chromium', name: 'Chromium', icon: icon('chromium')},
        {id: 'org.gnome.Ptyxis', name: 'Ptyxis', icon: icon('org.gnome.Ptyxis')},
    ];
    const win = new Adw.PreferencesWindow({title: 'Correctiond Companion'});
    win.set_default_size(840, 900);
    const ctl = fillWindow(win, {
        configPath, reportPath, apps, layout: 'qwerty', restartArgv: ['true'],
        companionCall: cb => cb(null), wordsCalls,
    });
    // The settings file row shows the path as a user sees it, not the temp HOME.
    for (const w of descendants(win)) {
        if (w instanceof Adw.ActionRow && w.title === 'config.toml')
            w.subtitle = '~/.config/correctiond/config.toml';
    }
    const pages = new Map([...descendants(win)]
        .filter(w => w instanceof Adw.PreferencesPage).map(p => [p.title, p]));
    let opened = false;
    readLines(line => {
        const [name, height] = line.split(/\s+/);
        const page = pages.get(name);
        if (!page) {
            say(`no-page ${name}`);
            return;
        }
        // The window's height, so each shot ends below its page's content.
        if (Number(height) > 0)
            win.set_default_size(840, Number(height));
        win.visible_page = page;
        if (name === 'Words' && !opened) {
            opened = true;
            ctl.words.showButton.activate();
        }
        // Let the page lay out (and the stub calls settle) first.
        GLib.timeout_add(GLib.PRIORITY_DEFAULT, 800, () => {
            say(`page ${name}`);
            return GLib.SOURCE_REMOVE;
        });
    });
    win.present();
    say(`ready ${pid()}`);
    new GLib.MainLoop(null, false).run();
}

if (mode === 'editor') {
    editor();
} else if (mode === 'prefs' && configPath && reportPath) {
    prefs();
} else {
    printerr('usage: gjs -m readme_screens.js editor | prefs CONFIG REPORT');
    System.exit(2);
}
