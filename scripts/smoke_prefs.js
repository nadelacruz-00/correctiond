// Test-only, for scripts/extension-smoke.sh: installed as the extension's
// prefs.js in the smoke's temp HOME only, with the real prefs.js beside it as
// prefs-real.js. Never packed or installed for real.
//
// GNOME's real preferences service (org.gnome.Shell.Extensions) loads it into
// its real window (an ExtensionPrefsDialog). It runs the real preferences
// class, so the real Words page and its real calls (defaultWordsCalls), and
// exports a small object on the service's own bus connection through which
// the smoke presses the page's widgets and reads them back:
//   ShowWords()  show the Words page and activate "Show learned words…"
//   Forget(s)    activate the row's Forget button for that word
//   State() -> s the page as JSON: status text, button, rows shown, title
//   Close()      close the window, as its close button does
// It prints `SMOKE-PREFS <event>` lines (ready, every title change) to stdout.

import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import Gtk from 'gi://Gtk';

import RealPreferences from './prefs-real.js';

const PATH = '/org/nadelacruz/CorrectiondSmokePrefs';
const IFACE = `<node><interface name="org.nadelacruz.CorrectiondSmokePrefs">
  <method name="ShowWords"/>
  <method name="Forget"><arg type="s" direction="in"/></method>
  <method name="State"><arg type="s" direction="out"/></method>
  <method name="Close"/>
</interface></node>`;

function* descendants(widget) {
    for (let c = widget.get_first_child(); c; c = c.get_next_sibling()) {
        yield c;
        yield* descendants(c);
    }
}
const find = (root, pred) => [...descendants(root)].find(pred) ?? null;

export default class SmokePreferences extends RealPreferences {
    fillPreferencesWindow(window) {
        super.fillPreferencesWindow(window);
        const page = find(window, w => w instanceof Adw.PreferencesPage && w.title === 'Words');
        const button = find(page, w => w instanceof Gtk.Button && w.label === 'Show learned words…');
        const status = button.get_parent().get_first_child();
        // The word list (the sort dropdown's popover has a ListView too).
        const list = find(page, w => w instanceof Gtk.ListView && w.has_css_class('rich-list'));
        const count = find(page, w => w instanceof Gtk.Label && /^[\d,]+ words?$/.test(w.label));
        // Row titles: the ListView's labels that are not subtitles, in mapped rows.
        const rows = () => [...descendants(list)]
            .filter(w => w instanceof Gtk.Label && !w.has_css_class('caption') && w.get_mapped())
            .map(w => w.label);
        const driver = Gio.DBusExportedObject.wrapJSObject(IFACE, {
            ShowWords() {
                window.visible_page = page;
                button.activate();
            },
            Forget(word) {
                const title = [...descendants(list)].find(w => w instanceof Gtk.Label && w.label === word);
                const del = title && find(title.get_parent().get_parent(),
                    w => w instanceof Gtk.Button && w.tooltip_text === 'Forget');
                if (!del)
                    throw new Error('no such row');
                del.activate();
            },
            State() {
                return JSON.stringify({
                    title: window.title,
                    status: status.get_mapped() ? status.label : null,
                    button: button.get_mapped() ? button.label : null,
                    count: count.get_mapped() ? count.label : null,
                    rows: rows(),
                });
            },
            Close() {
                window.close();
            },
        });
        driver.export(Gio.DBus.session, PATH);
        window.connect('notify::title', () => print(`SMOKE-PREFS title ${window.title}`));
        print('SMOKE-PREFS ready');
    }
}
