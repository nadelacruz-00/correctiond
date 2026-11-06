#!/usr/bin/env python3
"""Regenerate engine/tests/golden/ibustext_build.json from libibus itself.

Builds IBus.Text("build ") with python-gi, serializes it the way ibus-daemon
expects on the wire, and writes the GVariant as JSON: every struct becomes
{"signature": <type string>, "fields": [...]}, variants are unwrapped to
their contents, a{sv} becomes an object and arrays become lists.

Usage: python3 scripts/gen-ibus-golden.py
"""

import json
import pathlib

import gi

gi.require_version("IBus", "1.0")
from gi.repository import IBus  # noqa: E402

OUT = (
    pathlib.Path(__file__).resolve().parent.parent
    / "engine/tests/golden/ibustext_build.json"
)


def walk(v):
    t = v.get_type_string()
    if t == "v":
        return walk(v.get_variant())
    if t.startswith("("):
        return {
            "signature": t,
            "fields": [walk(v.get_child_value(i)) for i in range(v.n_children())],
        }
    if t.startswith("a{"):
        out = {}
        for i in range(v.n_children()):
            entry = v.get_child_value(i)
            out[entry.get_child_value(0).unpack()] = walk(entry.get_child_value(1))
        return out
    if t.startswith("a"):
        return [walk(v.get_child_value(i)) for i in range(v.n_children())]
    return v.unpack()


def main():
    text = IBus.Text.new_from_string("build ")
    golden = walk(text.serialize_object())
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(golden, indent=2) + "\n")
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
