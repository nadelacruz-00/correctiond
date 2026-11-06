# Troubleshooting

Most problems show up in one of three places: the Quick Settings menu's
status line, the Settings window's General page, and the journal. The
engine's own lines all start with `correctiond:`, and the extension's with
`correctiond-companion:`:

```sh
journalctl --user -b | grep correctiond
```

Neither ever logs typed words.

## What the Quick Settings status line means

| Status line | Meaning | What to do |
| --- | --- | --- |
| Correcting in NAME | The app is allowlisted and the field can be corrected. | |
| Not correcting in NAME (not in your app list) | Learn-only app. | Add it in Settings, Apps, "Fix typos in these apps". |
| Not correcting in NAME (marked broken) | The app is in "Learn only, never fix". | Remove it there if corrections work in it. |
| Not correcting here (app not identified) | The engine does not know which app has focus. | Check that the extension is enabled. |
| Not correcting in NAME (this text field doesn't support it) | The field cannot report or delete surrounding text, so a fix would duplicate the word. | Nothing; the engine only learns here. |
| Off in this field (password or private) | Password, PIN, email, URL, number or private field. | Nothing; this is by design. |
| Off in this terminal (not in your app list, or marked broken) | Terminals are off unless allowlisted. | Allowlist it for prose-only fixes. |
| No text field yet | No field has had focus since the engine started. | Click into a text field. |
| Engine not running | The engine is not reachable on the session bus. | See below. |
| Learning paused | "Learn new words" is off. | Turn it back on; a restart also turns it on. |

## Nothing is corrected anywhere

1. Check the active input source: `ibus engine` should print
   `correctiond:colemak` or `correctiond:us`. If not, select an "autocorrect"
   source with Super+Space, or run `ibus engine correctiond:colemak`.
2. If the source is missing from GNOME Settings, Keyboard, the IBus component
   is not installed: run `scripts/install-dev.sh` (after
   `cargo build --release`).
3. If the source is there but the Settings window says "Not running", look for
   startup errors in the journal. The component's `<exec>` points at
   `target/release/engine` in the checkout where you ran the script; if you
   moved or deleted that checkout, run the script again.
4. Check that the Autocorrect toggle in Quick Settings is on.

## A word I expected to be fixed was left alone

The engine fixes a word only when one candidate clearly beats the others.
Common reasons it does nothing:

- **First word of a field.** Until it has seen a few words, the engine does
  not know which language you are writing in, so it demands a wider margin.
  The same typo is often fixed once it follows a known word.
- **The word is known.** It is in a loaded lexicon, or you have typed it at
  least twice.
- **You reverted that fix before.** A reverted correction is never made again.
  Forgetting either word of the pair (✕ in Quick Settings, or Forget on the
  Words page) clears it, as does "Forget all learned words".
- **Its shape.** ALL-CAPS, mixed case, digits, symbols and words under 3
  letters are never corrected. In an allowlisted terminal, a fix also needs
  two clean prose words first and a word of at least 4 letters.
- **The app's text did not match.** If the text before the cursor is not what
  the engine saw you type (after a paste or a click, for example), it does
  nothing.

## Tagalog or Waray words are changed into English

The Tagalog and Waray lexicons are not shipped. The journal shows
`correctiond: WARNING: no lexicon for language 'tl' at ...` at startup.
Build them as described in
[configuration.md](configuration.md#building-a-lexicon), then restart the
engine.

## Colemak types as QWERTY

GNOME Shell 50 ignores an IBus engine's `layout_variant`, so a component that
declares `<layout>us</layout>` plus `<layout_variant>colemak</layout_variant>`
types QWERTY (and Caps Lock is not Backspace). The shipped
`data/correctiond.xml` declares `<layout>us+colemak</layout>` instead. If you
installed from an older checkout, update it and run `scripts/install-dev.sh`
again.

## Learned words are gone after a restart

The store is in memory only while the keyring is locked or missing. The
journal shows `correctiond: keyring locked; learning stays in memory` or
`correctiond: keyring unavailable (...)`. The engine never shows an unlock
prompt. Unlock the login keyring (for example in Seahorse) and switch windows:
the store attaches within seconds and the journal shows
`correctiond: encrypted learning store attached`.

While the store is not attached:

- `engine import` fails with `learning store not available; unlock your
  keyring and retry`.
- Forgetting a word fails with "Unlock your keyring first".
- "Forget all learned words" deletes the data files, and the notification
  says "Learned words deleted; the key will be removed when your keyring
  unlocks". The keyring item is deleted once it unlocks.

If the key in the keyring cannot open `learn.db`, the old file is renamed to
`learn.db.unreadable-*` and a new store is created. The renamed file is never
deleted automatically; "Forget all learned words" removes it.

## The on-screen keyboard types the wrong letters

GJS OSK sends keys through the active layout. If your on-screen keyboard is
drawn as QWERTY while Colemak is active, each key types the Colemak letter.

- In Settings, General, set "Keyboard layout while GJS OSK is open" to the
  layout your OSK is drawn for. The default is QWERTY (`correctiond:us`).
- That input source must also be in GNOME Settings, Keyboard; otherwise the
  journal says `correctiond:us is not an input source; not switching`.
- With per-window input sources on, the extension does not switch, to avoid
  leaving a window on the OSK's layout. The journal says so once.
- If GJS OSK is not installed, or its keyboard cannot be found, nothing
  changes.

## Changes in Settings do not apply

App and language changes need an engine restart: Settings, General, Restart
engine (it runs `ibus restart`). Changes to the extension itself (after
`scripts/install-extension.sh`) need a log out and back in on Wayland.

If the Settings window says "Settings file not editable here", the file has
a key or syntax the window does not understand. Fix it by hand
([configuration.md](configuration.md)) or remove it to start from defaults.

## The Words page shows nothing

| Text | Meaning |
| --- | --- |
| Your learned words are hidden until you allow it. | Click "Show learned words…" and then Allow in the Shell prompt. |
| Not allowed | The prompt was cancelled, timed out, or the window was not focused when it asked. |
| Turn on the Autocorrect extension to use this | The extension is disabled (the screen lock also disables it for a moment). |
| Update correctiond to use this | The engine is older than the extension. Rebuild and restart it. |

By default the page lists only personal words, the ones in no lexicon. Turn
on "Show all stored words" to see every word the store has saved.

## `engine import` errors

| Message | Meaning |
| --- | --- |
| `usage: engine import FILE` | Give exactly one file. |
| `cannot open the file` | The path does not exist or is not readable. |
| `correctiond is not running` | No engine on the session bus. |
| `not a correctiond vocab v1 file` | The first non-empty line is not `#correctiond-vocab v1`. |
| `file too large (over 16 MiB or 200,000 records)` | Split the file. |
| `this file was already imported` | The same bytes were imported before. |
| `learning store not available; unlock your keyring and retry` | See the keyring section above. |
| `cannot reach the session bus` | Run it from inside your desktop session. |
| `correctiond did not answer` | The engine did not reply within 120 seconds. |
| `learning store write failed` or `import failed` | Check the journal; nothing from the file was saved. |

The format is in [vocab-format.md](vocab-format.md).

## Collecting a trace

Set `CORRECTIOND_TRACE=1` for your session (for example in
`~/.config/environment.d/`) and log in again. The engine then writes key
classes and decisions, never characters, to
`~/.local/state/correctiond/trace.log`. Unset the variable and delete the file
when you are done. Do not attach traces or journal output to public issues
without reading them first.
