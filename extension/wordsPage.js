// The Settings window's Words page: lists the learned words after the user
// allows it in a Shell prompt, and forgets them with a 5 s Undo. Every call
// goes through the companion on one private bus connection, since the
// companion allows the connection (its unique name) that asked. The page
// never logs a word: its log lines are fixed texts.

import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk';
import Pango from 'gi://Pango';

import {
    PendingForget,
    SORTS,
    SORT_LABELS,
    compareWords,
    forgetFailureText,
    lastUsedText,
    matchesSearch,
} from './lib.js';

const SHELL = 'org.gnome.Shell';
const COMPANION_PATH = '/org/nadelacruz/CorrectiondCompanion';
const COMPANION_IFACE = 'org.nadelacruz.CorrectiondCompanion1';
const REQUEST_TIMEOUT_MS = 120000;
const CALL_TIMEOUT_MS = 2000;
const BATCH_MS = 5000;
// On close the connection waits this long at most for its flush.
const FLUSH_WAIT_MS = 1000;
const DEFAULT_SORT = 'newest';

const HIDDEN_TEXT = 'Your learned words are hidden until you allow it.';
const SHOW_LABEL = 'Show learned words…';
const NOT_ALLOWED = 'Not allowed';
const UNAVAILABLE = 'Turn on the Autocorrect extension to use this';
const LOAD_FAILED = 'Couldn\'t load your learned words';
const EMPTY_TEXT = 'Nothing learned yet. correctiond learns as you type and fix words. ' +
    'To start with your own vocabulary, see correctiond-import-cookbook.';
const UPDATE_ENGINE = 'Update correctiond to use this';

// The page's connection is not (or no longer) allowed: the screen lock
// disables the extension, which empties its allowed set.
const ACCESS_LOST = new Set([
    'org.freedesktop.DBus.Error.AccessDenied',
    'org.freedesktop.DBus.Error.UnknownObject',
]);
// From an engine without the method, passed through by the companion; but
// GDBus also says so for an object path with nothing exported (the
// extension turned off), so companionPresent tells the two apart.
const UNKNOWN_METHOD = 'org.freedesktop.DBus.Error.UnknownMethod';

const LOG = 'correctiond-companion prefs:';

/** "used C times · last used X" for one row. */
export function rowSubtitle(row, nowMs) {
    return `used ${row.count.toLocaleString()} times · last used ${lastUsedText(row.lastSeen, nowMs)}`;
}

const wordsText = n => (n === 1 ? '1 word' : `${n.toLocaleString()} words`);

/** The remote D-Bus error name (null for a local failure) and its message. */
function errorInfo(e) {
    if (!(e instanceof GLib.Error))
        return {name: null, message: null};
    const name = Gio.DBusError.get_remote_error(e);
    if (name)
        Gio.DBusError.strip_remote_error(e);
    return {name, message: e.message};
}

function companionCall(conn, method, params, replyType, timeout) {
    return new Promise((resolve, reject) => {
        conn.call(SHELL, COMPANION_PATH, COMPANION_IFACE, method, params,
            new GLib.VariantType(replyType), Gio.DBusCallFlags.NONE, timeout, null, (c, res) => {
                try {
                    resolve(c.call_finish(res).deepUnpack());
                } catch (e) {
                    reject(e);
                }
            });
    });
}

