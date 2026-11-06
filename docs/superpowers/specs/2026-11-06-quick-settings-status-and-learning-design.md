# Quick Settings: status at a glance and learning controls

Status: approved in brainstorming, 2026-11-06. Context awareness in scoring is a
separate design, not part of this one.

## Goal

The Autocorrect menu in Quick Settings has only an on/off toggle and "Forget
learned words", which wipes everything. Make it answer two questions at a
glance: "is it correcting here, and if not, why?" and "is it learning?". Also
let the user drop one bad learned word without wiping everything.

## Agreed scope

**What the user asked for:**
- **App state:** whether autocorrect is correcting in the focused app, and the
  reason when it isn't.
- **Counts:** corrections, undos (Backspace reverts) and words learned, since
  the engine started.
- **Pause learning:** a switch that stays off until the user turns it back
  on, or until the next engine restart or login.
- **Recently learned:** the last 5 words learned by typing, each with a ✕
  that forgets that one word.
- **Forget all** moves out of the menu into Settings (the extension's
  preferences window).

**Decided defaults:**
- **Counts** are per engine run (the existing `Counters`).
- **"Learned"** means a word that the user's typing takes from 1 use to 2 and
  that is in no dictionary. At 2 uses the engine stops correcting it
  (`decide()`: `personal_freq >= 2`). Imported words never enter the recent
  list.
- **The recent list** is in memory only. It is empty after a restart.
- **Forgetting a word** removes it and every pair containing it, both in
  memory and in the encrypted store. Typing it again can relearn it.
- **No new stored data.** No message text and no app names reach any output
  or log.

**Out of scope:**
- language and layout display;
- learning-health display;
- forgetting recent learning by time;
- live-updating menus;
- context-aware scoring.

## Menu

```
[✓] Autocorrect                         toggle (on/off for everything), as today
    Correcting in Firefox               subtitle

menu:
  Autocorrect                           header
  Correcting in Firefox                 app state line
  12 corrections · 3 undone · 9,000 words learned
  ─────────────────────────
  Learn new words                [●]    switch
  ─────────────────────────
  Recently learned
    kadto                         ✕     newest first, at most 5
    sige                          ✕
  (none yet this session)               when empty
  ─────────────────────────
  Autocorrect Settings…                 opens the preferences window
```

### App state line

`app_mode` and `app_reason` come from the Shell-only `GetStatus` (R4). The name
is the GNOME app name of the focused window (`Shell.WindowTracker`). Every
(app_mode, app_reason) pair and its text live in `data/status-vectors.tsv`
(R5).

