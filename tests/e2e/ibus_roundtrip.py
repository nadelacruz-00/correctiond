#!/usr/bin/env python3
"""IBus client half of scripts/e2e.sh.

Plays a text field: keeps a client-side model of the text (cursor always at
the end), applies the engine's commit-text and delete-surrounding-text
signals to it, inserts the keys the engine leaves unhandled, and sends the
model back with set_surrounding_text after every key, as GTK does.

Assertions (one PASS/FAIL line each):
  (1) "bulid " in an allowlisted app's FREE_FORM field becomes "build ";
  (2) the same keys in a PASSWORD field stay "bulid ";
  (3) one BackSpace right after (1) restores "bulid ".
(2) runs first: the revert in (3) blacklists bulid->build, which would
otherwise make (2) pass for the wrong reason. Exits non-zero on any failure.

With an argument it runs one Taglish case instead (scripts/e2e.sh starts
the engine with the shipped English list and a Tagalog lexicon path):
  tl-absent:  "the naman " IS rewritten (no TL lexicon: the failure C1
              measured, naman -> haman), which proves tl-present can bite;
  tl-present: "the naman " stays as typed (TL fixture lexicon has naman).
"""

import json
import os
import subprocess
import sys
import time

import gi

gi.require_version("IBus", "1.0")
from gi.repository import GLib, IBus  # noqa: E402

DEST = "org.nadelacruz.Correctiond1"
OBJ = "/org/nadelacruz/Correctiond"
APP = "e2e"

failures = []


def fail(msg):
    print(f"FAIL: {msg}")
    sys.exit(1)


def check(ok, label, detail):
    print(f"{'PASS' if ok else 'FAIL'} {label}: {detail}")
    if not ok:
        failures.append(label)


def pump(seconds):
    ctx = GLib.MainContext.default()
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        while ctx.pending():
            ctx.iteration(False)
        time.sleep(0.01)


def control(method, *args):
    """Calls a control-plane method with gdbus; returns (ok, output)."""
    cmd = ["gdbus", "call", "--session", "--dest", DEST, "--object-path", OBJ,
           "--method", f"{DEST}.{method}", *args]
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
    return r.returncode == 0, (r.stdout or r.stderr).strip()


def stats():
    ok, out = control("GetStats")
    if not ok:
        return None
    # gdbus prints ('{"corrections":...}',)
    return json.loads(out[out.index("'") + 1:out.rindex("'")])


class Field:
    """The app's text before the cursor."""

    def __init__(self, ic):
        self.ic = ic
        self.text = ""
        self.sync()

    def commit(self, text):
        self.text += text

    def delete(self, offset, nchars):
        start = len(self.text) + offset
        if start < 0 or nchars > len(self.text) - start:
            fail(f"delete-surrounding-text({offset}, {nchars}) outside the text")
        self.text = self.text[:start] + self.text[start + nchars:]

    def sync(self):
        n = len(self.text)
        self.ic.set_surrounding_text(IBus.Text.new_from_string(self.text), n, n)

    def key(self, keyval):
        """One key press: the engine first, then the app if unhandled."""
        handled = self.ic.process_key_event(keyval, 0, 0)
        self.ic.process_key_event(keyval, 0, int(IBus.ModifierType.RELEASE_MASK))
        pump(0.05)  # the engine's signals arrived before the reply
        if not handled:
            if keyval == IBus.KEY_BackSpace:
                self.text = self.text[:-1]
            elif keyval == IBus.KEY_Return:
                self.text += "\n"
            else:
                self.text += chr(keyval)
        self.sync()
        pump(0.05)
        return handled

    def type(self, s):
        return [self.key(ord(c)) for c in s]


