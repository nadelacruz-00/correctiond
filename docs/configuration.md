# Configuration

correctiond has one settings file, `~/.config/correctiond/config.toml`. The
Settings window of the companion extension (Extensions, Correctiond Companion,
Settings) edits it for you; this page is the reference for editing it by hand.

## When settings apply

- The engine reads the file once, when it starts. After a change, restart it
  with Settings, General, Restart engine, or `ibus restart`.
- The extension watches the file, so `[modules] touch` applies live, a moment
  after the file is saved, with no restart. It also reads the file when it
  turns on. It reads `osk_source` (and the older `auto_switch_osk`) each time
  GJS OSK opens, so those also take effect without a restart.
- A missing file means defaults. An unreadable or invalid file also means
  defaults, and the engine prints one line to the journal
  (`correctiond: config.toml is invalid, using defaults`). Nothing in the file
  can stop the engine from starting.
- Keys you leave out keep their defaults.

The Settings window rewrites the whole file when you change a setting, so
comments you add by hand are not kept. It refuses to edit a file that has keys
or syntax it does not understand, and shows the line instead.

## Keys

| Key | Type | Default | Read by |
| --- | --- | --- | --- |
| `allowlist` | array of strings | `["firefox", "firefox_firefox", "firefox-esr", "org.mozilla.firefox", "org.gnome.TextEditor"]` | engine |
| `broken` | array of strings | `[]` | engine |
| `[modules]` `touch` | boolean | derived (below) | extension |
| `osk_source` | string | unset | extension |
| `auto_switch_osk` | boolean | `true` | extension |
| `allow_any_caller` | boolean | `false` | engine |
| `[[languages]]` | array of tables | English, plus Tagalog and Waray if their lexicon files exist (below) | engine |

### `allowlist`

Apps where typos are fixed, by window class (the Wayland app id). Matching
ignores case. Every app that is not listed is learn-only: the engine learns
words there but never changes text.

The default list has five entries: `firefox`, `firefox_firefox` (the Firefox
snap), `firefox-esr`, `org.mozilla.firefox` and `org.gnome.TextEditor` (GNOME
Text Editor, native and Flatpak). If you write an `allowlist`, your list
replaces the default list entirely. A config with an explicit `allowlist`
that was written before Text Editor joined the defaults does not gain it;
add `org.gnome.TextEditor` yourself.

The easiest way to add an app is the Settings window's Apps page: "Add an
installed app…" lists installed apps with the id the engine matches on (the
desktop file's `StartupWMClass`, or else its desktop id). Some examples:
`firefox`, `firefox_firefox` (the Firefox snap), `org.gnome.TextEditor`.

A terminal in the allowlist gets prose-only corrections: a word is fixed only
after two clean prose words in a row, only if it is at least 4 letters, and
nothing typed in a terminal is ever learned. A terminal that is not
allowlisted is fully off. The engine treats a field as a terminal when the app
reports the terminal input purpose, or when its class is a known terminal
(Ptyxis, GNOME Console, GNOME Terminal, Alacritty, kitty, foot, WezTerm, xterm,
Ghostty, Terminator, Tilix, Konsole).

### `broken`

Apps whose text editing breaks corrections (for example, the deleted word and
the fix arrive out of order). An app listed here only learns, even if it is
also in `allowlist`. A terminal listed here is off.

### `[modules]`

```toml
[modules]
touch = true
```

`touch` turns on-screen keyboard support on or off. One switch governs both
on-screen keyboard behaviors: the touch geometry the extension sends to the
engine, and the input-source switch while the keyboard is open. The Settings
window shows it as a switch on the General page.

- **Derivation.** If the file has no `touch` key, the extension derives the
  value each time: it is on when the GJS OSK extension is installed, and off
  when it is not. GJS OSK counts as installed when
  `gnome-shell/extensions/gjsosk@vishram1123.com` is a directory under
  `$XDG_DATA_HOME` (`~/.local/share` when unset) or under any entry of
  `$XDG_DATA_DIRS`. Installing GJS OSK later turns touch on by itself while
  the key is absent. A `touch` key in the file always wins.
- **Written only on a flip.** The Settings window writes `touch` only after
  you flip the switch. Saving any other setting leaves the key absent if it
  was absent, so it keeps deriving.
- **Dependent keys.** `osk_source` and `auto_switch_osk` apply only while touch
  is on.
- **Wrong type.** If `touch` is not `true` or `false`, the engine ignores it
  and prints one line to the journal (`correctiond: config.toml modules.touch
  must be true or false; ignored`). The Settings window then treats the file
  as one it does not understand and is read-only until you fix the line.
- **`IBUS_COMPONENT_PATH`.** TODO-VM: whether this variable replaces the IBus
  component directory or adds to it is not verified yet. It is filled in from
  the VM tests (plan Task 20). Until then, do not rely on either behavior.