| Condition (first match wins) | App line | Toggle subtitle |
|---|---|---|
| Engine not running | Engine not running | Engine not running |
| `enabled = false` | Autocorrect is off | Off |
| `app_mode = correct` | Correcting in NAME | Correcting in NAME |
| `app_mode = learn_only`, `not_listed` | Not correcting in NAME (not in your app list) | Learning only in NAME |
| `app_mode = learn_only`, `broken` | Not correcting in NAME (marked broken) | Learning only in NAME |
| `app_mode = learn_only`, `unidentified` | Not correcting here (app not identified) | Learning only |
| `app_mode = learn_only`, `no_replace` | Not correcting in NAME (this text field doesn't support it) | Learning only in NAME |
| `app_mode = off`, `private_field` | Off in this field (password or private) | Off in this field |
| `app_mode = off`, `terminal` | Off in this terminal (not in your app list, or marked broken) | Off in this field |
| `app_mode = off`, `no_replace` | Not correcting in NAME (this text field doesn't support it) | Off in this field |
| `app_mode = none` | No text field yet | Autocorrect |

**Subtitle rules:**
- When `learning = false`, the subtitle is "Learning paused". That wins over
  every row except "Engine not running" and "Off".
- When NAME is unknown (no focused window), the text drops "in NAME".

**Counts line:** "C corrections · R undone · W words learned", with
thousands separators. It is hidden when the engine is not running.

**Refresh:**
- The menu refreshes (`GetStats`, `GetStatus` and `RecentWords`) each time it
  opens.
- The toggle subtitle refreshes on focus-window changes, debounced to one
  `GetStatus` call per change.
- Nothing polls.

## Engine

### App state with a reason (`corelib/src/policy.rs`)

`resolve_mode_reason(ctx, allowlist, broken) -> (Mode, Reason)` returns the
same `Mode` as `resolve_mode`, plus why:

| Reason | When | Mode |
|---|---|---|
| `PrivateField` | an off purpose, or the no-spellcheck or private hint | Off |
| `Terminal` | a terminal not in the allowlist, or listed as broken | Off |
| `Allowlisted` | in the allowlist and not broken (terminal → ProseAware, else Silent) | Silent / ProseAware |
| `Broken` | listed as broken (not a terminal) | Observe |
| `NotListed` | identified, not in the allowlist | Observe |
| `Unidentified` | no WM_CLASS | Observe |

The engine's effective mode adds one more reason (R1): `NoReplace`, when
the mode is Silent or ProseAware but the field has no surrounding-text support
(`ibus.rs` `mode()`: Silent → Observe, ProseAware → Off).

`resolve_mode` becomes `resolve_mode_reason(..).0`; its existing tests stay
unchanged.

### The last field state (`engine/src/control.rs`, `engine/src/ibus.rs`)

- **Recording it (R2: computed on read).** `ControlState` stores the last
  focused IBus field's `(purpose, hints, can_replace)`. It is set at FocusIn,
  SetContentType and SetCapabilities, and not cleared on focus-out, because
  opening Quick Settings moves focus into the shell. The mode and reason are
  computed when `GetStatus` is called, from that field and the current focused
  WM_CLASS, so switching windows without typing still gives the new app's
  state.
- **What `GetStatus` reports** (Shell-only, R4; `GetStats` is unchanged):
  - `app_mode`: `correct` (Silent or ProseAware), `learn_only` (Observe),
    `off` (Off), or `none` (no field yet);
  - `app_reason`: `allowlisted` | `not_listed` | `broken` | `private_field`
    | `terminal` | `unidentified` | `no_replace`, or `""` with `none`;
  - `enabled` and `learning`.
- **No app identity.** Neither method carries a WM_CLASS or app name.
  `GetStats` stays open to any caller with counts only.

### Pause learning

- **The flag.** `ControlState` gains `learning: AtomicBool`, true at start.
- **The method.** New Shell-only method `SetLearning(b)`.
- **How it takes effect.** The IBus engine applies the flag through
  `EngineState::set_learning` before handling each key. That already gates
  word, pair and slip recording.
- **Lifetime.** Memory only, so an engine restart or login turns learning
  back on.

### Recently learned (`corelib/src/learning.rs`)

- **The list.** `LearnStore` gains `recent: VecDeque<String>`, capacity 5,
  newest first, in memory only.
- **Adding to it.** `record_word` returns the new count. `EngineState::learn`
  pushes the word when the count becomes exactly 2 and
  `!dict.contains(word)`. If the word is already in the list, it moves to the
  front.
- **What never adds to it.** `add_imported` and `import_totals` never touch
  `recent`. `clear()` (purge) empties it.
- **The method.** New Shell-only method `RecentWords() -> as`.

### Forget one word

- **The method.** New Shell-only method `ForgetWord(s) -> b`. It returns
  `false` when the word is not learned.
- **Memory side:** `LearnStore::forget_word(w)` lowercases `w` and removes:
  - the vocab entry;
  - every pair with `w` as either word;
  - every blacklist entry with `w` as the typed word or the correction
    (final review: otherwise a learned typo stays uncorrected after ✕);
  - `w` from the dirty vocab, dirty pair and dirty blacklist sets;
  - `w` from `recent`.
- **Disk side:** the persistence actor gets a `ForgetWord` command, serialized
  with attach, flush, import and purge. In one transaction it runs:
  - `DELETE FROM vocab WHERE word = ?1`
  - `DELETE FROM bigram WHERE w1 = ?1 OR w2 = ?1`
  - the blacklist rows with `w` on either side
- **Order.**
  - When persistent, the actor first deletes from the database, then clears
    memory under the store lock. A failed delete leaves memory untouched and
    returns an error.
  - When memory-only, it clears memory only.
- **Why order matters.** Because the actor serializes the command with
  flushes, a pending flush cannot write the word back after the delete.
- **Errors.** It uses a new error name,
  `org.nadelacruz.Correctiond1.Error.ForgetFailed`, with fixed text.

### Access

`SetLearning`, `RecentWords`, `ForgetWord` and `GetStatus` join `Method` with `shell_only()
= true`. They use the existing `authorize` check, and `allow_any_caller`
applies as for the other Shell-only methods.

## Extension

### `extension/extension.js`

- **The menu.** `AutocorrectToggle` builds the menu above:
  - a header;
  - two `PopupMenuItem` text rows (not reactive);
  - a `PopupSwitchMenuItem` "Learn new words";
  - a `PopupMenuSection` for the recent words, where each row has an
    `St.Button` with `window-close-symbolic`;
  - "Autocorrect Settings…", which calls the extension's `openPreferences()`.
- **Opening the menu.** `menu.connect('open-state-changed')` runs a refresh
  when the menu opens.
- **Calls.** All of them go out through the existing `Gio.DBus.session`
  proxy, with a 2-second timeout, except `Purge`, which uses 25 s because it
  can wait behind an import, a flush or the keyring delete.
  - The switch calls `SetLearning`.
  - ✕ calls `ForgetWord`, then refreshes. On failure, the row stays and
    shows "Could not forget" for 3 seconds.
- **Engine restart.** When the bus owner changes, the extension re-sends
  `SetEnabled`, focus and OSK as today, but not `SetLearning`: a restart
  turns learning back on, and the switch follows `learning` from
  `GetStatus` (read without echoing a `SetLearning`).
- **Older engine.** If `GetStats` works but `GetStatus` is an unknown
  method, the menu shows the counts and "No text field yet", and hides the
  switch and the list.
- **Removed.** The "Forget learned words" menu action is removed.
  `_confirmForget` stays.
- **Forget from Settings.** The extension exports
  `/org/nadelacruz/CorrectiondCompanion` on its session connection.
  Interface `org.nadelacruz.CorrectiondCompanion1` has one method,
  `ConfirmForget()`, which shows the existing Shell confirm dialog. Only the
  user's click on "Forget" calls `Purge`; Cancel is the default and focused
  button, so Return or Space never purges. The object is unexported in
  `disable()`.

### `extension/lib.js`

`statusText(stats, appName) -> {subtitle, appLine, countsLine}` implements the
table above. `stats` is `null` when the engine is down. It is pure and has no
GNOME imports.

### `extension/prefsWindow.js`

The Learning page gains a "Forget all learned words…" button row. It calls
`ConfirmForget` on `org.gnome.Shell` at
`/org/nadelacruz/CorrectiondCompanion`. If the call fails (extension
disabled), the row's subtitle says "Turn on the Autocorrect extension to use
this".

**Trade-off:** any session app could make the confirm dialog appear. It
cannot purge without the user's click.

## Error handling

- **Engine down or slow.** Every call has a timeout, so the menu never blocks
  the Shell. The menu shows "Engine not running" and hides the counts, the
  switch and the list.
- **Logging.** Failures are logged once per kind, with fixed text and never a
  word (the existing `_logOnce`).
- **`ForgetWord` errors:**
  - a database failure leaves both memory and disk unchanged, and the
    extension shows "Could not forget";
  - a word that isn't learned returns `false`, and the extension refreshes;
  - with a locked keyring (a store file exists but isn't attached), it is
    refused with "unlock your keyring first" and nothing changes (R3); with no
    store file at all, only memory is cleared.
- **Restarts.** After an engine restart, learning is on and the recent list
  is empty. The switch reflects `learning` from the next `GetStatus`.

## Testing

Tests are written first.

**corelib:**
- `resolve_mode_reason`: one case per reason, plus a property that `.0`
  equals `resolve_mode` for every existing test context.
- The recent list:
  - only the step from 1 to 2;
  - only non-dictionary words;
  - capped at 5, newest first, a repeat moves to the front;
  - an import never adds to it;
  - `clear()` empties it.
- `forget_word`: the word, its pairs, its dirty entries and its recent entry
  are gone; other words are untouched.
- Database: `forget_word` deletes from an encrypted store, and a flush queued
  before the delete does not bring the word back after reopening.

**engine:**
- `authorize`: the four new methods are Shell-only (denied otherwise,
  allowed with `allow_any_caller`).
- Over the wire on the private test bus: `SetLearning`, `RecentWords`,
  `ForgetWord` and `GetStatus`. `GetStatus` is Shell-only, and neither method
  contains a WM_CLASS.
- `EngineState`:
  - paused learning records nothing;
  - the app state is computed on read: switching apps without typing gives
    the new app's reason, focus-out keeps the last field, and a field without
    surrounding text gives `no_replace`;
  - ForgetWord while locked is refused, and after attach the word is still
    there, then forgetting works and survives reopen;
  - every (app_mode, app_reason) the engine can emit is in
    `data/status-vectors.tsv`;
  - a new engine starts with learning on.

**extension:**
- `extension/test/lib.test.js`: `statusText` for every row of
  `data/status-vectors.tsv`, the
  "Learning paused" subtitle, an unknown app name, engine down, and number
  formatting.
- `scripts/extension-smoke.sh` gets a new scenario:
  - `fake_correctiond.py` answers the new methods;
  - the menu is opened, the learning switch is toggled and a word is
    forgotten;
  - `ConfirmForget` is called and the dialog appears;
  - the regression contract (R6): toggle off/on sends SetEnabled(false/true);
    SetEnabled is re-pushed when the engine reappears, and SetLearning is
    not (the switch reads `learning` back);
    ConfirmForget then Forget gives exactly one Purge, and Cancel gives none;
  - it checks the expected calls and no stack traces.
- `extension/test/prefs.smoke.js`: the Forget-all row calls `ConfirmForget`
  (against a stub), and shows the disabled message when the call fails.

**On the desktop** (manual checklist):
- the menu names the right app in Firefox, a non-listed app and a terminal;
- the counts move after a correction and a revert;
- pausing stops learning (`words_learned` stays flat while typing new
  words), and the pause survives menu closes but not `ibus restart`;
- ✕ removes a word, and it stays gone after relogin;
- Settings → Forget all shows the Shell dialog.

## Files

- `corelib/src/policy.rs`
- `corelib/src/learning.rs`
- `corelib/src/db.rs`
- `engine/src/persist.rs`
- `engine/src/control.rs`
- `engine/src/state.rs`
- `engine/src/ibus.rs`
- `extension/extension.js`
- `extension/lib.js`
- `extension/prefsWindow.js`
- tests:
  - `extension/test/lib.test.js`
  - `extension/test/prefs.smoke.js`
  - `scripts/fake_correctiond.py`
  - `scripts/extension-smoke.sh`
- `data/status-vectors.tsv` (R5)
- `README.md`, where the menu is described.

## Engineering review (plan-eng-review, 2026-11-06)

Target: this spec, `docs/superpowers/specs/2026-11-06-quick-settings-status-and-learning-design.md`, which is also the report file. Base: `main` at 89a9ea0, where the Messenger import is already merged.

### Scope record

feature answers: D1 = A, "Move to Settings" (Forget all moves to Settings as designed); structure: D2 = A, Original arrangement; accepted scope: the spec as written above; pending remedies: R1-R4.

Scope Challenge result: scope accepted as-is.

### Reality rows

- REALITY: premise-table finding 1 of 9 premises wrong: the app-state table misses the "client can't replace" downgrade (engine/src/ibus.rs:553-557), see R1; unverified=0
- REALITY: already-done pass: none of this is on main (git log main -- extension/extension.js engine/src/control.rs: last 7599489, edb096a) engine/src/control.rs:205
- REALITY: surface-check pass: the named methods exist: set_learning engine/src/state.rs:115, authorize engine/src/control.rs:187, merge_snapshot engine/src/persist.rs:502, openPreferences is an Extension method
- REALITY: binding-decisions pass: there are no branch decisions; the store-encryption decision (SQLCipher via keyring) is respected, and forget goes through the same actor (engine/src/persist.rs:127)
- REALITY: numbers pass: "about 15 files" counted from the spec's Files list [measured: docs/superpowers/specs/2026-11-06-quick-settings-status-and-learning-design.md:318]; the 5-word list and 2 s timeout are design choices [estimated]
- REALITY: deferred-asks pass: there is no linked issue; the brainstorming answers are all covered (status, counts, pause, recent words) docs/superpowers/specs/2026-11-06-quick-settings-status-and-learning-design.md:15
- REALITY: pr-count pass: one branch, one PR (no .gstack/ship-policy.json) docs/superpowers/specs/2026-11-06-quick-settings-status-and-learning-design.md:1

## Decision ledger

### R1: an app state for clients that can't be corrected
Finding: 1, CRITICAL, confidence 9/10, engine/src/ibus.rs:553-557, reviewer: plan-eng-review (Architecture)
Plan baseline: the spec's app-state table has rows for correct / learn_only (not_listed, broken, unidentified) / off (private_field, terminal) / none.
Runtime evidence: `Mode::Silent if !self.can_replace() => Mode::Observe` and `Mode::ProseAware if !self.can_replace() => Mode::Off` (ibus.rs:554-556). An allowlisted app whose text field lacks surrounding-text support learns but is never corrected, and the table has no row for it.
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| R1 reason for can't-replace | none (table gap) | new reason `no_replace`: "Not correcting in NAME (this text field doesn't support it)", subtitle "Learning only in NAME" | none (the menu shows it as allowlisted; wrong text) |
| R2-R4 | pending | pending | pending |
Question D3: an app state for text fields autocorrect can't fix
Header: Can't-fix row
Options:
A) Add a no_replace reason (recommended)
The app state is computed from the engine's effective mode, and a new reason `no_replace` covers allowlisted apps whose field has no surrounding-text support. Row: "Not correcting in NAME (this text field doesn't support it)". It needs 1 table row, 1 policy test and 1 lib.test row.
B) Leave the table as is
Such fields report as allowlisted, so the menu would say "Correcting in NAME" while nothing is corrected.

