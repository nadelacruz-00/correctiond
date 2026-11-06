#!/usr/bin/env python3
"""IBus client half of scripts/headless-smoke.sh.

Talks to the private ibus-daemon named by IBUS_ADDRESS: creates an input
context, selects correctiond:us, walks it through content types and sends
key events. Every ProcessKeyEvent must come back False (pass-through).
Prints one line per phase so the shell side can match them against
trace.log. Exits non-zero on any failure.
"""

import os
import sys
import time
import warnings

import gi

gi.require_version("IBus", "1.0")
from gi.repository import GLib, IBus  # noqa: E402

RELEASE = int(IBus.ModifierType.RELEASE_MASK)
SHIFT = int(IBus.ModifierType.SHIFT_MASK)

# (keyval, state): "Hi you" + Backspace + Return, with Shift and releases.
KEYS = [
    (IBus.KEY_Shift_L, 0),
    (ord("H"), SHIFT),
    (ord("H"), SHIFT | RELEASE),
    (IBus.KEY_Shift_L, SHIFT | RELEASE),
    (ord("i"), 0),
    (ord("i"), RELEASE),
    (ord(" "), 0),
    (ord("y"), 0),
    (ord("o"), 0),
    (ord("u"), 0),
    (IBus.KEY_BackSpace, 0),
    (IBus.KEY_Return, 0),
]


def fail(msg):
    print(f"FAIL: {msg}")
    sys.exit(1)


def pump(seconds):
    ctx = GLib.MainContext.default()
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        while ctx.pending():
            ctx.iteration(False)
        time.sleep(0.02)


def send_keys(ic, label):
    for keyval, state in KEYS:
        handled = ic.process_key_event(keyval, 0, state)
        if handled:
            fail(f"{label}: ProcessKeyEvent returned True for a key")
    print(f"{label}: {len(KEYS)} key events, all returned False")


# Every word the engine may write to trace.log. Anything else (a typed
# character, a word, surrounding text) fails the check.
TRACE_WORDS = {
    "start", "dictionary", "ready", "exit", "CreateEngine", "us", "colemak",
    "key", "Printable", "Boundary", "Backspace", "Invalidate",
    "FocusIn", "FocusOut", "Reset", "Enable", "Disable", "SetCapabilities",
    "ContentType", "SetSurroundingText", "SetCursorLocation", "invalidate",
    # Word-boundary outcome lines: fixed outcome and mode words only.
    "boundary", "empty", "off", "observe", "not-plain", "prose-gate",
    "no-candidate", "text-mismatch", "corrected", "corrected-unverified",
    "silent", "prose",
}
# FREE_FORM is the only purpose that resolves to an observing mode: TERMINAL
# without a wm_class and PASSWORD are both Mode::Off (including the RACE
# phase, whose keys follow Set(PASSWORD) with no pumping). Releases and the bare
# Shift are ignored, so 8 of the 12 events per phase reach the state machine.
EXPECTED_KEY_LINES = ["Printable"] * 2 + ["Boundary"] + ["Printable"] * 3 + [
    "Backspace",
    "Boundary",
    # The single key after returning to FREE_FORM.
    "Printable",
]


