#!/usr/bin/env python3
"""Remove English words, names and Latin taxonomy from a Tagalog or Waray lexicon.

Standard library only. Reads a lexicon (`word<TAB>count`, `#` comments) and
writes the cleaned lexicon in the same format, sorted by count.

A word is dropped when either:
- it is also in the English lexicon and is not at least --ratio times more
  frequent (per million words) in this language than in English, unless it
  is on the language's keep list (core words spelled like English ones,
  e.g. Tagalog "at", "may", Waray "ha", "in"); or
- it is not in the English lexicon and ends like Latin taxonomy
  (-idae, -inae, -aceae, -oidea, -optera, -ii, ...).

Dropping a word that is also English never makes the engine correct it: it
stays in the English lexicon. It only stops counting as a vote for this
language when the engine guesses which language you are typing.
"""
import argparse
import os
import re
import sys
import tempfile

KEEP = {
    "tl": {"at", "may", "man", "ring", "eh", "bang", "ate", "ha", "lola", "nag"},
    "war": {"ha", "in", "may", "man", "kay", "ira", "la", "pa", "na", "san",
            "ito", "gin", "nag", "ma"},
}
TAXON_RE = re.compile(
    r"^.{5,}(idae|inae|aceae|oidea|iformes|ineae|optera|opoda|ophyta|mycota|opsida|ozoa|ii)$"
)


def parse_mode(text):
    """argparse type: an octal permission string such as 0600."""
    if not re.match(r"^[0-7]{3,4}$", text):
        raise argparse.ArgumentTypeError(f"invalid mode {text!r}; use octal such as 0644")
    return int(text, 8)


def load(path):
    counts = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip("\r\n")
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) == 2 and parts[1].isascii() and parts[1].isdigit():
                counts[parts[0]] = counts.get(parts[0], 0) + int(parts[1])
    return counts


def per_million(counts):
    total = sum(counts.values()) or 1
    return {w: c / total * 1e6 for w, c in counts.items()}


def clean(lang_counts, en_counts, keep, ratio):
    """Returns (kept counts, number dropped as English, number dropped as taxa)."""
    lp, ep = per_million(lang_counts), per_million(en_counts)
    kept, english, taxa = {}, 0, 0
    for w, c in lang_counts.items():
        if w in ep:
            if w not in keep and lp[w] < ratio * ep[w]:
                english += 1
                continue
        elif TAXON_RE.search(w):
            taxa += 1
            continue
        kept[w] = c
    return kept, english, taxa


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--lang", required=True, choices=sorted(KEEP))
    ap.add_argument("--english", required=True, help="English lexicon, e.g. data/en_words.txt")
    ap.add_argument("--in", dest="inp", required=True)
    ap.add_argument("--out", required=True, help="may equal --in (replaced atomically)")
    ap.add_argument("--ratio", type=float, default=10.0)
    ap.add_argument("--mode", type=parse_mode, default=0o644,
                    help="octal permission bits of --out (default 0644)")
    a = ap.parse_args()

    kept, english, taxa = clean(load(a.inp), load(a.english), KEEP[a.lang], a.ratio)
    rows = sorted(kept.items(), key=lambda kv: (-kv[1], kv[0]))
    d = os.path.dirname(os.path.abspath(a.out))
    fd, tmp = tempfile.mkstemp(dir=d, prefix=".clean-")
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        f.write(f"# lexicon lang={a.lang} entries={len(rows)}\n")
        for w, c in rows:
            f.write(f"{w}\t{c}\n")
    os.chmod(tmp, a.mode)
    os.replace(tmp, a.out)
    print(f"{a.lang}: kept {len(rows)}, dropped {english} English/names, {taxa} taxonomy")


if __name__ == "__main__":
    sys.exit(main())