State: approved
Actual answer: A, "Add a no_replace reason" (D3, 2026-11-06)
Accepted scope: the app state comes from the effective mode (ibus.rs `mode()`), and a new reason `no_replace` applies when an allowlisted field lacks surrounding-text support. Text: "Not correcting in NAME (this text field doesn't support it)", with subtitle "Learning only in NAME". Tests: 1 engine test, 1 lib.test row.
History: none

### R2: where the app state comes from (stale after switching windows)
Finding: 2, CRITICAL, confidence 8/10, spec "The last field state" + extension/extension.js:221-227, reviewer: plan-eng-review (Architecture)
Plan baseline: the IBus engine records `last_field` each time it resolves the mode for a focused field.
Runtime evidence: the mode is resolved on keys and content-type changes (ibus.rs:578, 658-661). Switching to another window without typing leaves `last_field` from the old app, while the extension shows the new window's name. Example: "Correcting in Terminal" while still in Firefox's state.
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| R2 source of app state | recorded at mode resolution | computed when GetStats/GetStatus is read: current focused wm_class + the last IBus field's purpose/hints/capabilities (stored on FocusIn, SetContentType and SetCapabilities) | also clear `last_field` on SetFocusedApp to a different app (D-Bus ordering between the two connections can wipe the right state) |
| R1, R3, R4 | pending | pending | pending |
Question D4: computing the app state when it is read
Header: App state
Options:
A) Compute on read (recommended)
The engine stores the last focused field's content type and capabilities at FocusIn, SetContentType and SetCapabilities. It computes mode and reason when asked, with the currently focused app. The menu always pairs the right app with the right state. It needs 1 engine test (switch app without typing → the new app's reason).
B) Record and clear on app change
It keeps recording at resolution and clears on SetFocusedApp changes. It is simpler to describe, but FocusIn and SetFocusedApp arrive on different connections, so a late SetFocusedApp can wipe the correct state and show "No text field yet".

