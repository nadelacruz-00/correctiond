# Implementation notes

Running notes from implementation tasks: measurements, protocol findings
and checks still owed on the live desktop.

## IBus protocol findings (Task 8, IBus 1.5.34-rc2)

Checked against libibus's own introspection XML (strings in
`libibus-1.0.so.5`) and the 1.5.34 sources (`src/ibusengine.c`,
`bus/engineproxy.c`, `bus/ibusimpl.c`):

- `ContentType` is a write-only `(uu)` property set via
  `org.freedesktop.DBus.Properties.Set`, not a `SetContentType` method. The
  daemon only forwards it when the context's purpose/hints change.
- `FocusId` and `ActiveSurroundingText` are advertised as `(b)`, but libibus
  returns a plain `b` and the daemon reads it with `g_variant_get_boolean`.
  Ours are plain `bool`: `FocusId = false` (we get `FocusIn`/`FocusOut`),
  `ActiveSurroundingText = true` (the daemon requests surrounding text on
  focus-in and enable; we also emit `RequireSurroundingText` on `Enable`).
- `RequireSurroundingText ()` and `DeleteSurroundingText (iu)` are signals
  that the daemon handles although neither is in the XML.
- Engines registered at runtime (`RegisterComponent`, our `--register`)
  appear only in `ActiveEngines`; `ListEngines`/`GetEnginesByNames` read the
  on-disk registry only. `InputContext.SetEngine` is refused while
  use-global-engine is on (the default): `SetGlobalEngine`, which is what
  `ibus engine NAME` calls, works.
- zbus spawns a task per method call by default, which can reorder
  keystrokes. The Engine interface uses `spawn = false`. zbus's built-in
  `fdo::Properties` is always spawned, so `Properties.Set(ContentType)`
  could lose the race against an inline `ProcessKeyEvent`. Engine paths
  therefore replace it with our own `org.freedesktop.DBus.Properties`
  (`EngineProperties`, `spawn = false`). zbus's object-server loop dispatches
  one message at a time, so ContentType changes and keys take effect in
  strict message order.
- After `FocusOut` with use-global-engine, the daemon moves the engine to
  its fake input context (`FocusIn`, `SetCapabilities caps=41`).

## Dictionary build (Task 8)

Release build, en (builtin) only, because the tl/war lexicons are absent in
the headless run: 509–987 ms across runs (higher while cargo was compiling in
parallel), with an index of 24.9 MiB. This is under the 2 s threshold, so no
follow-up is needed yet. Re-measure once the tl and war lexicons are
installed.

## To verify on the desktop

These need the live GNOME 50 / Wayland session, which the headless smoke
(`scripts/headless-smoke.sh`) cannot exercise. Run them after
`cargo build --release && scripts/install-dev.sh` with `CORRECTIOND_TRACE=1`
set for the dev session (for example in `~/.config/environment.d/`, then log
in again). Afterwards, unset `CORRECTIOND_TRACE` and delete
`~/.local/state/correctiond/trace.log`.

1. **Build the Tagalog and Waray lexicons first.** They are never shipped
   (D4), and without them the engine corrects Filipino words to English
   (`naman` → `haman`). Download the Leipzig `*-words.txt` files
   (data/ATTRIBUTION.md), then, from the repo root:

       python3 scripts/build-lexicon.py --lang tl  --freq tgl_*-words.txt --freq-format leipzig --out ~/.local/share/correctiond/lexicons/tl.txt
       python3 scripts/build-lexicon.py --lang war --freq war_*-words.txt --freq-format leipzig --out ~/.local/share/correctiond/lexicons/war.txt

   The script creates missing directories with mode 0700. Check: the journal
   has no `correctiond: WARNING: no lexicon for language` line after the
   engine next starts.