- An older extension rejects the `[modules]` block and shows read-only
  Settings. See Troubleshooting in the [README](../README.md#downgraded-extension-shows-read-only-settings).

### `osk_source`

The IBus input source the extension switches to while GJS OSK is on screen,
so each on-screen key types the letter it shows. The previous source comes
back when the OSK closes, unless you picked another one by hand meanwhile.

- `"correctiond:us"`: QWERTY (what an unset value means).
- `"correctiond:colemak"`: Colemak.
- `""`: never switch.

The target must be one of your input sources, or nothing switches (the
journal says so once). With per-window input sources turned on
(`org.gnome.desktop.input-sources per-window`), the extension does not switch
either. In every case the engine still scores typos with on-screen keyboard
geometry while the OSK is open.

### `auto_switch_osk`

The older form of `osk_source`. `true` means `correctiond:us`, `false` means
no switch. It is used only when `osk_source` is absent. The Settings window
writes both keys so older versions of the extension keep working.

### `allow_any_caller`

Debugging only. When `true`, any client on your session bus can call the
engine's Shell-only control methods (for example with `busctl`), and the
engine prints `correctiond: WARNING: allow_any_caller is on; any session-bus
client can call the control interface` at startup. `ListWords` and
`ForgetWords` stay Shell-only even then. Leave it `false` in daily use. See
[SECURITY.md](../SECURITY.md).

### Languages

Each `[[languages]]` table has a `code` and a `lexicon`. If you write any
`[[languages]]` table, your list replaces the default list entirely. The
default is English only. Tagalog and Waray are added to the default list only
when their lexicon file already exists at the old default path
(`~/.local/share/correctiond/lexicons/tl.txt` and `war.txt`), so a fresh
install has English only and no startup warning, and an older install that
has those files keeps them. With both files present the default is:

```toml
[[languages]]
code = "en"
lexicon = "builtin:en"

[[languages]]
code = "tl"
lexicon = "~/.local/share/correctiond/lexicons/tl.txt"

[[languages]]
code = "war"
lexicon = "~/.local/share/correctiond/lexicons/war.txt"
```

- `lexicon` is `builtin:en` (the English list compiled into the engine) or a
  file path. A leading `~/` is expanded to your home directory.
- A lexicon file has one `word<TAB>count` per line. Lines starting with `#`
  are comments, malformed lines are skipped, words are lowercased, and words
  with non-ASCII characters are dropped.
- A missing file is skipped with a warning at startup
  (`correctiond: WARNING: no lexicon for language ...`). Words in that
  language may then be corrected into another language.
- At most 16 languages; the rest are ignored.
- A word in any loaded lexicon is never corrected. Corrections are limited
  to the language most of your last four known words were in; when no
  language leads, every language is allowed but the best candidate must win
  by a wider margin.

The Settings window's Languages page accepts codes of 2 to 8 lowercase
letters or `_`, such as `ceb`.

#### Building a lexicon

`scripts/build-lexicon.py` builds a lexicon from frequency lists (standard
library only). For Tagalog and Waray, download the Leipzig Corpora Collection
`*-words.txt` files (<https://wortschatz.uni-leipzig.de/en/download>), then
run from the repo root:

```sh
python3 scripts/build-lexicon.py --lang tl  --freq tgl_*-words.txt --freq-format leipzig --out ~/.local/share/correctiond/lexicons/tl.txt
python3 scripts/build-lexicon.py --lang war --freq war_*-words.txt --freq-format leipzig --out ~/.local/share/correctiond/lexicons/war.txt
```

Options (see `python3 scripts/build-lexicon.py --help`):

- `--freq FILE` or `--freq FORMAT:FILE`, repeatable (counts are summed).
  `FORMAT` is `leipzig`, `plain` or `lexicon`.
- `--freq-format leipzig|plain`: the format of a bare `--freq` path (default
  `leipzig`).
- `--words FILE`: a vetted word list; every listed word is kept.
- `--min-count N`: drop `--freq` entries seen fewer than N times (default 3).
- `--mode MODE`: octal permissions of the output (default `0644`).

Missing parent directories of `--out` are created with mode `0700`.

`scripts/clean-lexicon.py` removes English words, names and Latin taxonomy
from a Tagalog or Waray lexicon. It keeps a short list of core words that are
spelled like English ones. `--out` may equal `--in`:

```sh
python3 scripts/clean-lexicon.py --lang tl --english data/en_words.txt --in ~/.local/share/correctiond/lexicons/tl.txt --out ~/.local/share/correctiond/lexicons/tl.txt
```

`--ratio` (default 10) sets how much more frequent, per million words, a word
must be in the language than in English to stay.

## Full example

```toml
osk_source = "correctiond:us"
auto_switch_osk = true

allowlist = ["firefox", "firefox_firefox", "org.gnome.TextEditor"]
broken = []

[modules]
touch = true

[[languages]]
code = "en"
lexicon = "builtin:en"

[[languages]]
code = "tl"
lexicon = "~/.local/share/correctiond/lexicons/tl.txt"
```

## Environment variables

| Variable | Effect |
| --- | --- |
| `CORRECTIOND_TRACE=1` | Writes an event trace to `~/.local/state/correctiond/trace.log` (mode `0600`, truncated past 1 MiB). It records key classes and decisions, never characters or words. For debugging; unset it and delete the file afterwards. |
| `IBUS_ADDRESS` | The IBus daemon address. Normally unset: the engine finds the address under `~/.config/ibus/bus/`. |

The engine is started by the IBus daemon, so set variables for your session
(for example in `~/.config/environment.d/`) and log in again.

## Files

| Path | What |
| --- | --- |
| `~/.config/correctiond/config.toml` | settings |
| `~/.local/share/correctiond/learn.db` (plus `-wal`, `-shm`) | encrypted learning store |
| `~/.local/share/correctiond/learn.db.unreadable-*` | a store the key could not open, renamed aside; removed by "Forget all learned words" |
| `~/.local/share/correctiond/lexicons/` | locally built lexicons |
| `~/.local/share/correctiond/import-report.json` | optional report left by an importer ([import-report.md](import-report.md)) |
| `~/.local/state/correctiond/trace.log` | trace, only with `CORRECTIOND_TRACE=1` |
| `/usr/share/ibus/component/correctiond.xml` | IBus component, written by `scripts/install-dev.sh` |
