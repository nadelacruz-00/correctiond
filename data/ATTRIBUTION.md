# Attribution

## Data

- **SCOWL / wamerican** (`/usr/share/dict/american-english`): the English word
  list used to vet spellings in `data/en_words.txt`. SCOWL is copyright Kevin
  Atkinson and is distributed under a permissive MIT-style notice; see
  http://wordlist.aspell.net/scowl-readme/ for the full notice.
- **hermitdave FrequencyWords, `content/2018/en/en_50k.txt`**
  (https://github.com/hermitdave/FrequencyWords): word frequencies in
  `data/en_words.txt`. The data is licensed CC-BY-SA-4.0
  (https://creativecommons.org/licenses/by-sa/4.0/); derived from OpenSubtitles.
  `data/en_words.txt` is a derived work and is shared under the same license.

## Software

- **SQLCipher**: BSD-style license (Zetetic LLC).
- **OpenSSL**: Apache-2.0.

## Building the Tagalog and Waray-Waray lexicons locally

The TL and WAR lexicons are never committed or shipped. Until they are built,
the engine prints `correctiond: WARNING: no lexicon for language ...` at
startup and may correct Tagalog and Waray words to English. Download the
Leipzig Corpora Collection `*-words.txt` files for each language
(https://wortschatz.uni-leipzig.de/en/download), then run, from the repo root:

    python3 scripts/build-lexicon.py --lang tl  --freq tgl_*-words.txt --freq-format leipzig --out ~/.local/share/correctiond/lexicons/tl.txt
    python3 scripts/build-lexicon.py --lang war --freq war_*-words.txt --freq-format leipzig --out ~/.local/share/correctiond/lexicons/war.txt

These are the paths the engine reads by default (`languages` in
`~/.config/correctiond/config.toml` overrides them). The script creates
missing directories with mode 0700. Lexicons load when the engine starts, so
log out and back in afterwards.

Entries with non-ASCII characters (for example words with n-tilde) are dropped.
