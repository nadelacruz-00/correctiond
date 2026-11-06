#!/usr/bin/env python3
"""Build a lexicon file (`word<TAB>count` per line) from frequency data.

Standard library only. Lexicons hold lowercase ASCII letters plus apostrophe;
entries with any other character (including non-ASCII) are dropped. Missing
parent directories of --out are created with mode 0700.

The engine reads ~/.local/share/correctiond/lexicons/<code>.txt by default
(for example tl.txt and war.txt); see data/ATTRIBUTION.md.
"""
import argparse
import os
import re
import sys

WORD_RE = re.compile(r"^[a-z']+$")
# ASCII digits only: str.isdigit() also accepts e.g. '\u00b2', which int() rejects.
COUNT_RE = re.compile(r"^[0-9]+$")


FREQ_FORMATS = ("leipzig", "plain", "lexicon")
MODE_RE = re.compile(r"^[0-7]{3,4}$")


def parse_mode(text):
    """argparse type: an octal permission string such as 0600."""
    if not MODE_RE.match(text):
        raise argparse.ArgumentTypeError(f"invalid mode {text!r}; use octal such as 0644")
    return int(text, 8)


def split_freq(spec, default_fmt):
    """`FORMAT:PATH` -> (format, path). Only a known format name counts as a
    prefix, so a path that itself contains a colon is left alone."""
    fmt, sep, rest = spec.partition(":")
    if sep and fmt in FREQ_FORMATS:
        return fmt, rest
    return default_fmt, spec


def norm(word):
    word = word.strip().lower()
    return word if WORD_RE.match(word) else None


def read_freq(path, fmt):
    counts = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip("\r\n")
            if not line or line.startswith("#"):
                continue
            if fmt == "leipzig":
                # `<id>\t<word>\t<count>`, or the 2017 community downloads'
                # `<id>\t<word>\t<word>\t<count>`: the count is always last.
                parts = line.split("\t")
                if len(parts) not in (3, 4):
                    continue
                word, count = parts[1], parts[-1]
            else:
                # `plain` and `lexicon`: whitespace-separated `word count`.
                parts = line.split()
                if len(parts) != 2:
                    continue
                word, count = parts
            w = norm(word)
            if w is None or not COUNT_RE.match(count):
                continue
            counts[w] = counts.get(w, 0) + int(count)
    return counts


def read_words(path):
    words = set()
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            w = norm(line)
            if w:
                words.add(w)
    return words


def make_parents(path):
    """Creates the missing parent directories of `path`, each mode 0700."""
    parent = os.path.dirname(os.path.abspath(path))
    missing = []
    while not os.path.isdir(parent):
        missing.append(parent)
        parent = os.path.dirname(parent)
    for d in reversed(missing):
        os.mkdir(d, 0o700)
        os.chmod(d, 0o700)  # independent of the umask


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--lang", required=True)
    ap.add_argument(
        "--freq", action="append",
        help="frequency file, or FORMAT:PATH with FORMAT leipzig|plain|lexicon "
        "(lexicon is word<TAB>count with # comments); repeat to merge several "
        "corpora (counts are summed). A bare PATH uses --freq-format",
    )
    ap.add_argument("--freq-format", choices=["leipzig", "plain"], default="leipzig")
    ap.add_argument("--words")
    ap.add_argument(
        "--min-count", type=int, default=3,
        help="drop --freq entries seen fewer times (default 3); ignored with --words, "
        "which keeps every listed word",
    )
    ap.add_argument("--out", required=True)
    ap.add_argument("--mode", type=parse_mode, default=0o644,
                    help="octal permission bits of --out (default 0644)")
    a = ap.parse_args()

    if not a.freq and not a.words:
        ap.error("need --freq and/or --words")
    freq = {}
    for spec in a.freq or []:
        fmt, path = split_freq(spec, a.freq_format)
        for w, c in read_freq(path, fmt).items():
            freq[w] = freq.get(w, 0) + c
    if a.words:
        # Vetted word set: keep every member; unseen words get count 1.
        entries = {w: freq.get(w, 1) for w in read_words(a.words)}
    else:
        entries = {w: c for w, c in freq.items() if c >= a.min_count}

    rows = sorted(entries.items(), key=lambda kv: (-kv[1], kv[0]))
    make_parents(a.out)
    with open(a.out, "w", encoding="utf-8") as f:
        f.write(f"# lexicon lang={a.lang} entries={len(rows)}\n")
        for w, c in rows:
            f.write(f"{w}\t{c}\n")
    os.chmod(a.out, a.mode)


if __name__ == "__main__":
    sys.exit(main())