State: approved
Actual answer: A, "Compute on read" (D4, 2026-11-06)
Accepted scope: `ControlState` stores the last focused IBus field's (purpose, hints, can_replace), set at FocusIn, SetContentType and SetCapabilities and never cleared on focus-out. The app mode and reason are computed when read, with the current focused wm_class (resolve_mode_reason plus the can_replace downgrade from R1). `none` is returned until a field has focused. Tests: 1 engine test (switch app without typing → the new app's reason), plus focus-out keeps the last field.
History: none

### R3: a word forgotten while the keyring is locked comes back
Finding: 3, CRITICAL, confidence 9/10, engine/src/persist.rs:502 (`s.merge_snapshot(snap)`) + :598 ("will retry"), reviewer: plan-eng-review (Architecture)
Plan baseline: when the store is memory-only, forgetting clears memory only.
Runtime evidence: a locked keyring makes the store memory-only, and the actor keeps retrying attach. When it attaches, it merges the on-disk snapshot into memory, so a word forgotten while locked returns.
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| R3 forget while not attached | memory only (word returns on attach) | refuse: ForgetFailed "unlock your keyring first"; the row stays; allowed when no store file exists on disk | in-memory tombstones skipped by the attach merge and deleted on attach (lost on engine restart, so the word can still return) |
| R1, R2, R4 | pending | pending | pending |
Question D5: forgetting a word while the keyring is locked
Header: Locked store
Options:
A) Refuse while locked (recommended)
If an encrypted store exists but isn't attached yet, ForgetWord fails with "unlock your keyring first" and the row shows that. If no store file exists, memory-only forgetting is safe and allowed. It needs 1 persist test (locked → refused, attach → no resurrection).
B) Tombstones until attach
Forget works while locked, and the forgotten words are kept in memory and deleted on attach. If the engine restarts before attach, the tombstones are lost and the word comes back. It adds more code and 2 tests.