2. **Open Question 2: does Ptyxis report purpose TERMINAL (10)?** Type in
   Ptyxis, then look for `ContentType purpose=10` in `trace.log`. Without a
   wm_class (Task 11) TERMINAL resolves to `Mode::Off`, so the trace goes
   silent in Ptyxis, and that silence is itself the evidence. If Ptyxis does
   NOT report TERMINAL, it resolves to Observe and learns word-shaped tokens
   until Task 11 supplies wm_class. In that case, avoid typing passphrases
   in Ptyxis on dev builds between Tasks 9 and 11.
3. **Zero behavior change:** `ibus engine correctiond:colemak`, then type in
   gnome-text-editor and Ptyxis. It must behave exactly like plain Colemak,
   which also confirms that `PR_SET_DUMPABLE=0` breaks nothing. `ibus engine`
   must print `correctiond:colemak`.
4. **Colemak keymap is applied.** Found on the desktop (2026-11-05): with
   `<layout>us</layout>` plus `<layout_variant>colemak</layout_variant>`,
   `correctiond:colemak` typed QWERTY (and Caps Lock was not Backspace, which
   the xkb Colemak variant provides). GNOME Shell 50's `_getXkbId()`
   (`ui/status/keyboard.js:71`) reads `engineDesc.variant`, which
   IBusEngineDesc does not have, so the variant is dropped and the bare
   layout string is applied as a GnomeXkbInfo id. The engine therefore
   declares `<layout>us+colemak</layout>` with no `<layout_variant>`;
   `GnomeXkbInfo.get_layout_info("us+colemak")` resolves to us/colemak.

   To check: after `scripts/install-dev.sh`, select `correctiond:colemak`
   and type; the keymap is Colemak and Caps Lock deletes.
5. **Variant switch:** Super+Space to `correctiond:us` types QWERTY, and
   `trace.log` shows a second `CreateEngine us` (or reuse) without a restart.
6. **Trace hygiene:** `trace.log` lists key classes (`Printable`,
   `Boundary`, `Backspace`, `Invalidate`) with no characters. It shows
   nothing while a password field is focused.
7. **Journal:** `journalctl --user -b | grep -c correctiond` shows only
   startup warnings (missing lexicons), never per-key lines.
8. **Dumpable:** `stat -c %U /proc/$(pgrep -f 'correctiond|target/release/engine')/mem`
   prints `root` when the daemon has exec'd the engine. The headless smoke
   already confirms this for a `--register` start.
9. **Cursor-move heuristic:** while typing a word continuously in
   gnome-text-editor and Firefox, `trace.log` must not show
   `SetCursorLocation invalidate` between the keys of a word, only after
   mouse clicks or arrow keys. Each passed-through Printable, Boundary or
   Backspace counts as one expected cursor move, capped at 2, and an
   `Invalidate` key (an arrow, for example) resets the count to 0. Clients
   that send several cursor updates per key, or that lag more than two keys
   behind, would break this.
10. **Surrounding-text warm-up:** a `SetSurroundingText` line appears soon
    after `Enable`/`FocusIn` in gnome-text-editor and Firefox (this feeds
    Open Question 1).
11. **Task 9: store key in the login keyring, never a prompt.** Not run by
    the agent (controller ruling: never touch the real keyring or the live
    desktop); the fake `SecretBackend` tests cover the logic. `secret-tool`
    needs `sudo apt install libsecret-tools`.
    - Live keyring round trip (creates and deletes a `purpose=test-key`
      item): `cargo test -p engine live_keyring -- --ignored`.
    - `ibus restart`, type a few words twice in gnome-text-editor (still
      pass-through until Task 10), then switch focus to flush. Expected:
      `secret-tool search purpose learn-db-key` shows exactly one item
      (label "Correctiond learning store key");
      `ls -ld ~/.local/share/correctiond ~/.local/share/correctiond/*`
      shows the dir 0700 and `learn.db` (plus `-wal`/`-shm`) 0600;
      `head -c 16 ~/.local/share/correctiond/learn.db | xxd` is NOT
      `SQLite format 3`. The journal shows
      `encrypted learning store attached` once.
    - Lock the login keyring in Seahorse, `ibus restart`, and type: typing
      works and NO prompt of any kind appears; the journal shows
      `keyring locked; learning stays in memory`. Unlock it and change
      focus: the store attaches within seconds (journal line as above).
