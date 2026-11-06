// Test-only client for scripts/extension-smoke.sh: stands in for the
// Settings window's Words page. Never packed or installed.
//
// Like GNOME's real preferences service (dbusService.js), it is no
// Gtk.Application: it owns org.gnome.Shell.Extensions with Gio.DBus.own_name
// and shows a plain Adw.Window, so the window has no GTK app id and its
// wm_class (Wayland app id) is the program name, which is argv[0] here: the
// smoke passes the real service's value, or another one for the wm_class check.
// Its calls are the Words page's own (defaultWordsCalls in wordsPage.js).
//
// It reads commands from stdin, one per line, and runs each at once (a
// request can still be waiting for the prompt when the next one runs):
//   open C            open private connection C, as the Words page does
//   request C [TAG]   RequestWordAccess on C (120 000 ms timeout)
//   list C            ListWords on C (2000 ms)
//   forget C WORD...  ForgetWords on C (2000 ms)
//   probe C           whether the Shell exports the companion object
//   close C           close C
// Each prints one line, `CLIENT <the command> => <result>`, where the result
// is `true`/`false`, the rows as word:count:seen:personal, the count, `ok`,
// or `error <D-Bus error name>: <message>`. It prints `CLIENT ready <pid>`
// once its window is presented, and stays alive until it is killed.

import Adw from 'gi://Adw?version=1';
import Gio from 'gi://Gio';
import GioUnix from 'gi://GioUnix';
import GLib from 'gi://GLib';
import 'gi://Gtk?version=4.0';
import System from 'system';

import {defaultWordsCalls as calls} from '../extension/wordsPage.js';

const BUS_NAME = 'org.gnome.Shell.Extensions';

const prgname = System.programArgs[0];
const conns = new Map();

function say(line) {
    print(`CLIENT ${line}`);
}

function errorText(e) {
    if (!(e instanceof GLib.Error))
        return `error local: ${e}`;
    const name = Gio.DBusError.get_remote_error(e);
    if (name)
        Gio.DBusError.strip_remote_error(e);
    return `error ${name ?? 'local'}: ${e.message}`;
}

async function step(cmd, name, args) {
    if (cmd === 'open') {
        conns.set(name, await calls.open());
        return 'ok';
    }
    const conn = conns.get(name);
    if (!conn)
        throw new Error(`no connection ${name}`);
    switch (cmd) {
    case 'request':
        return String(await calls.requestAccess(conn));
    case 'list': {
        const rows = await calls.listWords(conn);
        return rows.map(([w, c, seen, p]) => `${w}:${c}:${seen > 0 ? 'seen' : 'never'}:${p}`).join(' ');
    }
    case 'forget':
        return String(await calls.forgetWords(conn, args));
    case 'probe':
        return String(await calls.companionPresent(conn));
    case 'close':
        await calls.close(conn);
        return 'ok';
    default:
        throw new Error(`unknown command ${cmd}`);
    }
}

function run(line) {
    const [cmd, name, ...args] = line.trim().split(/\s+/);
    if (!cmd)
        return;
    // A request's TAG only tells its reply apart.
    step(cmd, name, cmd === 'request' ? [] : args).then(
        result => say(`${line.trim()} => ${result}`),
        e => say(`${line.trim()} => ${errorText(e)}`));
}

function readCommands() {
    const stdin = new Gio.DataInputStream({
        base_stream: new GioUnix.InputStream({fd: 0, close_fd: false}),
    });
    const next = () => stdin.read_line_async(GLib.PRIORITY_DEFAULT, null, (s, res) => {
        let line;
        try {
            [line] = s.read_line_finish_utf8(res);
        } catch (e) {
            say(`stdin => ${errorText(e)}`);
            return;
        }
        if (line === null)
            return; // EOF: stay alive until killed
        run(line);
        next();
    });
    next();
}

if (!prgname) {
    printerr('usage: gjs -m smoke_words_client.js PRGNAME');
    System.exit(2);
}
GLib.set_prgname(prgname);
Adw.init();
const loop = new GLib.MainLoop(null, false);
Gio.DBus.own_name(Gio.BusType.SESSION, BUS_NAME, Gio.BusNameOwnerFlags.NONE, null, () => {
    const win = new Adw.Window({title: 'Smoke words client', default_width: 400, default_height: 300});
    win.present();
    say(`ready ${new Gio.Credentials().get_unix_pid()}`);
    readCommands();
}, () => {
    say(`lost ${BUS_NAME}`);
    loop.quit();
});
loop.run();