State: approved
Actual answer: A, "Refuse while locked" (D5, 2026-11-06)
Accepted scope: if a store file exists but the store isn't attached, ForgetWord fails with ForgetFailed "unlock your keyring first" and memory is untouched; the extension row shows "Unlock your keyring first". If no store file exists, it forgets from memory only. Test: locked → refused; after attach, the word is still there and forgetting then works and survives reopen.
History: none

### R4: app state over the open GetStats
Finding: 4, INFORMATIONAL, confidence 6/10, engine/src/control.rs:366 ("Open to any caller: counts and booleans only"), reviewer: plan-eng-review (Architecture)
Plan baseline: GetStats gains app_mode, app_reason, enabled and learning, and stays open to any caller.
Runtime evidence: GetStats is callable by any session client. `app_reason = private_field` would tell any local app the moment you are in a password field.
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| R4 where app state is served | GetStats (open) | new Shell-only `GetStatus` (app_mode, app_reason, enabled, learning); GetStats unchanged | GetStats (open), as in the spec |
| R1-R3 | pending | pending | pending |
Question D6: serving the app state Shell-only
Header: App state access
Options:
A) Shell-only GetStatus (recommended)
App state, enabled and learning come from a new Shell-only method, and GetStats keeps today's counts-only contract. The menu makes 3 calls when it opens (GetStats + GetStatus + RecentWords). It needs 1 authorize test row.
B) Keep it in GetStats
One call fewer, but any session app can watch when you enter password fields or which mode you're in.

State: approved
Actual answer: A, "Shell-only GetStatus" (D6, 2026-11-06)
Accepted scope: new Shell-only method `GetStatus() -> s`, a JSON with app_mode, app_reason, enabled and learning. GetStats is unchanged (counts and booleans only, no app state). The extension calls GetStats + GetStatus + RecentWords when the menu opens, and GetStatus for the subtitle on focus changes. Test: an authorize row (Shell-only) and a wire test.
History: none

### R5: keep the reason codes and the menu text in step
Finding: 5, INFORMATIONAL, confidence 7/10, spec "App state line" table vs `resolve_mode_reason`, reviewer: plan-eng-review (Code Quality, red flag 8: hand-synced list)
Plan baseline: the reason codes are strings in Rust (control.rs) and in the extension's statusText (lib.js), each tested on its own.
Runtime evidence: the repo already uses shared vector files for this, e.g. data/primary-lang-vectors.tsv (Python converter vs corelib).
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| R5 cross-language check | none | `data/status-vectors.tsv` (app_mode, app_reason → expected appLine template); a Rust test asserts every reason the engine can emit is listed, and lib.test.js asserts statusText for each line | none; each side tested alone |
| R6 | pending | pending | pending |
Question D7: a shared vector file for status codes
Header: Status codes
Options:
A) Shared vectors file (recommended)
One TSV lists every app_mode/app_reason pair and its menu text. Rust fails if the engine can emit a pair not in the file, and lib.test.js checks the text for each row. Adding a reason in one place without the other breaks the build.
B) Test each side alone
Less wiring, but a new Rust reason shows up as a blank or wrong line in the menu with every test green.