/** The real calls: one private session bus connection, closed with the page. */
export const defaultWordsCalls = {
    open() {
        const address = Gio.dbus_address_get_for_bus_sync(Gio.BusType.SESSION, null);
        return new Promise((resolve, reject) => {
            Gio.DBusConnection.new_for_address(address,
                Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT |
                    Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
                null, null, (_o, res) => {
                    try {
                        resolve(Gio.DBusConnection.new_for_address_finish(res));
                    } catch (e) {
                        reject(e);
                    }
                });
        });
    },
    requestAccess: conn => companionCall(conn, 'RequestWordAccess', null, '(b)', REQUEST_TIMEOUT_MS)
        .then(([allowed]) => allowed),
    listWords: conn => companionCall(conn, 'ListWords', null, '(a(sutb))', CALL_TIMEOUT_MS)
        .then(([rows]) => rows),
    forgetWords: (conn, words) => companionCall(conn, 'ForgetWords',
        new GLib.Variant('(as)', [words]), '(u)', CALL_TIMEOUT_MS).then(([n]) => n),
    /** Whether the Shell exports the companion object (its introspection
     * lists the interface); false on any failure. */
    companionPresent: conn => new Promise(resolve => {
        conn.call(SHELL, COMPANION_PATH, 'org.freedesktop.DBus.Introspectable', 'Introspect', null,
            new GLib.VariantType('(s)'), Gio.DBusCallFlags.NONE, CALL_TIMEOUT_MS, null, (c, res) => {
                try {
                    const [xml] = c.call_finish(res).deepUnpack();
                    resolve(xml.includes(`<interface name="${COMPANION_IFACE}">`));
                } catch (e) {
                    resolve(false);
                }
            });
    }),
    /** Writes out every message sent on `conn` so far. */
    flush: conn => new Promise((resolve, reject) => {
        conn.flush(null, (c, res) => {
            try {
                c.flush_finish(res);
                resolve();
            } catch (e) {
                reject(e);
            }
        });
    }),
    /** Closes `conn`; the promise settles (never rejects) once it is closed. */
    close(conn) {
        return new Promise(resolve => {
            conn.close(null, (c, res) => {
                try {
                    c.close_finish(res);
                } catch (e) {
                    log(`${LOG} words connection close failed`);
                }
                resolve();
            });
        });
    },
};

/** A ListWords row, as a (word, count, last_seen, personal) tuple or an object. */
const toRow = r => (Array.isArray(r)
    ? {word: r[0], count: Number(r[1]), lastSeen: Number(r[2]), personal: Boolean(r[3])}
    : {word: r.word, count: Number(r.count), lastSeen: Number(r.lastSeen), personal: Boolean(r.personal)});

/**
 * Adds the Words page to `window` (an Adw.PreferencesWindow) and returns its
 * controller; the smoke test drives it directly.
 *
 * @param {Adw.PreferencesWindow} window
 * @param {{wordsCalls?: {open: () => object|Promise<object>,
 *   requestAccess: (conn: object) => Promise<boolean>,
 *   listWords: (conn: object) => Promise<object[]>,
 *   forgetWords: (conn: object, words: string[]) => Promise<number>,
 *   companionPresent: (conn: object) => Promise<boolean>,
 *   flush: (conn: object) => Promise<void>,
 *   close: (conn: object) => void}}} opts
 */
