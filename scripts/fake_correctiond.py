# Stand-in for the engine's control plane, for scripts/extension-smoke.sh:
# owns org.nadelacruz.Correctiond1 on the (private) session bus and appends
# each call to the log file given as argv[1]. No caller check. Like the
# engine, it remembers SetLearning in memory only: a restarted fake learns.
# Purge replies after 2.5 s, longer than the extension's 2 s call timeout.
#
# argv[2], the mode (default "normal"):
#   forget-fails  ForgetWord and ForgetWords fail as with a locked keyring
#   old           an engine from before GetStatus: no GetStatus, SetLearning,
#                 RecentWords, ForgetWord, ListWords or ForgetWords (callers
#                 get UnknownMethod)
#   v0.1.0        exactly the method set of the v0.1.0 tag (V0_1_0_XML, frozen:
#                 later additions to XML do not change it); an unknown mode
#                 exits with an error
#   readme        for scripts/readme-screenshots.sh: invented counts (128
#                 corrections, 9 undone, 2,431 words learned) and five
#                 invented recent words
# ListWords answers 3 fixed rows; ForgetWords counts the given words among
# them and forgets nothing.
import sys
import time
import gi
gi.require_version('Gio', '2.0')
from gi.repository import Gio, GLib

XML = '''<node><interface name="org.nadelacruz.Correctiond1">
<method name="SetFocusedApp"><arg type="s" direction="in"/></method>
<method name="SetOskVisible"><arg type="b" direction="in"/></method>
<method name="SetEnabled"><arg type="b" direction="in"/></method>
<method name="Purge"/>
<method name="GetStats"><arg type="s" direction="out"/></method>
NEW_METHODS
</interface></node>'''
NEW_METHODS = '''<method name="GetStatus"><arg type="s" direction="out"/></method>
<method name="SetLearning"><arg type="b" direction="in"/></method>
<method name="RecentWords"><arg type="as" direction="out"/></method>
<method name="ForgetWord"><arg type="s" direction="in"/><arg type="b" direction="out"/></method>
<method name="ListWords"><arg type="a(sutb)" direction="out"/></method>
<method name="ForgetWords"><arg type="as" direction="in"/><arg type="u" direction="out"/></method>'''
# The method set of the v0.1.0 tag, written out in full and never edited: it
# is what the extension must keep working against (docs/ARCHITECTURE.md).
V0_1_0_XML = '''<node><interface name="org.nadelacruz.Correctiond1">
<method name="SetFocusedApp"><arg type="s" direction="in"/></method>
<method name="SetOskVisible"><arg type="b" direction="in"/></method>
<method name="SetEnabled"><arg type="b" direction="in"/></method>
<method name="Purge"/>
<method name="GetStats"><arg type="s" direction="out"/></method>
<method name="GetStatus"><arg type="s" direction="out"/></method>
<method name="SetLearning"><arg type="b" direction="in"/></method>
<method name="RecentWords"><arg type="as" direction="out"/></method>
<method name="ForgetWord"><arg type="s" direction="in"/><arg type="b" direction="out"/></method>
<method name="ListWords"><arg type="a(sutb)" direction="out"/></method>
<method name="ForgetWords"><arg type="as" direction="in"/><arg type="u" direction="out"/></method>
</interface></node>'''
MODE = sys.argv[2] if len(sys.argv) > 2 else 'normal'
if MODE not in ('normal', 'forget-fails', 'old', 'readme', 'v0.1.0'):
    sys.exit(f'unknown mode {MODE!r}')
XML = V0_1_0_XML if MODE == 'v0.1.0' else XML.replace('NEW_METHODS', '' if MODE == 'old' else NEW_METHODS)
PURGE_DELAY_MS = 2500
state = {'learning': True}
README_STATS = '{"corrections":128,"reverts":9,"words_learned":2431,"persistent":true}'
README_RECENT = ['kadto', 'sige', 'ayaw', 'kuya', 'grabe']
LOCKED = ('org.nadelacruz.Correctiond1.Error.ForgetFailed', 'unlock your keyring first')

def words():
    now = int(time.time())
    return [('kadto', 41, now, True), ('sige', 12, now - 86400, True), ('the', 500, now, False)]

def status():
    learning = 'true' if state['learning'] else 'false'
    return f'{{"app_mode":"correct","app_reason":"allowlisted","enabled":true,"learning":{learning}}}'
log = open(sys.argv[1], 'a', buffering=1)

def call(conn, sender, path, iface, method, params, inv):
    log.write(f'{method} {params.unpack()!r} from {sender}\n')
    if method == 'GetStats':
        stats = README_STATS if MODE == 'readme' else '{}'
        inv.return_value(GLib.Variant('(s)', (stats,)))
    elif method == 'GetStatus':
        inv.return_value(GLib.Variant('(s)', (status(),)))
    elif method == 'SetLearning':
        state['learning'] = params.unpack()[0]
        inv.return_value(None)
    elif method == 'RecentWords':
        recent = README_RECENT if MODE == 'readme' else ['kadto', 'sige']
        inv.return_value(GLib.Variant('(as)', (recent,)))
    elif method in ('ForgetWord', 'ForgetWords') and MODE == 'forget-fails':
        inv.return_dbus_error(*LOCKED)
    elif method == 'ForgetWord':
        inv.return_value(GLib.Variant('(b)', (True,)))
    elif method == 'ListWords':
        inv.return_value(GLib.Variant('(a(sutb))', (words(),)))
    elif method == 'ForgetWords':
        known = {w for w, *_ in words()}
        inv.return_value(GLib.Variant('(u)', (len(set(params.unpack()[0]) & known),)))
    elif method == 'Purge':
        def reply():
            inv.return_value(None)
            log.write('PURGE-REPLIED\n')
            return GLib.SOURCE_REMOVE
        GLib.timeout_add(PURGE_DELAY_MS, reply)
    else:
        inv.return_value(None)

def acquired(conn, name):
    info = Gio.DBusNodeInfo.new_for_xml(XML).interfaces[0]
    conn.register_object_with_closures2('/org/nadelacruz/Correctiond', info, call, None, None)
    log.write(f'owned {MODE}\n')

Gio.bus_own_name(Gio.BusType.SESSION, 'org.nadelacruz.Correctiond1', 0, acquired, None, None)
GLib.MainLoop().run()