State: approved
Actual answer: A, "Shared vectors file" (D7, 2026-11-06)
Accepted scope: `data/status-vectors.tsv` holds every (app_mode, app_reason) pair with its appLine and subtitle templates. A Rust test checks that every pair the engine can emit is listed, and lib.test.js checks statusText against each row.
History: none

### R6: regression contract for the rebuilt menu
Finding: 6, CRITICAL (regression rule), confidence 8/10, extension/extension.js:45-50 (toggle + "Forget learned words"), scripts/extension-smoke.sh:104-113, reviewer: plan-eng-review (Test review)
Plan baseline: the menu is rebuilt and the "Forget learned words" action is removed. The smoke checks SetEnabled pushes at start and on re-enable only.
Runtime evidence: nothing tests that flipping the toggle sends SetEnabled(false)/(true), or that Purge happens only after the dialog's Forget click.
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| R6 regression contract | untested | preserve: toggle off → SetEnabled(false), on → SetEnabled(true); state re-pushed when the engine reappears (now incl. SetLearning); Purge only after the dialog's Forget click, Cancel sends nothing. Asserted in extension-smoke via calls.log | manual desktop check only |
Question D8: the regression contract for the rebuilt menu
Header: Menu regression
Options:
A) Smoke-test the contract (recommended)
The extension-smoke scenario asserts the four behaviors above from the fake engine's calls.log, including Cancel → no Purge.
B) Manual check only
It relies on the desktop checklist, so a menu rebuild that drops a signal handler would ship unnoticed.