def check_trace(path):
    lines = open(path).read().splitlines()
    if not lines:
        fail("trace.log is empty")
    for line in lines:
        stamp, *rest = line.split(" ")
        float(stamp)
        for tok in rest:
            word, _, num = tok.partition("=")
            if num:
                float(num)
                if not word.replace("_", "").isalpha() or not word.islower():
                    fail(f"unexpected trace field: {line!r}")
            elif word not in TRACE_WORDS:
                fail(f"unexpected trace word {word!r} in {line!r}")
    keys = [ln.split(" ")[2] for ln in lines if ln.split(" ")[1] == "key"]
    if keys != EXPECTED_KEY_LINES:
        fail(f"key classes {keys} != {EXPECTED_KEY_LINES}")
    if any("purpose=8" in ln or "purpose=10" in ln for ln in lines):
        fail("a content type that resolves to Off was logged")
    # Between the last FREE_FORM key and the return to FREE_FORM lie the
    # RACE, TERMINAL and PASSWORD phases (all Off): nothing may be logged.
    # Boundary outcome lines follow their key line; leave them out here.
    body = [
        ln.split(" ", 1)[1] for ln in lines if ln.split(" ")[1] != "boundary"
    ]
    last_free = max(i for i, b in enumerate(body) if b == "key Boundary")
    if "ContentType purpose=0 hints=0" not in body:
        fail("the return to FREE_FORM never reached the engine")
    back = body.index("ContentType purpose=0 hints=0")
    if back != last_free + 1:
        fail(f"trace lines while Off: {body[last_free + 1:back]}")
    # After FocusOut the global-engine daemon moves the engine to its fake
    # input context (FocusIn, SetCapabilities): lifecycle lines only.
    if body[back + 1:back + 3] != ["key Printable", "FocusOut"] or body[-1] != "exit":
        fail(f"unexpected tail after returning to FREE_FORM: {body[back + 1:]}")
    for ln in lines:
        print(f"  trace| {ln}")
    print(f"trace OK: {len(lines)} lines, key classes only, silent while Off")


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--check-trace":
        check_trace(sys.argv[2])
        return
    if not os.environ.get("IBUS_ADDRESS"):
        fail("IBUS_ADDRESS is not set")
    bus = IBus.Bus()
    if not bus.is_connected():
        fail("cannot connect to ibus-daemon")

    names = []
    for _ in range(100):
        # Runtime-registered engines are only in ActiveEngines; ListEngines
        # and GetEnginesByNames read the on-disk component registry.
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", DeprecationWarning)
            names = [e.get_name() for e in bus.list_active_engines()]
        if "correctiond:us" in names:
            break
        time.sleep(0.05)
    for want in ("correctiond:us", "correctiond:colemak"):
        if want not in names:
            fail(f"{want} not registered")
    print("registered: correctiond:colemak correctiond:us")
    # What the daemon deserialized from our RegisterComponent. Checked at
    # the end so the key phases (and the trace check) still run.
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", DeprecationWarning)
        layouts = {
            e.get_name(): (e.get_layout(), e.get_layout_variant())
            for e in bus.list_active_engines()
        }
    want_layouts = {
        "correctiond:colemak": ("us+colemak", ""),
        "correctiond:us": ("us", ""),
    }

    ic = bus.create_input_context("headless-smoke")
    ic.set_capabilities(
        int(IBus.Capabilite.FOCUS) | int(IBus.Capabilite.SURROUNDING_TEXT)
    )
    ic.focus_in()
    # The daemon runs with use-global-engine (its default), which refuses
    # InputContext.SetEngine; this is what `ibus engine NAME` does. It only
    # affects this private daemon.
    if not bus.set_global_engine("correctiond:us"):
        fail("SetGlobalEngine failed")
    pump(0.5)
    engine = ic.get_engine()
    if engine is None or engine.get_name() != "correctiond:us":
        fail("engine was not selected")
    print("engine: correctiond:us")

    ic.set_surrounding_text(IBus.Text.new_from_string("Hello"), 5, 5)
    ic.set_content_type(int(IBus.InputPurpose.FREE_FORM), 0)
    pump(0.3)
    send_keys(ic, "FREE_FORM")
    pump(0.3)
    print("MARK end-free-form", flush=True)

    # Race: the field turns sensitive and keys follow immediately, with no
    # main-loop pumping in between. The engine must apply ContentType before
    # any of these keys: none of them may show up in trace.log.
    ic.set_content_type(int(IBus.InputPurpose.PASSWORD), 0)
    send_keys(ic, "RACE (PASSWORD, no pump)")
    pump(0.3)

    ic.set_content_type(int(IBus.InputPurpose.TERMINAL), 0)
    pump(0.3)
    send_keys(ic, "TERMINAL")
    pump(0.3)

    ic.set_content_type(int(IBus.InputPurpose.PASSWORD), 0)
    pump(0.3)
    ic.set_surrounding_text(IBus.Text.new_from_string("hunter2"), 7, 7)
    send_keys(ic, "PASSWORD")
    pump(0.3)

    # Back to a normal field: the ContentType change must reach the engine
    # (one trace line) and keys are observed again.
    ic.set_content_type(int(IBus.InputPurpose.FREE_FORM), 0)
    pump(0.3)
    if ic.process_key_event(ord("k"), 0, 0):
        fail("FREE_FORM again: ProcessKeyEvent returned True")
    print("FREE_FORM again: 1 key event, returned False")
    pump(0.3)

    ic.focus_out()
    pump(0.3)
    for name, want in want_layouts.items():
        if layouts.get(name) != want:
            fail(f"{name} layout/variant {layouts.get(name)} != {want}")
    print("layouts: colemak=us+colemak us=us")
    print("OK")


if __name__ == "__main__":
    main()
