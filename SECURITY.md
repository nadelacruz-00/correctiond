# Security

correctiond sits in your input method, so it sees every key you type in a
session where it is the active input source. This page says what it keeps,
how it protects it, who may talk to it, and how to report a problem.

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's private
vulnerability reporting: open the repository's **Security** tab and choose
**Report a vulnerability**
(<https://github.com/nadelacruz-00/correctiond/security/advisories/new>).
Do not open a public issue for a security problem.

Include the version or commit, your GNOME Shell and IBus versions, and the
steps to reproduce. Do not include real typed text, learned words, keyring
contents or an unreviewed trace; synthetic examples are enough.

Fixes land on the `main` branch and in the next release.

## What is kept

The engine learns only from fields it is allowed to learn from:

- Never from password, PIN, email, URL, phone, digits or number fields, or
  from fields with the no-spellcheck or private hint.
- Never from terminals, whether the terminal reports the terminal input
  purpose or is recognized by its window class. A passphrase typed at a
  hidden `sudo` or `ssh` prompt looks like ordinary prose, so terminals are
  excluded entirely.
- Never while correction is off (the Quick Settings toggle) or learning is
  paused ("Learn new words").
- Only word-shaped tokens: 3 to 24 ASCII letters or apostrophes, starting with
  a letter, lowercase or Capitalized. Tokens with digits, symbols or mixed
  case are never learned.

What it keeps:

| Data | Saved to disk when |
| --- | --- |
| Words with a count and last-use time | seen at least twice |
| Adjacent word pairs with a count | seen at least twice |
| Letter slips per layout (you deleted one letter and typed another) | seen at least twice |
| Reverted corrections (typed word, rejected fix) | on the next flush |
| SHA-256 of each imported vocab file, with the import time | at import |

Anything seen once stays in memory only. When the store opens, words with a
count of 2 or less that were not used for 90 days are pruned, and the store is
capped at 200,000 words and 100,000 word pairs.
The five most recently learned words are kept in memory only, for the Quick
Settings menu.

Residual risk: a letters-only secret typed into an ordinary text field that
does not report a sensitive purpose can be learned. Encryption covers that
case, and you can forget any word.

## Where it is kept, and how

- `~/.local/share/correctiond/learn.db`, with its `-wal` and `-shm` files.
  The directory is mode `0700` and the files `0600`.
- The database is encrypted with SQLCipher (AES-256 pages, HMAC-SHA512 page
  authentication) using a random 256-bit raw key from the OS random number
  generator. There is no plaintext fallback.
- The key is stored only as a Secret Service item ("Correctiond learning store
  key") in your default (login) keyring collection, which gnome-keyring
  unlocks with your login password.
- The engine never asks to unlock the keyring and never shows a prompt. If the
  keyring is locked or not running, learning stays in memory and the store is
  attached later.
- Database writes are batched in the background every 30 seconds and on
  exit; the typing path only reads memory.
- A database the key cannot open is renamed to `learn.db.unreadable-*`, never
  silently deleted.

"Forget all learned words" (Settings, Learning) deletes the database, its
side files, any renamed copies, and the keyring item; the next start uses a
new key. If the keyring is locked at that moment, the files are deleted at
once and the keyring item when the keyring next unlocks.

### What this protects against

Backups of your home directory, a lost or stolen device without full-disk
encryption, and other local users. Each is protected only as strongly as your
login password: with an empty login keyring password, the key is effectively
stored in plain text.

### What it does not protect against

Malware already running as your user, which can ask the Secret Service for
the key or read keystrokes directly. Use full-disk encryption as the
operating-system complement.

## No side copies

- No network code. The engine's only connections are local Unix sockets to
  the IBus daemon and the session D-Bus.
- Logs, D-Bus errors and `engine import` output are fixed texts and counts.
  They never contain typed text, learned words, file contents, paths of
  imported files, or key material.
- Panics print only the source location, not the message.
- The process sets `PR_SET_DUMPABLE=0` (no ptrace by other processes of your
  user) and `RLIMIT_CORE=0` (no core files). The `.deb` package also installs
  an apport ignore entry so crash reports do not capture its memory.
- Tracing is opt-in (`CORRECTIOND_TRACE=1`), records key classes only, and
  writes to a `0600` file.

## Control D-Bus caller rules

The engine serves `org.nadelacruz.Correctiond1` at `/org/nadelacruz/Correctiond`
on the session bus. Only processes of your own user can reach the session
bus.

| Method | Who may call |
| --- | --- |
| `GetStats` | any caller; returns counts and booleans only |
| `ImportVocabulary` | any caller, so `engine import FILE` works from a terminal |
| `SetFocusedApp`, `SetOskVisible`, `SetEnabled`, `Purge`, `GetStatus`, `SetLearning`, `RecentWords`, `ForgetWord` | only the connection that owns `org.gnome.Shell` |
| `ListWords`, `ForgetWords` | only the connection that owns `org.gnome.Shell`, even with `allow_any_caller` |

The Shell owner is tracked through `NameOwnerChanged`. A refused call gets
`org.freedesktop.DBus.Error.AccessDenied`.

`allow_any_caller = true` in `config.toml` lifts the Shell-only rule for all
methods except `ListWords` and `ForgetWords`, for debugging with `busctl`. The
engine prints a warning at startup when it is on. With it on, any process of
your user can change the focused app, turn correction off, purge the store or
read recently learned words.

### Learned words in the Settings window

The Settings window runs in a separate process (GNOME's extension preferences
service), so it cannot call the Shell-only methods itself. It goes through the
extension's companion object, `org.nadelacruz.CorrectiondCompanion1` at
`/org/nadelacruz/CorrectiondCompanion`:

- `ConfirmForget` shows a Shell confirmation dialog. Only Forget in that
  dialog calls `Purge`; Cancel is the default button.
- `RequestWordAccess` returns false at once, without a prompt, unless the
  caller is the process that owns `org.gnome.Shell.Extensions` and has the
  focused window with that wm_class. Otherwise it shows "Show your learned
  words in Settings?" with Cancel as the default. Allow grants that one bus
  connection access until it closes, at most two connections at a time, and
  nothing is stored. Disabling the extension (the screen lock does this)
  revokes every grant.
- `ListWords` and `ForgetWords` on the companion are passed to the engine only
  for connections granted this way.