def connect():
    """The engine selected on an input context with surrounding text, the
    app focused and allowlisted. Returns (ic, holder); holder["field"] is
    the Field the engine's signals apply to."""
    if not os.environ.get("IBUS_ADDRESS"):
        fail("IBUS_ADDRESS is not set")
    bus = IBus.Bus()
    if not bus.is_connected():
        fail("cannot connect to ibus-daemon")

    ic = bus.create_input_context("e2e")
    ic.set_capabilities(
        int(IBus.Capabilite.FOCUS) | int(IBus.Capabilite.SURROUNDING_TEXT)
    )
    ic.focus_in()
    # The daemon runs with use-global-engine (its default), which refuses
    # InputContext.SetEngine; this is what `ibus engine NAME` does, on this
    # private daemon only.
    if not bus.set_global_engine("correctiond:us"):
        fail("SetGlobalEngine failed")
    pump(0.5)
    engine = ic.get_engine()
    if engine is None or engine.get_name() != "correctiond:us":
        fail("engine correctiond:us was not selected")
    print("engine: correctiond:us")

    holder = {"field": None}
    # python-gi prints "g_object_is_floating: assertion ... failed" once per
    # commit-text it marshals (the IBusText argument); the text itself
    # arrives intact. Client-library noise, not an engine failure.
    ic.connect("commit-text", lambda _ic, t: holder["field"].commit(t.get_text()))
    ic.connect("delete-surrounding-text",
               lambda _ic, off, n: holder["field"].delete(off, n))

    # The control plane comes up next to the IBus side; give it a moment.
    ok, out = False, ""
    for _ in range(50):
        ok, out = control("SetFocusedApp", APP)
        if ok:
            break
        time.sleep(0.1)
    check(ok, "SetFocusedApp", repr(APP) if ok else out.splitlines()[0])
    return ic, holder


def new_field(ic, holder, purpose):
    ic.set_content_type(int(purpose), 0)
    ic.reset()
    pump(0.2)
    holder["field"] = Field(ic)
    pump(0.1)
    return holder["field"]


def finish(ic):
    ic.focus_out()
    pump(0.2)
    if failures:
        print(f"failed: {', '.join(failures)}")
        sys.exit(1)
    print("OK")


def roundtrip():
    ic, holder = connect()

    # (2) PASSWORD: hard-off, so the typo stays.
    field = new_field(ic, holder, IBus.InputPurpose.PASSWORD)
    handled = field.type("bulid ")
    check(field.text == "bulid " and not any(handled), "(2) PASSWORD field",
          f"model {field.text!r}, handled {handled}")

    # (1) FREE_FORM in the allowlisted app: corrected at the space.
    field = new_field(ic, holder, IBus.InputPurpose.FREE_FORM)
    handled = field.type("bulid ")
    check(field.text == "build " and handled == [False] * 5 + [True],
          "(1) FREE_FORM in allowlisted app", f"model {field.text!r}, handled {handled}")

    # (3) BackSpace right after the fix restores what was typed.
    handled = field.key(IBus.KEY_BackSpace)
    check(field.text == "bulid " and handled, "(3) BackSpace reverts",
          f"model {field.text!r}, handled {handled}")

    s = stats()
    print(f"GetStats: {s}")
    if s is None or (s["corrections"], s["reverts"]) != (1, 1):
        check(False, "GetStats", "expected corrections=1 reverts=1")
    elif any(w in json.dumps(s) for w in ("bulid", "build")):
        check(False, "GetStats", "contains a typed word")
    finish(ic)


def taglish(case):
    """A Tagalog word after an English one, in an allowlisted FREE_FORM field."""
    ic, holder = connect()
    typed = "the naman "
    field = new_field(ic, holder, IBus.InputPurpose.FREE_FORM)
    field.type(typed)
    s = stats() or {}
    if case == "tl-absent":
        check(field.text != typed and s.get("corrections") == 1,
              "(4a) no TL lexicon: naman IS rewritten",
              f"model {field.text!r}, corrections {s.get('corrections')}")
    else:
        check(field.text == typed and s.get("corrections") == 0,
              "(4b) TL lexicon: naman is left alone",
              f"model {field.text!r}, corrections {s.get('corrections')}")
    finish(ic)


def main():
    case = sys.argv[1] if len(sys.argv) > 1 else None
    if case is None:
        roundtrip()
    elif case in ("tl-absent", "tl-present"):
        taglish(case)
    else:
        fail(f"unknown case {case!r}")


if __name__ == "__main__":
    main()