12. **Task 10: live corrections and the control plane** (the brief's Step 5,
    not run by the agent: controller ruling, never the live desktop, the
    real session bus services or the real keyring; `scripts/e2e.sh` covers
    the keystroke-to-commit path headless). Set `allow_any_caller = true` in
    `~/.config/correctiond/config.toml` for this step only, rebuild,
    reinstall, `ibus restart`. Record results per app below (the spec's
    Assignment matrix, now with the real engine).
    - Without the extension, type `bulid ` in Firefox: NO correction (no
      wm_class ⇒ Observe).
    - `busctl --user call org.nadelacruz.Correctiond1 /org/nadelacruz/Correctiond org.nadelacruz.Correctiond1 SetFocusedApp s firefox`,
      then type `bulid ` in Firefox: it becomes `build `. An immediate
      Backspace restores `bulid `, and a second `bulid ` is NOT corrected
      (blacklisted). **Expect a first-word miss:** with the shipped English
      list, `bulid` as the first word of a field scores build 0.34 against
      bulls 0.68, a gap of 0.335, short of the 0.6 open margin (no
      dominant language yet). After one known English word (`the bulid `)
      the 0.3 margin applies and it is fixed
      (`builtin_english_fixes_bulid_after_an_english_word` pins both).
      If the first-word miss is not acceptable, the margins or the layout
      costs need retuning (separate decision).
    - In Ptyxis type `rm -rf ./bulid`: untouched. In a Firefox password
      field: untouched.
    - Type `hunter2 ` twice in a Firefox text field, and `export KEY=abc123`
      plus `some plain words here` twice in Ptyxis, then wait 30 s for a
      flush: `busctl --user call org.nadelacruz.Correctiond1 /org/nadelacruz/Correctiond org.nadelacruz.Correctiond1 GetStats`
      shows that `words_learned` did not grow for any of them.
    - Note the secret from `secret-tool search --all purpose learn-db-key`,
      then call `Purge` with busctl: `words_learned` drops to 0, any
      `learn.db.unreadable-*` files are gone, `learn.db` is recreated
      empty, and `secret-tool` shows exactly one item with a DIFFERENT
      secret.
    - Lock the keyring and call `Purge` again: it returns
      `org.nadelacruz.Correctiond1.Error.PurgePartial`, `learn.db` is gone
      and `key_delete_pending` is true. Unlock it and change focus: the old
      item is deleted before a new one is created.
    - Set `allow_any_caller = false` again and `ibus restart` (config is
      read only at startup). busctl `SetFocusedApp` must now fail with
      `org.freedesktop.DBus.Error.AccessDenied`, while `GetStats` still
      answers.
    - `SetEnabled false` (from the extension, Task 11) means `Mode::Off`:
      no correction and no learning until `SetEnabled true`.