State: approved
Actual answer: A, "Smoke-test the contract" (D8, 2026-11-06)
Accepted scope: an extension-smoke scenario that asserts from calls.log:
1. toggle off → SetEnabled(false), and on → SetEnabled(true);
2. when the engine reappears, SetEnabled is re-pushed with the current value. (Amended during execution: SetLearning is not re-pushed; a restart turns learning back on, per the user's brainstorming answer and the Pause learning Lifetime line.)
3. ConfirmForget → dialog → Forget → exactly one Purge;
4. ConfirmForget → dialog → Cancel → no Purge.
History: none

Approval readiness: PASS. Checked R1 (D3 = A), R2 (D4 = A), R3 (D5 = A), R4 (D6 = A), R5 (D7 = A) and R6 (D8 = A), all answered on 2026-11-06. Scope: D1 = A, D2 = A.

## Review sections

### 1. Architecture

- [CRITICAL] (confidence: 9/10) engine/src/ibus.rs:554-556: the status table missed the can't-replace downgrade. → R1, approved.
- [CRITICAL] (confidence: 8/10) spec "The last field state": state recorded at mode resolution goes stale after a window switch without typing. → R2, approved (compute on read).
- [CRITICAL] (confidence: 9/10) engine/src/persist.rs:502: a word forgotten while the store is locked is restored by the attach merge. → R3, approved (refuse while locked).
- [INFORMATIONAL] (confidence: 6/10) engine/src/control.rs:366: app state in the open GetStats would expose password-field timing to any session app. → R4, approved (Shell-only GetStatus).

**Design twice** (new public surface: 4 control methods and the `CorrectiondCompanion1` object):
- **Shape A (chosen).** The engine owns all state. It has a small set of Shell-only methods; the extension is a thin view.
- **Shape B.** The extension owns the pause flag and the recent list, by watching typing through IBus. Rejected: the extension can't see learning, and it would duplicate state.

A wins on depth: the policy, the store and the counters stay behind the engine's interface.

**Red flags:**
- 8 (hand-synced list: reason codes in Rust and JS) → R5.
- 5 (split ownership of the learning flag: `ControlState.learning` and `EngineState.learning`). Accepted: ControlState is the source, and the IBus engine applies it per key. Replay sets its own copy because it has no ControlState.
- No others.

**Production failure per new path:**
- **Engine hung.** The 2 s timeout means the menu shows "Engine not running".
- **D-Bus export collides on Shell reload.** It is unexported in disable() and re-exported in enable().
- **Locked keyring.** Covered by R3.

### 2. Code quality

- [INFORMATIONAL] (confidence: 7/10) spec "App state line" vs resolve_mode_reason: hand-synced lists. → R5, approved.
- [INFORMATIONAL] (confidence: 7/10) corelib/src/learning.rs:60: `record_word` changes to return the new count. Every caller is in state.rs and tests, so it is a mechanical change and needs no decision.

Dispositions: R5 accepted (D7).

### 3. Tests

```
CODE PATHS                                              USER FLOWS
[+] corelib/policy.rs resolve_mode_reason               [+] Open menu
  ├── [GAP→planned] each Reason, .0 == resolve_mode       ├── [GAP→planned] right app + state (smoke)
[+] engine control GetStatus (computed on read)           ├── [GAP→planned] switch app, no typing
  ├── [GAP→planned] no_replace (caps without surr.)       └── [GAP→planned] engine down → message
  ├── [GAP→planned] focus-out keeps last field          [+] Learn new words switch
  └── [GAP→planned] Shell-only (authorize row)            ├── [GAP→planned] paused → nothing recorded
[+] LearnStore recent / forget_word                       └── [GAP→planned] restart → on again
  ├── [GAP→planned] 1→2 only, non-dict, cap 5           [+] ✕ forget word
  └── [GAP→planned] forget removes word+pairs+dirty       ├── [GAP→planned] [→E2E] gone after reopen
[+] persist ForgetWord                                    └── [GAP→planned] locked → refused (R3)
  ├── [GAP→planned] DB delete + queued flush            [+] Settings Forget all
  └── [GAP→planned] locked → ForgetFailed                 ├── [GAP→planned] [→E2E] Forget → 1 Purge
[+] extension statusText                                  └── [GAP→planned] [→E2E] Cancel → no Purge
  └── [GAP→planned] every status-vectors.tsv row        [+] Toggle (regression, R6)
[+] existing: SetEnabled push at start/re-enable          └── [GAP→planned] [→E2E] off/on → SetEnabled
  └── [★★ TESTED] scripts/extension-smoke.sh:104-113

COVERAGE today: 1/22 paths tested (5%); every gap above has a planned test in the Testing section
QUALITY: ★★:1 | GAPS: 21 (5 E2E via extension-smoke.sh; 0 eval, no LLM code)
```

- **Regression rule:** R6 approved (D8).
- **Test Plan Artifact:** (kept outside the repo)
- **Tests made obsolete:** none.

### 4. Performance

- [INFORMATIONAL] (confidence: 7/10) corelib/src/learning.rs:287: `learned_word_count` scans the vocab (about 9k entries [measured: GetStats words_learned]) under the store lock on every GetStats. GetStats now runs only when the menu opens (GetStatus serves focus changes), so the cost is negligible. No action.

No other issues: no per-key D-Bus calls, no polling, and RecentWords is capped at 5.

## NOT in scope

- **Language and layout display.** The user didn't pick it in brainstorming.
- **Learning health ("saved, encrypted" vs memory-only).** Not picked.
- **Forget recent learning by time.** Not picked; vocab has no first-seen time.
- **A recent list that survives restarts.** The user kept it memory-only.
- **Live-updating menus (signals).** Approach A was chosen.
- **Tombstones for forget-while-locked.** R3 chose to refuse instead.
- **Context-aware scoring.** A separate design, to follow.

## What already exists

- `resolve_mode` (corelib/src/policy.rs:72) is reused, wrapped by `resolve_mode_reason`.
- `authorize` and `Method::shell_only` (engine/src/control.rs:149, 187) are reused for the 4 new methods.
- `EngineState::set_learning` (engine/src/state.rs:115) is reused for pause.
- The persistence actor serializes commands (engine/src/persist.rs:127 purge, :141 import); ForgetWord is added the same way.
- The extension's proxy, `_logOnce` and `_confirmForget` dialog (extension/extension.js:354) are reused; ConfirmForget calls `_confirmForget`.
- The vectors-file pattern is reused from data/primary-lang-vectors.tsv.
- The headless Shell harness (scripts/extension-smoke.sh, scripts/fake_correctiond.py) is extended.

## Failure modes

| New path | Realistic failure | Test / handling | User sees |
|---|---|---|---|
| GetStatus | engine hung | 2 s timeout, lib.test engine-down row | "Engine not running" |
| App state | window switched, no typing | R2 compute on read + engine test | the correct new app's state |
| ForgetWord | keyring locked | R3 refusal + persist test | "Unlock your keyring first" |
| ForgetWord | DB write fails | ForgetFailed, memory untouched, test | "Could not forget" (3 s) |
| SetLearning | engine restarted while paused | re-push on owner change (R6 smoke) | the switch shows the real state |
| ConfirmForget | extension disabled | prefs row message, prefs.smoke test | "Turn on the Autocorrect extension…" |
| Status text | new reason without text | status-vectors.tsv test (R5) | (caught before ship) |

There are no critical gaps: every failure has handling and a planned test, and none is silent.

## Worktree parallelization

| Step | Modules touched | Depends on |
|------|----------------|------------|
| A. Policy reason + vectors file | corelib/, data/ | — |
| B. Recent list + forget_word + DB delete | corelib/ | — |
| C. Control/persist/ibus methods | engine/ | A, B |
| D. Extension menu + lib + prefs + smoke | extension/, scripts/ | C (method shapes; it can start against fake_correctiond.py) |

- **Lanes:**
  - Lane A: A → B, run sequentially because both touch corelib/.
  - Lane B: D, against fake_correctiond.py.
  - Then C, which needs corelib.
- **Execution order:** launch Lanes A and B; merge A; then C; then the final integration of D against the real engine.
- **Conflict flags:** corelib/ is shared by A and B, so they are sequential. The D-Bus method signatures are shared by C and D, so pin them in fake_correctiond.py first.

## Implementation Tasks
Synthesized from this review's findings. Each task derives from a specific finding above. Check each box as it ships.

- [ ] **T1 (P1, human: ~1h / CC: ~5min)**: engine: `no_replace` reason from the effective mode
  - Surfaced by: Architecture, R1 (engine/src/ibus.rs:554-556)
  - Files: corelib/src/policy.rs, engine/src/control.rs, data/status-vectors.tsv
  - Verify: `cargo test -p engine` (no_replace case)
- [ ] **T2 (P1, human: ~2h / CC: ~10min)**: engine: compute the app state on read from the stored last field and the current app
  - Surfaced by: Architecture, R2
  - Files: engine/src/control.rs, engine/src/ibus.rs
  - Verify: an engine test that switches apps without typing
- [ ] **T3 (P1, human: ~1h / CC: ~10min)**: persist: refuse ForgetWord while a store file exists but isn't attached
  - Surfaced by: Architecture, R3 (engine/src/persist.rs:502)
  - Files: engine/src/persist.rs, engine/src/control.rs, extension/extension.js
  - Verify: a persist test (locked → refused; after attach, forget survives reopen)
- [ ] **T4 (P1, human: ~1h / CC: ~5min)**: control: a Shell-only `GetStatus`, with GetStats unchanged
  - Surfaced by: Architecture, R4 (engine/src/control.rs:366)
  - Files: engine/src/control.rs, scripts/fake_correctiond.py, extension/extension.js
  - Verify: an authorize test row plus a wire test
- [ ] **T5 (P2, human: ~1h / CC: ~10min)**: tests: `data/status-vectors.tsv`, checked from Rust and lib.test.js
  - Surfaced by: Code Quality, R5
  - Files: data/status-vectors.tsv, engine/src/control.rs (test), extension/test/lib.test.js
  - Verify: `cargo test` plus `gjs -m extension/test/lib.test.js`
- [ ] **T6 (P1, human: ~3h / CC: ~20min)**: smoke: the regression contract for the toggle, the re-push and ConfirmForget/Purge
  - Surfaced by: Test review, R6 (extension/extension.js:45-50, scripts/extension-smoke.sh:104-113)
  - Files: scripts/extension-smoke.sh, scripts/fake_correctiond.py
  - Verify: `bash scripts/extension-smoke.sh`

_No new tasks from Performance._

## Unresolved decisions

None.

## Completion summary

- Step 0: Scope Challenge: scope accepted as-is.
- Reality rows: 7/7 applicable rows emitted (PLAN_REALITY below), 0 unverified premises.
- Architecture Review: 4 issues found (R1-R4, all approved).
- Code Quality Review: 2 issues found (R5 approved; the record_word signature needs no decision).
- Test Review: diagram produced, 21 gaps identified, all with planned tests; regression contract R6 approved.
- Performance Review: 0 issues found (1 informational note, no action).
- NOT in scope: written.
- What already exists: written.
- TODOS.md updates: 0 items proposed (no deferrable work: the deferred options were rejected outright).
- Failure modes: 0 critical gaps flagged.
- Unresolved decisions: 0 in this review.
- Outside voice: codex, unavailable (codex not installed; the native Plan fallback gave no complete report within five minutes).
- Parallelization: 3 lanes, 2 parallel / 1 sequential.
- Lake Score: 5/6 = answers picking a 10/10 option / answers scored for Completeness (D3, D4, D6, D7, D8 at 10/10; D5 picked A at 9/10, the highest offered).

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` | Scope & strategy | 0 | — | not run (skipped at the user's request) |
| Outside Review | codex (`/plan-eng-review` outside voice) | Independent 2nd opinion | 1 | unavailable | codex not installed; native fallback timed out, no findings credited |
| Eng Review | `/plan-eng-review` | Architecture & tests (required) | 1 | issues_open | 6 issues, 0 critical gaps (all 6 resolved: R1-R6 approved) |
| Design Review | `/plan-design-review` | UI/UX gaps | 0 | — | — |
| DX Review | `/plan-devex-review` | Developer experience gaps | 0 | — | — |

- **OUTSIDE COVERAGE:** codex, plan-review phase, unavailable (not installed). The native Plan-agent fallback did not finish within five minutes and was stopped, so there is no outside coverage.
- **VERDICT:** no review is CLEAR. The Eng Review found 6 issues, all resolved into approved plan changes (R1-R6), so the status stays issues_open: the work is mapped, not failing. The plan is ready to implement with those amendments. eng review required
NO UNRESOLVED DECISIONS