export function addWordsPage(window, opts = {}) {
    const calls = opts.wordsCalls ?? defaultWordsCalls;

    // --- state ---

    const rows = new Map(); // word -> {word, count, lastSeen, personal, item}
    const leaving = new Set(); // out of the list: pending or being forgotten
    const selected = new Set(); // by word, so it survives search and sort
    const pending = new PendingForget();
    let conn = null;
    let busy = false;
    let torn = false;
    let batchTimer = 0;
    let query = '';

    // --- widgets ---

    const page = new Adw.PreferencesPage({title: 'Words', icon_name: 'accessories-dictionary-symbolic'});
    window.add(page);

    const hiddenGroup = new Adw.PreferencesGroup({title: 'Learned words'});
    page.add(hiddenGroup);
    const hiddenBox = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 12});
    const statusLabel = new Gtk.Label({
        label: HIDDEN_TEXT, wrap: true, xalign: 0, use_markup: false, css_classes: ['dim-label'],
    });
    const showButton = new Gtk.Button({
        label: SHOW_LABEL, halign: Gtk.Align.CENTER, css_classes: ['pill', 'suggested-action'],
    });
    hiddenBox.append(statusLabel);
    hiddenBox.append(showButton);
    hiddenGroup.add(hiddenBox);

    const listGroup = new Adw.PreferencesGroup({visible: false});
    page.add(listGroup);
    const listBox = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 12, vexpand: true});
    listGroup.add(listBox);

    const toolbar = new Gtk.Box({spacing: 6});
    const searchEntry = new Gtk.SearchEntry({placeholder_text: 'Search words…', hexpand: true});
    const sortRow = Gtk.DropDown.new_from_strings(SORTS.map(s => SORT_LABELS[s]));
    sortRow.selected = SORTS.indexOf(DEFAULT_SORT);
    sortRow.valign = Gtk.Align.CENTER;
    sortRow.tooltip_text = 'Sort';
    const refreshButton = new Gtk.Button({
        icon_name: 'view-refresh-symbolic', tooltip_text: 'Refresh', valign: Gtk.Align.CENTER,
        css_classes: ['flat'],
    });
    toolbar.append(searchEntry);
    toolbar.append(new Gtk.Label({label: 'Sort:', css_classes: ['dim-label']}));
    toolbar.append(sortRow);
    toolbar.append(refreshButton);
    listBox.append(toolbar);
    const emptyLabel = new Gtk.Label({
        label: EMPTY_TEXT, wrap: true, xalign: 0, use_markup: false, visible: false, css_classes: ['dim-label'],
    });
    listBox.append(emptyLabel);

    const switchList = new Gtk.ListBox({selection_mode: Gtk.SelectionMode.NONE, css_classes: ['boxed-list']});
    const allSwitch = new Adw.SwitchRow({title: 'Show all stored words', use_markup: false});
    switchList.append(allSwitch);
    listBox.append(switchList);

    const countLabel = new Gtk.Label({label: wordsText(0), xalign: 0, css_classes: ['dim-label']});
    listBox.append(countLabel);

    // Gio.ListStore -> FilterListModel (switch and search) -> SortListModel.
    const store = new Gio.ListStore({item_type: Gtk.StringObject});
    const filter = Gtk.CustomFilter.new(item => {
        const row = rows.get(item.string);
        return row !== undefined && (allSwitch.active || row.personal) && matchesSearch(row.word, query);
    });
    const sortKey = () => SORTS[sortRow.selected] ?? DEFAULT_SORT;
    const sorter = Gtk.CustomSorter.new((a, b) => {
        const ra = rows.get(a.string);
        const rb = rows.get(b.string);
        return ra && rb ? compareWords(ra, rb, sortKey()) : 0;
    });
    const listModel = new Gtk.SortListModel({
        model: new Gtk.FilterListModel({model: store, filter}),
        sorter,
    });

    const actionLabel = new Gtk.Label({label: '', xalign: 0});
    // Every row widget the factory made (a small recycled pool). There is no
    // unbind handler: GTK emits unbind while GJS finalizes the list, when JS
    // callbacks are blocked.
    const bound = new Set();
    const syncSelection = () => {
        actionBar.revealed = selected.size > 0;
        actionLabel.label = `${selected.size.toLocaleString()} selected`;
        for (const w of bound) {
            w._syncing = true;
            w._check.active = selected.has(w._word);
            w._syncing = false;
        }
    };

    const factory = new Gtk.SignalListItemFactory();
    factory.connect('setup', (_f, listItem) => {
        const box = new Gtk.Box({spacing: 12, margin_top: 6, margin_bottom: 6, margin_start: 12, margin_end: 6});
        const check = new Gtk.CheckButton({valign: Gtk.Align.CENTER, tooltip_text: 'Select'});
        const text = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, hexpand: true, valign: Gtk.Align.CENTER});
        const title = new Gtk.Label({xalign: 0, ellipsize: Pango.EllipsizeMode.END, use_markup: false});
        const subtitle = new Gtk.Label({xalign: 0, use_markup: false, css_classes: ['dim-label', 'caption']});
        text.append(title);
        text.append(subtitle);
        const del = new Gtk.Button({
            icon_name: 'user-trash-symbolic', tooltip_text: 'Forget', valign: Gtk.Align.CENTER,
            css_classes: ['flat'],
        });
        box.append(check);
        box.append(text);
        box.append(del);
        box._check = check;
        box._title = title;
        box._subtitle = subtitle;
        box._word = null;
        bound.add(box);
        check.connect('toggled', () => {
            if (!box._syncing && box._word !== null)
                controller.select(box._word, check.active);
        });
        del.connect('clicked', () => {
            if (box._word !== null)
                controller.forgetWord(box._word);
        });
        listItem.child = box;
    });
    factory.connect('bind', (_f, listItem) => {
        const box = listItem.child;
        const row = rows.get(listItem.item.string);
        box._word = listItem.item.string;
        box._title.label = box._word;
        box._subtitle.label = row ? rowSubtitle(row, Date.now()) : '';
        box._syncing = true;
        box._check.active = selected.has(box._word);
        box._syncing = false;
    });
    const listView = new Gtk.ListView({
        model: new Gtk.NoSelection({model: listModel}), factory, css_classes: ['rich-list'],
    });
    const scroller = new Gtk.ScrolledWindow({
        child: listView, vexpand: true, min_content_height: 360,
        hscrollbar_policy: Gtk.PolicyType.NEVER, css_classes: ['card'],
    });
    listBox.append(scroller);

    const actionBar = new Gtk.ActionBar({revealed: false});
    const clearButton = new Gtk.Button({label: 'Clear'});
    const forgetSelectedButton = new Gtk.Button({label: 'Forget selected', css_classes: ['destructive-action']});
    actionBar.pack_start(actionLabel);
    actionBar.pack_end(forgetSelectedButton);
    actionBar.pack_end(clearButton);
    listBox.append(actionBar);

    // --- list helpers ---

    listModel.connect('items-changed', () => {
        countLabel.label = wordsText(listModel.get_n_items());
    });
    // The empty text shows while no row is left in the list.
    const syncEmpty = () => {
        emptyLabel.visible = [...rows.keys()].every(w => leaving.has(w));
    };
    const showList = on => {
        hiddenGroup.visible = !on;
        listGroup.visible = on;
    };
    // The hidden state with TEXT; a null BUTTONLABEL hides the button.
    const showStatus = (text, buttonLabel = SHOW_LABEL) => {
        statusLabel.label = text;
        emptyLabel.visible = false;
        showButton.visible = buttonLabel !== null;
        if (buttonLabel !== null)
            showButton.label = buttonLabel;
        showList(false);
    };
    // Every row not leaving goes back in the store (one splice).
    const fillStore = () => {
        const items = [];
        for (const r of rows.values()) {
            if (!leaving.has(r.word))
                items.push(r.item);
        }
        store.splice(0, store.get_n_items(), items);
    };
    const removeFromStore = words => {
        if (words.length === 1) {
            const [found, pos] = store.find(rows.get(words[0]).item);
            if (found)
                store.remove(pos);
        } else {
            fillStore();
        }
    };
    const restore = words => {
        const items = [];
        for (const w of words) {
            const r = rows.get(w);
            if (leaving.delete(w) && r)
                items.push(r.item);
        }
        store.splice(store.get_n_items(), 0, items);
        syncEmpty();
    };
    const addToast = title => {
        const t = new Adw.Toast({title, timeout: 3, use_markup: false});
        try {
            window.add_toast(t);
        } catch (e) {
            log(`${LOG} words toast failed`);
        }
        return t;
    };

    // --- forgetting ---

    const stopTimer = () => {
        if (batchTimer) {
            GLib.source_remove(batchTimer);
            batchTimer = 0;
        }
    };
    // The batch toast goes away in code: its batch was sent or undone.
    const dismissToast = () => {
        const t = controller.toast;
        controller.toast = null;
        if (t) {
            t._done = true;
            t.dismiss();
        }
    };
    // One ForgetWords for `words`; on failure the rows come back with a toast.
    const send = words => {
        if (words.length === 0 || conn === null)
            return;
        let call;
        try {
            call = Promise.resolve(calls.forgetWords(conn, words)); // sent now, not in a later job
        } catch (e) {
            call = Promise.reject(e);
        }
        call.then(() => {
            for (const w of words) {
                leaving.delete(w);
                rows.delete(w);
            }
            syncEmpty();
        }, e => {
            if (torn)
                return; // closed: the connection went before the reply
            const info = errorInfo(e);
            log(`${LOG} forgetting words failed: ${info.name ?? 'local'}`);
            restore(words);
            controller.lastToast = addToast(forgetFailureText(info));
            if (ACCESS_LOST.has(info.name))
                showStatus(NOT_ALLOWED);
        });
    };
    const fireBatch = () => {
        batchTimer = 0;
        dismissToast();
        send(pending.take());
        return GLib.SOURCE_REMOVE;
    };
    const undo = () => {
        stopTimer();
        dismissToast();
        restore(pending.undo());
    };
    const queueForget = list => {
        const words = list.filter(w => rows.has(w) && !leaving.has(w));
        if (words.length === 0 || torn)
            return;
        for (const w of words) {
            leaving.add(w);
            selected.delete(w);
        }
        removeFromStore(words);
        syncSelection();
        syncEmpty();
        const size = pending.add(words);
        const title = size === 1 ? `Forgot ${words[0]}` : `Forgot ${size.toLocaleString()} words`;
        if (controller.toast) {
            controller.toast.title = title;
        } else {
            const t = new Adw.Toast({
                title, timeout: 0, button_label: 'Undo', use_markup: false,
                priority: Adw.ToastPriority.HIGH, // not queued behind a failure toast
            });
            t._done = false;
            t.connect('button-clicked', () => {
                if (!t._done)
                    undo();
            });
            // Closed by the user without Undo: the batch still goes.
            t.connect('dismissed', () => {
                if (controller.toast === t)
                    controller.toast = null;
            });
            controller.toast = t;
            try {
                window.add_toast(t);
            } catch (e) {
                log(`${LOG} words toast failed`);
            }
        }
        stopTimer();
        batchTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, BATCH_MS, fireBatch);
    };

    // --- access and loading ---

    const ensureConn = async () => {
        if (conn !== null)
            return conn;
        const c = await calls.open();
        if (torn) {
            calls.close(c);
            return null;
        }
        conn = c;
        return conn;
    };

    const reload = async () => {
        if (conn === null || torn)
            return;
        let list;
        try {
            list = await calls.listWords(conn);
        } catch (e) {
            const {name} = errorInfo(e);
            log(`${LOG} listing words failed: ${name ?? 'local'}`);
            if (torn)
                return;
            if (ACCESS_LOST.has(name)) {
                showStatus(NOT_ALLOWED);
            } else if (name === UNKNOWN_METHOD) {
                // An older engine: asking again would only prompt again.
                let present = false;
                try {
                    present = await calls.companionPresent(conn);
                } catch (e2) {
                    present = false;
                }
                if (!torn)
                    showStatus(present ? UPDATE_ENGINE : UNAVAILABLE, present ? null : SHOW_LABEL);
            } else {
                showStatus(LOAD_FAILED, 'Retry');
            }
            return;
        }
        if (torn)
            return;
        rows.clear();
        for (const raw of list) {
            const r = toRow(raw);
            if (typeof r.word === 'string' && r.word !== '' && !rows.has(r.word))
                rows.set(r.word, {...r, item: Gtk.StringObject.new(r.word)});
        }
        for (const w of [...selected]) {
            if (!rows.has(w))
                selected.delete(w);
        }
        fillStore();
        syncSelection();
        syncEmpty();
        showList(true);
    };

    const requestAccess = async () => {
        if (busy || torn)
            return;
        busy = true;
        showButton.sensitive = false;
        try {
            let allowed;
            try {
                const c = await ensureConn();
                if (c === null)
                    return;
                allowed = await calls.requestAccess(c);
            } catch (e) {
                log(`${LOG} word access request failed: ${errorInfo(e).name ?? 'local'}`);
                if (!torn)
                    showStatus(UNAVAILABLE);
                return;
            }
            if (torn)
                return;
            if (!allowed) {
                showStatus(NOT_ALLOWED);
                return;
            }
            await reload();
        } finally {
            busy = false;
            showButton.sensitive = true;
        }
    };

    // --- teardown and close ---

    // Once: sends the pending words in one ForgetWords, then flushes the
    // connection (so that call is on the bus) and closes it, after the flush
    // or FLUSH_WAIT_MS. Nothing waits for the reply: the close goes ahead.
    const teardown = () => {
        if (torn)
            return;
        stopTimer();
        dismissToast();
        send(pending.take());
        torn = true;
        const c = conn;
        conn = null;
        if (c === null)
            return;
        let timer = 0;
        let closed = false;
        const closeConn = () => {
            if (closed)
                return;
            closed = true;
            if (timer)
                GLib.source_remove(timer);
            timer = 0;
            try {
                calls.close(c);
            } catch (e) {
                log(`${LOG} words connection close failed`);
            }
        };
        timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, FLUSH_WAIT_MS, () => {
            timer = 0;
            log(`${LOG} closing the words connection without its flush`);
            closeConn();
            return GLib.SOURCE_REMOVE;
        });
        let flushed;
        try {
            flushed = Promise.resolve(calls.flush(c));
        } catch (e) {
            flushed = Promise.reject(e);
        }
        flushed.catch(() => log(`${LOG} words connection flush failed`)).finally(closeConn);
    };

    // Never held: GNOME's preferences service releases its hold on its own
    // close-request, and its window turns destroy() into an error page.
    window.connect('close-request', () => {
        teardown();
        return false;
    });
    // Destroyed without a close-request (a realized window unrealizes in
    // gtk_window_destroy). No ::destroy handlers: GJS runs those during GC,
    // where they are blocked.
    window.connect('unrealize', teardown);

    // --- wiring ---

    showButton.connect('clicked', () => requestAccess());
    refreshButton.connect('clicked', () => reload());
    searchEntry.connect('search-changed', () => {
        query = searchEntry.text.trim();
        filter.changed(Gtk.FilterChange.DIFFERENT);
    });
    allSwitch.connect('notify::active', () =>
        filter.changed(allSwitch.active ? Gtk.FilterChange.LESS_STRICT : Gtk.FilterChange.MORE_STRICT));
    sortRow.connect('notify::selected', () => sorter.changed(Gtk.SorterChange.DIFFERENT));
    clearButton.connect('clicked', () => controller.clearSelection());
    forgetSelectedButton.connect('clicked', () => controller.forgetSelected());

    const controller = {
        page,
        showButton,
        statusLabel,
        emptyLabel,
        searchEntry,
        sortRow,
        allSwitch,
        countLabel,
        listModel,
        selected,
        toast: null, // the batch toast while it is up
        lastToast: null, // the last failure toast
        requestAccess,
        reload,
        undo,
        /** What a row's delete button does. */
        forgetWord(word) {
            queueForget([word]);
        },
        /** What a row's check button does. */
        select(word, on) {
            if (on && rows.has(word) && !leaving.has(word))
                selected.add(word);
            else if (!on)
                selected.delete(word);
            syncSelection();
        },
        clearSelection() {
            selected.clear();
            syncSelection();
        },
        /**
         * Forgets the selected words, hidden ones too. Several ask first: the
         * confirm dialog is returned (null when nothing needed asking).
         */
        forgetSelected() {
            const words = [...selected];
            if (words.length === 0)
                return null;
            if (words.length === 1) {
                queueForget(words);
                return null;
            }
            const dialog = new Adw.AlertDialog({heading: `Forget ${words.length.toLocaleString()} words?`});
            dialog.add_response('cancel', 'Cancel');
            dialog.add_response('forget', 'Forget');
            dialog.set_response_appearance('forget', Adw.ResponseAppearance.DESTRUCTIVE);
            dialog.default_response = 'cancel';
            dialog.close_response = 'cancel';
            dialog.connect('response', (_d, response) => {
                if (response === 'forget')
                    queueForget(words);
            });
            dialog.present(window);
            return dialog;
        },
    };
    return controller;
}