13. **Task 11: companion Shell extension** (the brief's Steps 2 and 3, not
    run by the agent: controller ruling, never install, enable or reload
    anything in the live shell). `scripts/extension-smoke.sh` covers load,
    enable, disable, the gjs-osk open/close detection and the state push
    in a headless shell on private buses; everything below needs the
    desktop. Set `allow_any_caller = false` (the default) first: the
    extension must pass the caller check on its own.
    - Install: `scripts/install-extension.sh` (copies `extension/` to
      `~/.local/share/gnome-shell/extensions/correctiond-companion@nadelacruz-00.github.io/`).
      Log out and back in (Wayland), then
      `gnome-extensions enable correctiond-companion@nadelacruz-00.github.io`.
      `journalctl --user -b | grep correctiond-companion` shows no errors.
    - Focus Firefox: `busctl --user monitor org.nadelacruz.Correctiond1`
      shows `SetFocusedApp "firefox"` with no manual busctl call; focusing
      Ptyxis sends `org.gnome.ptyxis`; opening the overview search sends
      `""`. Type `the bulid ` in Firefox: it becomes `the build `.
    - Open GJS OSK: the monitor shows `SetOskVisible true` (false when it
      hides). With `auto_switch_osk = true` (the default) and
      `correctiond:us` among the input sources, the input source visibly
      switches to `correctiond:us` while the OSK is up and back to the
      previous one when it hides; with `auto_switch_osk = false` it does
      not switch. If `correctiond:us` is not an input source, the journal
      says so once and nothing switches. Tap-type a typo in a Firefox text
      field: the correction uses QWERTY geometry.
    - Switch the input source by hand while the OSK is up, then hide it:
      the hand-picked source stays (no restore over a user choice).
    - Per-window input sources (`gsettings set org.gnome.desktop.input-sources per-window true`):
      opening the OSK does NOT switch the source (the journal says so
      once), while `SetOskVisible true` still arrives. Focus another window
      while the OSK is up, hide it: no window is left on `correctiond:us`.
      Set it back to `false` afterwards; the next OSK show switches again
      without a relogin.
    - Quick settings: turn Autocorrect off; no corrections anywhere
      (`SetEnabled false` in the monitor). Lock and unlock the screen: it
      is still off. Turn it back on.
    - "Forget learned words" (the toggle's menu): a confirmation dialog;
      Cancel does nothing. Confirm: a "Learned words forgotten."
      notification, GetStats `words_learned` is 0 and `learn.db` is
      recreated empty under a new key. With the keyring locked: the
      notification reads "Learned words deleted; the key will be removed
      when your keyring unlocks".
    - `ibus restart`: the extension pushes the focused app, OSK state and
      on/off again as soon as the engine is back (monitor shows the three
      calls).
    - Disable the extension: the engine gets `SetFocusedApp ""` (and
      `SetOskVisible false` if the OSK was up), any switched input source
      is restored, and the quick toggle is gone.
14. **Task 12: vocabulary import** (the brief's Step 6, not run by the
    agent: controller ruling, never the live desktop, the real session bus
    services or the real keyring). The fake `SecretBackend` actor tests
    (`import_is_idempotent`, `import_refused_when_not_persistent`) cover the
    success, duplicate and locked paths, and `scripts/e2e.sh` checks the
    refusal end to end over a private bus. After `cargo build --release`,
    `scripts/install-dev.sh` and `ibus restart`, with the keyring unlocked:
    - Write the example from `docs/vocab-format.md` to `/tmp/t.vocab.tsv`
      (TAB separators). `target/release/engine import /tmp/t.vocab.tsv`
      prints `Imported 1 words and 1 word pairs.` and exits 0; GetStats
      `words_learned` grows by one (`ako`, count 12).
    - Run it again: `this file was already imported`, exit 1.
    - Lock the login keyring in Seahorse, `ibus restart`, and import a
      DIFFERENT file (add a line): `learning store not available; unlock
      your keyring and retry`, exit 1, and no prompt appears.
    - `journalctl --user -b | grep correctiond` shows no word and no path
      from the files.
    - Purge, then the first file imports again (the import records live in
      `learn.db` and go with it).
15. **Task 14: packaging and release** (the brief's Step 3, not run by the
    agent: it pushes, tags and installs system-wide). Local checks
    (`cargo deb -p engine`, `dpkg-deb -c/-I`) are done. On the desktop:
    - Push to GitHub and confirm CI is green; then
      `git tag v0.1.0 && git push --tags` and confirm the release has both
      `.deb` files and the extension zip.
    - `sudo apt install ./correctiond_*_arm64.deb`, `ibus restart`, and
      confirm the engine still works from the packaged path.
    - Crash-report check (must use the packaged binary: apport ignores
      unpackaged binaries under `target/`):
      `kill -SEGV $(pgrep -f /usr/lib/correctiond/engine)`, then
      `ls /var/crash` shows no `_usr_lib_correctiond_engine.*` file;
      `ibus restart` afterwards.
