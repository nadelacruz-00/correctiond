# Settings: browse, search, sort and forget learned words

Status: approved in brainstorming, 2026-11-06.

## Goal

A Settings page where the user can find specific learned words and forget
them: search, sort A→Z / Z→A / newest / oldest, and delete one word or
several, with undo. Learned words stay private. Only GNOME Shell, or a
Settings window the user explicitly allowed through a Shell prompt, can
read them.

## Agreed scope

**What the user asked for:**
- **Which words.** Personal words by default: words that appear in no
  English, Tagalog or Waray list. A switch shows every stored word.
- **Chronological.** "Newest" and "oldest" mean *last used*, the existing
  `last_seen` timestamp. There is no schema change. Imported words show the
  import time until they are typed again.
- **Deleting.** A per-row delete and select-several delete. Both show an
  Undo toast for 5 s. Deleting several also asks "Forget N words?" first,
  with Cancel as the default.
- **Access.** Settings asks the Shell; the Shell shows a prompt
  *every time* (not remembered); only that Settings window may list and
  forget words, until it closes.

**Not in scope:**
- a "first learned" date;
- remembering the Allow;
- editing or renaming words;
- exporting the list;
- live updates while the page is open (there is a Refresh button);
- listing word pairs, blacklist entries or slips.

**Inherited behavior:** forgetting removes the word, every pair with it
on either side, and every blacklist entry with it on either side. This is
exactly like ✕ in Quick Settings (`LearnStore::forget_word`, `Db::forget_word`).

## The page

A new page in the extension's Settings window, titled "Words" and built by
`extension/wordsPage.js`:

```
Hidden state:
  Learned words
  Your learned words are hidden until you allow it.
                      [ Show learned words… ]

Allowed state:
  [🔍 Search words…           ]   Sort: [Newest first ▾]   [⟳]
  Show all stored words                                   [ ○ ]
  2,314 words
  ☐ kadto        used 41 times · last used today         🗑
  ☐ sige         used 12 times · last used 3 Nov         🗑
  …
  (action bar when any box is ticked)
  2 selected                    [ Clear ]  [ Forget selected ]

Toasts: "Forgot kadto" [Undo]     "Forgot 2 words" [Undo]
```

- **Sorts:**
  - A→Z and Z→A compare `toLowerCase()` words.
  - "Newest first" and "Oldest first" compare `last_seen`.
  - Ties break by A→Z word.
- **Search:** case-insensitive, matching anywhere in the word, applied as
  you type.
- **The switch:** off shows `personal == true` rows; on shows all rows. The
  word count line reflects the rows shown.
- **"Last used" text:**
  - "today" or "yesterday";
  - otherwise "D Mon" within the current year, or "D Mon YYYY" for older dates;
  - "never" for `last_seen == 0`.
- **Selection:** tracked by word, not row position, so it survives search
  and sort changes. Clear empties it.
- **Undo** (amended by R8):
  - the rows leave the list immediately;
  - the words join one pending batch;
  - one toast "Forgot N words" [Undo] stays up while its 5 s timer restarts
    on each delete;
  - Undo restores the whole batch;
  - when the timer fires, the batch is sent in one `ForgetWords` call;
  - closing the window sends it at once, flushes the connection and lets the
    close go ahead (R3, amended: GNOME's prefs window must never be held or
    destroyed by the page).
- **Deleting several** asks first, with an `Adw.AlertDialog`: "Forget N
  words?", Cancel (default) and Forget (destructive).
- **The list widget:** a `Gtk.ListView` over a `Gio.ListStore`, through a
  `Gtk.FilterListModel` (search and switch) and a `Gtk.SortListModel`.
  about 10,000 rows must scroll smoothly.

## Access

These sections were amended by the eng review (R1-R10). Everything goes
through GNOME Shell, the way Forget all already works.

```
Words page (own private D-Bus connection)
   │ RequestWordAccess()                      ┌── checks: caller PID == owner PID of
   ▼                                          │   org.gnome.Shell.Extensions, focused
Shell companion object ───────────────────────┤   window is that PID's org.gnome.Shell.Extensions
   │ prompt "Show your learned words…?"       └── window, caller still connected
   │ Allow → add caller name to the allowed set (max 2; watched, dropped on vanish)
   │
   │ ListWords() / ForgetWords(as)  (only for an allowed caller)
   ▼
engine (Shell-only ListWords / ForgetWords)
```

1. **The private connection.** The Words page opens its own connection
   (`Gio.DBusConnection.new_for_address` on the session bus address,
   `AUTHENTICATION_CLIENT | MESSAGE_BUS_CONNECTION`) and makes every call on
   it. Closing the page closes the connection (R1).
2. **The request.** The page calls `RequestWordAccess() -> b` on
   `org.gnome.Shell` `/org/nadelacruz/CorrectiondCompanion`
   `org.nadelacruz.CorrectiondCompanion1`, with a 120 000 ms timeout.
3. **The checks before any prompt** (R2, R5). The companion replies `false`
   at once, with no prompt and one fixed-text log line, unless all three
   hold:
   - the caller's PID equals the PID of the owner of
     `org.gnome.Shell.Extensions` (`GetNameOwner` and
     `GetConnectionUnixProcessID`);
   - `global.display.focus_window?.get_pid()` equals the caller's PID;
   - `focus_window.get_wm_class() === 'org.gnome.Shell.Extensions'`. Amended
     after the final review: GNOME's real prefs service is not a
     Gtk.Application, so its window has no GTK app id. Its wm_class was
     measured from the real service in the headless smoke.

   A request while a prompt is already open also gets `false`.
4. **The prompt.** It is a Shell `ModalDialog`:
   - text: "Show your learned words in Settings? Settings can list and
     forget your learned words until you close it.";
   - buttons: **Cancel** (default, with key Escape, so Return and Space
     cancel) and **Allow**.

   On Allow, the companion checks that the caller still has an owner (R9).
   If it does, the caller joins the allowed set and the reply is `true`;
   otherwise `false`. Cancel, or the prompt closing on `disable()`, gives
   `false`.
5. **The allowed set** (Shell, R4). It holds at most 2 unique names, oldest
   replaced. Each is watched with `Gio.bus_watch_name_on_connection` and
   dropped when it vanishes. It is cleared on `disable()`, and never stored.
6. **Companion pass-through methods,** for allowed callers only (otherwise
   a D-Bus `AccessDenied` error):
   - `ListWords() -> a(sutb)`
   - `ForgetWords(as) -> u`

   They call the engine as the Shell, with the engine's error passed
   through by name and message.
7. **Engine methods** (Shell-only, even with `allow_any_caller`, R9):

   | Method | Behavior |
   |---|---|
   | `ListWords() -> a(sutb)` | (word, count, last_seen, personal) for every stored word with count ≥ 2; `personal` = in no loaded dictionary; unsorted (R6, R9) |
   | `ForgetWords(as) -> u` | 1-10 000 words, each 1-64 bytes, else `ForgetFailed` "could not forget"; one persistence request and one transaction (R8); returns how many existed; while locked: `ForgetFailed` "unlock your keyring first" |

   No log line contains a word.

## Code

**corelib:**
- `LearnStore::list_words(&self, is_known: &dyn Fn(&str) -> bool) -> Vec<(String, u64, i64, bool)>`
- `LearnStore::forget_words(&mut self, ws: &[String]) -> u32`: a single pass (R8). `forget_word(w)` is `forget_words(&[w]) == 1`.
- `Db::forget_words(&mut self, ws: &[String]) -> Result<(), StoreError>`: one transaction, with chunked `IN (...)` deletes of ≤ 500 words per chunk over vocab, bigram and blacklist. `Db::forget_word` calls it.

**engine:**
- **persist.rs:** `Req::Forget` carries a `Vec<String>`. `PersistHandle::forget_words(Vec<String>) -> Result<u32, String>` has the same rules as today's forget, and `forget_word` wraps it.
- **control.rs:**
  - `Method::{ListWords, ForgetWords}`, both Shell-only;
  - `authorize` ignores `allow_any_caller` for these two (R9);
  - `ControlState`'s existing `forget_word` path is reused for ForgetWord.

**extension:**
- **extension.js:**
  - the companion interface gains `RequestWordAccess` (async), `ListWords` and `ForgetWords`;
  - the allowed set with name watches;
  - the request checks;
  - the prompt, built like `_confirmForget`;
  - the engine proxy XML gains `ListWords` and `ForgetWords`.
- **lib.js** (pure): `compareWords(a, b, sort)`, `matchesSearch(word, query)`, `lastUsedText(lastSeen, now)`, and `class PendingForget` (`add(words) -> count`, `undo() -> words`, `take() -> words` once).
- **wordsPage.js:**
  - the page and its private connection;
  - one batched toast whose 5 s timer restarts on each delete (R8);
  - the close guard (R3, R8);
  - injectable `opts.wordsCalls = {requestAccess, listWords, forgetWords, close}`.
- **prefsWindow.js:** adds the page.

## Error handling

| Situation | User sees |
|---|---|
| Not focused or wrong process (checks fail) / prompt cancelled / no answer in 120 s | "Not allowed"; the button can be pressed again |
| Extension disabled (RequestWordAccess fails) | "Turn on the Autocorrect extension to use this" |
| ListWords fails (engine down) | "Couldn't load your learned words" + Retry (asks the Shell again) |
| Access lost (AccessDenied / UnknownObject, e.g. after screen lock) | back to the hidden state, "Not allowed"; failed rows are restored |
| Older engine (companion present, ListWords UnknownMethod) | "Update correctiond to use this" |
| Prompt unanswered for 115 s, or the asking page closed | the prompt closes; `false` |
| ForgetWords: keyring locked | rows come back; toast "Unlock your keyring first" |
| ForgetWords: other failure | rows come back; toast "Could not forget" |
| Close with pending deletes | the batch is sent and the connection flushed, then the window closes normally; a failure is not shown |
| Word already forgotten elsewhere | harmless; the returned count is lower |

## Testing

Tests are written first.

**corelib:**
- `list_words`: count ≥ 2 only, the personal flag through `is_known`, imported words included;
- `forget_words`: a batch with pairs and blacklist on both sides; a missing word not counted; unrelated rows kept; a 1 000-word batch; existing forget tests green;
- db `forget_words`: chunking above 500, and reopen.

**persist:** a batch commits and survives restart; refused while locked with nothing changed; memory-only without a store file.

**control:**
- ListWords/ForgetWords are Shell-only, denied otherwise, including with `allow_any_caller`;
- size caps;
- wire tests;
- no reply except ListWords carries a word.

**lib.test.js:** the 4 sorts with ties; search case and substring;
lastUsedText (today, yesterday, this year, older year, 0); PendingForget
add, undo and take-once.

**extension-smoke.sh:**
- the fake engine answers ListWords and ForgetWords;
- a `gdbus` caller (another PID) gets `false` with no dialog;
- a caller in the name owner's process with its window focused, set up via Eval, gets the prompt;
- Allow gives `true`;
- the companion's ListWords works only after Allow;
- closing the caller's connection revokes access;
- Cancel and Return give `false`;
- a second request during the prompt gives `false`;
- disable during the prompt gives `false`.

**prefs.smoke.js** (with stubs):
- hidden, then Allow, then loaded;
- the switch filters with no second ListWords;
- search and the 4 sorts;
- 3 quick deletes give one toast, "Forgot 3 words", and one ForgetWords after 5 s;
- Undo restores all;
- multi-select gives the confirm, then Forget;
- close with pending sends once and closes once; a stuck stub closes after 3 s;
- the not-allowed, disabled, load-failure and locked texts.

**Desktop checklist:**
- open Settings → Words → Show → Allow; with Show all on, the count matches the menu's words learned;
- search "kad";
- sort each way;
- forget one word, then Undo;
- forget for real, and it is gone after relogin;
- forget 3 selected words through the confirm;
- Cancel the prompt, and the page stays hidden;
- run `gdbus call` from a terminal, and it gets no prompt.

## Files

- `corelib/src/learning.rs`
- `corelib/src/db.rs`
- `engine/src/persist.rs`
- `engine/src/control.rs`
- `extension/extension.js`
- `extension/lib.js`
- `extension/wordsPage.js` (new)
- `extension/prefsWindow.js`
- `scripts/fake_correctiond.py`
- `scripts/extension-smoke.sh`
- `scripts/install-extension.sh` and `scripts/pack-extension.sh` (add wordsPage.js)
- `extension/test/lib.test.js`
- `extension/test/prefs.smoke.js`

## Engineering review (plan-eng-review, 2026-11-06)

Target: this spec, which is also the report file. Base: main 85d2a92.

### Scope record

feature answers: none proposed; structure: D1 = A, Original arrangement; accepted scope: the spec as written above; pending remedies: R1-R3.

Scope Challenge result: scope accepted as-is.

### Reality rows

- REALITY: premise-table finding 1 of 6 premises wrong: "until it closes" assumes one process per Settings window, but extension prefs run in the shared D-Bus-activated service /usr/share/dbus-1/services/org.gnome.Shell.Extensions.service (see R1), engine/src/control.rs:557-559; unverified=0
- REALITY: already-done pass: no ListWords/grant exists on main; control.rs tracks only the org.gnome.Shell owner (engine/src/control.rs:287-297)
- REALITY: surface-check pass: LearnStore::forget_word corelib/src/learning.rs:90, Db::forget_word corelib/src/db.rs:411, authorize engine/src/control.rs:287, companion object extension/extension.js:161
- REALITY: binding-decisions pass: the Shell-only control plane and the SQLCipher store are respected; forgetting reuses the reviewed forget path (docs/superpowers/specs/2026-11-06-quick-settings-status-and-learning-design.md:170)
- REALITY: numbers pass: about 9k stored words [measured: GetStats words_learned, docs/superpowers/specs/2026-11-06-quick-settings-status-and-learning-design.md:1]; caps 10 000 / 64 bytes / 2 grants / 120 s / 5 s are design choices [estimated]
- REALITY: deferred-asks pass: no linked issue; the brainstorming asks (search, 4 sorts, personal/all, single + multi delete, undo, confirm, ask every time) are all in scope docs/superpowers/specs/2026-11-06-learned-words-page-design.md:12
- REALITY: pr-count pass: one branch, one PR (no .gstack/ship-policy.json) docs/superpowers/specs/2026-11-06-learned-words-page-design.md:1

## Decision ledger

### R1: what a grant is bound to
Finding: 1, CRITICAL, confidence 8/10, spec "Access" step 4, plus /usr/share/dbus-1/services/org.gnome.Shell.Extensions.service (`Exec=/usr/bin/gjs -m /usr/share/gnome-shell/org.gnome.Shell.Extensions`), reviewer: plan-eng-review (Architecture)
Plan baseline: the grant is bound to the Settings window's unique bus name and ends when that name disappears ("until it closes").
Runtime evidence: every extension's prefs window runs in the one shared org.gnome.Shell.Extensions process, on one shared session connection. The grant would outlive the Words window for as long as that service lives, and any other extension's prefs code in the same process could call ListWords with it.
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| R1 grant binding | the shared prefs connection | a private bus connection opened by the Words page only (Gio.DBusConnection to the session address), used for RequestWordAccess, ListWords and ForgetWords and closed when the page or window closes, so the grant drops at once (NameOwnerChanged); plus an explicit ReleaseWordAccess on close | keep the shared connection; the grant expires after 10 minutes of no calls |
| R2, R3 | pending | pending | pending |
Question D2: binding the grant to a private connection
Header: Grant binding
Options:
A) Private connection (recommended)
The Words page opens its own D-Bus connection, and only that connection is granted. Closing the page closes the connection, so the grant ends immediately. Other extensions' Settings code in the same process can't use it. It adds 1 engine method (ReleaseWordAccess), connection setup and teardown in wordsPage.js, and 2 tests.
B) Shared connection with expiry
Less code, but the grant outlives the window by up to 10 minutes and is usable by any extension's Settings code in that process.

State: approved
Actual answer: A, "Private connection" (D2, 2026-11-06)
Accepted scope:
- wordsPage.js opens its own connection with `Gio.DBusConnection.new_for_address(Gio.dbus_address_get_for_bus_sync(SESSION), AUTHENTICATION_CLIENT | MESSAGE_BUS_CONNECTION)` and uses it for RequestWordAccess, ListWords, ForgetWords and ReleaseWordAccess.
- On page or window destroy it calls ReleaseWordAccess and then closes the connection.
- The engine adds `ReleaseWordAccess()`, which drops the caller's own grant and is callable only by a granted name.
- It also revokes a grant when that unique name's owner vanishes.
- Tests: the engine drops the grant on Release and on disconnect; prefs.smoke checks that closing calls Release and closes the connection.
History: none

### R2: who may trigger the Shell prompt
Finding: 2, INFORMATIONAL, confidence 7/10, spec "Access" step 2, reviewer: plan-eng-review (Architecture)
Plan baseline: any caller can call RequestWordAccess. The prompt says "Show your learned words in Settings?" without naming who asks.
Runtime evidence: the companion sees only a unique bus name. Any app could pop the prompt at a moment the user expects one (for example right after they opened Settings) and receive the words on Allow.
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| R2 requester check | none | allowed only if the caller's PID (org.freedesktop.DBus.GetConnectionUnixProcessID) equals the PID of the current owner of org.gnome.Shell.Extensions; otherwise `false` at once, with no prompt | none; rely on the prompt |
| R1, R3 | pending | pending | pending |
Question D3: allowing only the real Settings process to ask
Header: Who may ask
Options:
A) Same-process check (recommended)
The extension accepts a request only from the process that owns org.gnome.Shell.Extensions, which is GNOME's own extension-Settings service. A random app gets "not allowed" with no prompt to trick you with. It adds 2 D-Bus lookups and 2 smoke checks. It isn't proof against code running as you, but it removes the easy phishing path.
B) Prompt only
Simpler, but any app can show the same prompt, and an Allow then gives it your words.

State: approved
Actual answer: A, "Same-process check" (D3, 2026-11-06)
Accepted scope:
- RequestWordAccess first resolves the caller's PID and the PID of the current owner of `org.gnome.Shell.Extensions` (via `GetNameOwner` and `GetConnectionUnixProcessID` on org.freedesktop.DBus).
- If they differ, or either lookup fails, it replies `false` at once with no prompt and logs a fixed text once.
- The smoke tests check that a gdbus caller (a different PID) gets `false` with no dialog, and that a caller in the owner's process gets the prompt. The test harness owns the name from the same process.
History: none

### R3: pending deletes when the window closes
Finding: 3, INFORMATIONAL, confidence 7/10, spec "The page", Undo: "closing the window sends any pending words first", reviewer: plan-eng-review (Code Quality)
Plan baseline: the pending words are sent on close.
Runtime evidence: an async call started in close-request can be cut off when the window and connection are torn down, so the user's delete is silently lost.
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| R3 close with pending | fire-and-forget | close-request returns true (it holds the close), sends ForgetWords, and closes the window when the reply arrives or after 3 s; the toast is dismissed first | drop pending deletes on close (Undo window = until close) |
| R1, R2 | pending | pending | pending |
Question D4: holding the close until pending deletes are sent
Header: Close + pending
Options:
A) Hold close until sent (recommended)
Closing waits (at most 3 s) for the pending deletes to reach the engine, then closes. What you deleted stays deleted. It needs 1 prefs.smoke test.
B) Drop on close
Simpler, but deleting a word and quickly closing Settings silently keeps it.

State: approved
Actual answer: A, "Hold close until sent" (D4, 2026-11-06)
Accepted scope:
- If words are pending when the window is closed, close-request dismisses the toast, returns true (holding the close) and sends one ForgetWords on the private connection.
- When the reply arrives, or after 3000 ms, it calls ReleaseWordAccess, closes the connection and closes the window.
- A failure is not shown, because the window is closing; the words simply aren't forgotten.
- Test: prefs.smoke delete then close sends ForgetWords before the window is destroyed, and a stuck stub closes after 3 s.
History: none

### R4: where the access state lives (outside voice OV4; reopens part of R1)
Finding: OV4 (Medium, strategy) plus OV2 (High), reviewer: Claude Plan subagent (outside voice, native fallback). Verified: the engine watches only org.gnome.Shell (engine/src/control.rs:557-559), so engine-side revocation has no subscription.
Plan baseline: the engine holds a grant table (GrantWordAccess, ReleaseWordAccess, revoke on disconnect), and Settings calls ListWords/ForgetWords on the engine directly (R1 = A: private connection).
Comparison grid:
| Choice | Current | A | B | C | D |
|---|---|---|---|---|---|
| R4 access architecture | engine grant table | Shell-mediated: the companion object gains ListWords/ForgetWords; it keeps the allowed set of caller unique names (the Words page's private connection, R1 kept) and drops a name via Gio.bus_watch_name when it vanishes; the engine only gains Shell-only ListWords/ForgetWords; no grant table, GrantWordAccess or ReleaseWordAccess | keep the engine grant table and add a per-grant NameOwnerChanged subscription | investigate | defer |
Question D5: Shell-mediated access
Header: Access design
Options:
A) Apply: Shell-mediated
B) Keep the engine grant table
C) Investigate before choosing
D) Defer this proposed change only

State: approved
Actual answer: A, "Apply: Shell-mediated" (D5, 2026-11-06)
Accepted scope:
- **Engine:** gains only ListWords and ForgetWords, both Shell-only. There is no GrantWordAccess, ReleaseWordAccess or grant table; authorize changes only by the R9 rule.
- **Companion object:** gains RequestWordAccess (async, with the prompt and the R2/R5 checks), ListWords and ForgetWords. ListWords and ForgetWords answer only for a caller in its allowed set and pass the call through to the engine.
- **The allowed set:** at most 2 unique names, oldest replaced. A name is dropped when it vanishes (Gio.bus_watch_name_on_connection) and on disable().
- **The Words page** keeps its private connection (R1), so closing it drops the name and ends access.
- **Tests:** smoke checks that the companion's ListWords and ForgetWords are denied without Allow and allowed after it, and that closing the caller's connection revokes access.
History: R1 approved a private connection bound to an engine grant. R4 moves the grant into the Shell and keeps the private connection; R1's ReleaseWordAccess and engine revocation are superseded.

### R5: the requester check can be beaten (outside voice OV1; reopens R2)
Finding: OV1 (High). While no Settings window is open, any app can own the D-Bus-activated name org.gnome.Shell.Extensions and pass the same-PID check. The spec's own smoke plan does exactly that (spec R2 accepted scope).
Plan baseline: R2 = A, same-PID check against the owner of org.gnome.Shell.Extensions.
Comparison grid:
| Choice | Current | A | B | C | D |
|---|---|---|---|---|---|
| R5 requester check | same-PID only | same-PID AND the focused window (global.display.focus_window) belongs to the caller's PID with GTK app id org.gnome.Shell.Extensions; drop the "other extensions can't use it" claim | same-PID only | investigate | defer |
Question D6: also requiring the asking window to be focused
Header: Focus check
Options:
A) Apply: add the focus check
B) Keep same-PID only
C) Investigate before choosing
D) Defer this proposed change only

State: approved
Actual answer: A, "Apply: add the focus check" (D6, 2026-11-06)
Accepted scope:
- RequestWordAccess requires all three of:
  - the caller's PID equals the PID of the owner of org.gnome.Shell.Extensions (R2);
  - `global.display.focus_window?.get_pid()` equals the caller's PID;
  - `focus_window.get_gtk_application_id() === 'org.gnome.Shell.Extensions'`.
- Otherwise it replies `false` at once, with no prompt.
- The R1 claim that other extensions' Settings code can't use the grant is dropped. Code in the same process is out of scope.
- Smoke: a caller whose window is not focused gets `false` with no dialog. The fake focused-window condition is set through Eval.
History: R2 approved the same-PID check, which is kept and extended.

### R6: the switch has nothing to filter on (outside voice OV3)
Finding: OV3 (High). ListWords returns a(sut) with no personal flag, but the page filters personal words on the client.
Comparison grid:
| Choice | Current | A | B | C | D |
|---|---|---|---|---|---|
| R6 ListWords shape | a(sut), include_all argument | `ListWords() -> a(sutb)` (word, count, last_seen, personal), always every stored word; the switch filters on the client | refetch with include_all on each switch flip | investigate | defer |
Question D7: returning a personal flag
Header: List shape
Options:
A) Apply: personal flag
B) Refetch on flip
C) Investigate before choosing
D) Defer this proposed change only

State: approved
Actual answer: A, "Apply: personal flag" (D7, 2026-11-06)
Accepted scope:
- Engine `ListWords() -> a(sutb)` returns (word, count, last_seen, personal) for every stored word with count ≥ 2. `personal` means the word is in no loaded dictionary. There is no argument and no server-side sort (see R9).
- The companion passes it through unchanged.
- The page filters by `personal` on the client.
- Tests: the corelib list_words personal flag; prefs.smoke checks that the switch filters without a second ListWords call.
History: none

### R7: a batch forget scans per word (outside voice OV5)
Finding: OV5 (Medium). forget_words loops forget_word (corelib/src/learning.rs:91-101), each a full scan of bigrams, blacklist and the dirty sets, under the store lock the typing path uses. Up to 10 000 words.
Comparison grid:
| Choice | Current | A | B | C | D |
|---|---|---|---|---|---|
| R7 batch forget | loop forget_word | one HashSet of lowercased words, then one retain pass per table; the DB uses a temporary table or chunked `IN (...)` deletes in one transaction | loop | investigate | defer |
Question D8: a single-pass batch forget
Header: Batch forget
Options:
A) Apply: single pass
B) Keep the loop
C) Investigate before choosing
D) Defer this proposed change only

State: approved
Actual answer: A, "Apply: single pass" (D8, 2026-11-06)
Accepted scope:
- **corelib:** `LearnStore::forget_words(&mut self, ws: &[String]) -> u32` lowercases the words into a HashSet, then makes one retain pass each over bigrams, blacklist, dirty_bigrams, dirty_blacklist and recent, plus vocab and dirty_vocab removes. `forget_word(w)` becomes `forget_words(&[w.into()]) == 1`, so there is one path.
- **`Db::forget_words`** runs in one transaction, with chunked `DELETE ... WHERE word IN (...)` / `w1 IN (...) OR w2 IN (...)` / `orig IN (...) OR cand IN (...)` at ≤ 500 words per chunk.
- **The actor's `Req::Forget`** takes a Vec. The existing single-word ForgetWord uses it with one word.
- **Tests:** the existing forget tests stay green. A 1 000-word batch test checks that every row is gone and the unrelated rows are kept, plus a reopen test.
History: none

### R8: holes in close and toast (outside voice OV6; amends R3)
Finding: OV6 (Medium).
- close-request fires again on window.close(), so it needs a guard.
- Dismissing the toast on close would double-send unless both paths go through PendingForget.take().
- Adw.ToastOverlay queues toasts, so 5 quick deletes wait 25 s or more.
Comparison grid:
| Choice | Current | A | B | C | D |
|---|---|---|---|---|---|
| R8 toast/close mechanics | one toast per delete; unguarded close | one toast and one pending batch whose 5 s timer restarts on each delete ("Forgot N words"); every send goes through PendingForget.take(); a closing flag guards close-request | as is | investigate | defer |
Question D9: one batched toast plus a close guard
Header: Toast + close
Options:
A) Apply: one batched toast and a guard
B) Keep per-delete toasts
C) Investigate before choosing
D) Defer this proposed change only

State: approved
Actual answer: A, "Apply: batched toast + guard" (D9, 2026-11-06)
Accepted scope:
- **The batch:** one pending batch and one toast. Each delete adds to the batch, updates the toast title to "Forgot N words" ("Forgot kadto" when N = 1), and restarts a 5000 ms GLib timeout. The toast itself has no timeout and is dismissed by code.
- **Undo** restores the whole batch, clears it and dismisses the toast.
- **Sending:** when the timeout fires or the window closes, `PendingForget.take()` returns the batch and clears it, so a word is sent at most once. The batch goes out in one ForgetWords call.
- **The close guard:** close-request checks a `closing` flag. When the flag is already set it returns false, letting the close go ahead. Otherwise it sets the flag and runs R3.
- **Tests:**
  - lib.test checks PendingForget's take-once behavior;
  - prefs.smoke checks that 3 quick deletes give one toast "Forgot 3 words" and one ForgetWords call after 5 s;
  - Undo restores all 3;
  - close during the batch sends once and closes once.
History: R3 approved holding the close until pending deletes are sent. That is kept, with the guard added.

### R9: small gaps (outside voice OV7)
Finding: OV7 (Low):
- a late Allow can grant a caller that has already disconnected;
- allow_any_caller silently opens the word methods to everyone;
- the checklist's word-count line holds only with the switch on;
- a redundant server-side sort.
Comparison grid:
| Choice | Current | A | B | C | D |
|---|---|---|---|---|---|
| R9 small gaps | as written | check the caller still has its name before allowing; exclude ListWords/ForgetWords from allow_any_caller (Shell only, always); fix the checklist line ("with Show all on"); drop the server sort | as is | investigate | defer |
Question D10: the small gaps
Header: Small gaps
Options:
A) Apply all four
B) Keep as written
C) Investigate before choosing
D) Defer this proposed change only

State: approved
Actual answer: A, "Apply all four" (D10, 2026-11-06)
Accepted scope:
1. Before adding a name to the allowed set after Allow, the companion checks that the name still has an owner (GetNameOwner). If it doesn't, it replies `false`.
2. Engine ListWords and ForgetWords are Shell-only even with `allow_any_caller`. This is a new rule in `authorize`, with a test row.
3. The desktop checklist says "with Show all on, the count matches the menu's words learned".
4. There is no server-side sort.
History: none
History: none

Approval readiness: PASS. Checked R1 (D2 = A, partly superseded by R4), R2 (D3 = A, extended by R5), R3 (D4 = A, amended by R8), R4 (D5 = A), R5 (D6 = A), R6 (D7 = A), R7 (D8 = A), R8 (D9 = A) and R9 (D10 = A), all answered on 2026-11-06. Scope: D1 = A.

## Review sections

### 1. Architecture

- [CRITICAL] (confidence: 8/10) A grant bound to the shared prefs connection outlives the window (org.gnome.Shell.Extensions.service). → R1 (private connection), approved.
- [INFORMATIONAL] (confidence: 7/10) Any app could trigger the prompt. → R2 (same-PID check), approved, then extended by R5.

**Design twice** (new surface: the companion's access methods):
- **Shape A:** an engine grant table.
- **Shape B (chosen via R4):** the Shell holds the allowed set and passes calls through.

B hides more behind less: the engine keeps a single trust rule (Shell only), and the Shell already owns caller identity, focus and the prompts.

**Red flags:**
- 2 (information leakage): the "who may read words" policy is now in one place, the Shell.
- 5 (split ownership): removed by R4.
- No others.

### 2. Code quality

- [INFORMATIONAL] (confidence: 7/10) The close-request hand-off could drop deletes. → R3, approved, then amended by R8.

### 3. Tests

```
CODE PATHS                                          USER FLOWS
[+] corelib list_words / forget_words / db          [+] Show learned words
  ├── [GAP→planned] personal flag, count ≥ 2          ├── [GAP→planned] [→E2E] Allow → list (smoke)
  └── [GAP→planned] batch single pass, chunks, reopen ├── [GAP→planned] [→E2E] Cancel / Return → hidden
[+] persist Forget(Vec)                               └── [GAP→planned] [→E2E] other PID / unfocused → no prompt
  └── [GAP→planned] locked refuse, restart          [+] Browse
[+] control ListWords/ForgetWords                     ├── [GAP→planned] search, 4 sorts, switch (prefs.smoke)
  └── [GAP→planned] Shell-only incl. allow_any_caller [+] Forget
[+] companion RequestWordAccess / pass-through        ├── [GAP→planned] batched toast, Undo
  ├── [GAP→planned] PID + focus + still-connected     ├── [GAP→planned] multi confirm
  └── [GAP→planned] allowed set, vanish revoke        └── [GAP→planned] close with pending (≤ 3 s)
[+] existing ForgetWord / ConfirmForget
  └── [★★★ TESTED] extension-smoke.sh, persist forget tests (kept green; R8 routes ForgetWord through forget_words)

COVERAGE today: 1/18 paths (existing forget path); every gap has a planned test
QUALITY: ★★★:1 | GAPS: 17 (5 E2E via extension-smoke.sh; no LLM code)
```

- **Regression risk:** R8 changes the existing single-word forget path into a one-word batch. The exact approved contract from the Quick Settings work (memory cleared only after the DB commit, refused while locked, blacklist rows removed) stays, and the existing persist/corelib/smoke forget tests must stay green. That is covered by R8's accepted scope, so no new question was needed.
- **Test Plan Artifact:** (kept outside the repo)

### 4. Performance

- [INFORMATIONAL] (confidence: 7/10) The per-word forget scans under the store lock. → R7 (single pass), approved.
- ListWords over about 9k entries with a dictionary lookup each runs under the store lock once per load, for an estimated few ms. The GTK list is virtualized. No issue.

### Outside voice

A native Claude Plan subagent (Codex is not installed) completed within the 5-minute bound. Its 7 findings were resolved:
- OV1 → R5;
- OV2 + OV4 → R4;
- OV3 → R6;
- OV5 → R7;
- OV6 → R8;
- OV7 → R9.

There are no remaining disagreements.

## NOT in scope

- **"First learned" date:** the user chose "last used".
- **Remembering the Allow:** the user chose "ask every time".
- **Isolation from other extensions' Settings code in the same process:** same-process code is out of reach (R5).
- **Live list updates:** a Refresh button instead.
- **Exporting or editing words, or listing pairs and blacklist entries:** not asked for.

## What already exists

- The forget path in memory and on disk (learning.rs forget_word, db.rs forget_word, persist Req::Forget) is generalized to batches by R7.
- The companion object and `_confirmForget` dialog pattern (extension.js) gain RequestWordAccess.
- The Settings page, toast and stub injection patterns (prefsWindow.js, prefs.smoke.js) are reused.
- The headless Shell smoke with Eval and the virtual keyboard (extension-smoke.sh) is extended.

## Failure modes

| New path | Realistic failure | Test / handling | User sees |
|---|---|---|---|
| RequestWordAccess | wrong PID or not focused | the R5 check gives `false`, smoke-tested | "Not allowed" |
| Allow | the caller closed meanwhile | the R9 still-connected check | (nothing; window gone) |
| ListWords | engine down | an error through the companion, prefs.smoke-tested | "Couldn't load…" + Retry |
| ForgetWords | keyring locked | the persist refusal, tested | rows back + "Unlock your keyring first" |
| Close with pending | engine slow | 3 s cap (R3), prefs.smoke-tested | the window closes; the delete may not apply (rare) |
| Big batch | lock contention | single pass (R7), 1 000-word test | none |

There are no critical gaps.

## Worktree parallelization

| Step | Modules touched | Depends on |
|------|----------------|------------|
| A. corelib list/forget_words + db | corelib/ | — |
| B. persist + control methods | engine/ | A |
| C. lib.js pure logic + tests | extension/ (lib) | — |
| D. companion access + smoke | extension/, scripts/ | B (method shapes; can start on fake_correctiond) |
| E. wordsPage.js + prefs + smoke | extension/ | C, D (companion interface) |

- **Lanes:**
  - Lane 1: A → B, sequentially (engine depends on corelib).
  - Lane 2: C, in parallel.
  - D and E share extension.js/prefs, so they run sequentially after C.
- **Order:** launch A and C, then B, then D, then E.
- **Conflict flags:** extension/ is shared by C, D and E, so pin the companion interface in fake_correctiond.py first.

## Implementation Tasks
Synthesized from this review's findings. Check each box as it ships.

- [ ] **T1 (P1, human: ~3h / CC: ~15min)**: wordsPage: private D-Bus connection; close ends access
  - Surfaced by: Architecture, R1
  - Files: extension/wordsPage.js
  - Verify: `gjs -m extension/test/prefs.smoke.js`
- [ ] **T2 (P1, human: ~4h / CC: ~20min)**: companion: RequestWordAccess with the PID, focus and still-connected checks, an allowed set with name watches, and pass-through ListWords/ForgetWords
  - Surfaced by: R2, R4, R5, R9
  - Files: extension/extension.js, scripts/fake_correctiond.py, scripts/extension-smoke.sh
  - Verify: `bash scripts/extension-smoke.sh`
- [ ] **T3 (P1, human: ~2h / CC: ~10min)**: engine: Shell-only ListWords a(sutb) and ForgetWords, ignoring allow_any_caller
  - Surfaced by: R6, R9
  - Files: engine/src/control.rs
  - Verify: `cargo test -p engine`
- [ ] **T4 (P1, human: ~2h / CC: ~10min)**: corelib/persist: single-pass batch forget; ForgetWord routed through it
  - Surfaced by: Performance, R7
  - Files: corelib/src/learning.rs, corelib/src/db.rs, engine/src/persist.rs
  - Verify: `cargo test`
- [ ] **T5 (P1, human: ~2h / CC: ~10min)**: wordsPage: one batched Undo toast, take-once PendingForget, close guard, hold close ≤ 3 s
  - Surfaced by: Code Quality, R3, R8
  - Files: extension/wordsPage.js, extension/lib.js, extension/test/lib.test.js, extension/test/prefs.smoke.js
  - Verify: gjs lib.test plus prefs.smoke

## Unresolved decisions

None.

## Completion summary

- Step 0: Scope Challenge: scope accepted as-is.
- Reality rows: 7/7 applicable rows emitted, 0 unverified premises.
- Architecture Review: 2 issues found (R1, R2).
- Code Quality Review: 1 issue found (R3).
- Test Review: diagram produced, 17 gaps identified, all with planned tests.
- Performance Review: 0 new issues (R7 came from the outside voice).
- NOT in scope: written.
- What already exists: written.
- TODOS.md updates: 0 items proposed.
- Failure modes: 0 critical gaps flagged.
- Unresolved decisions: 0 in this review.
- Outside voice: codex unavailable (not installed); the native Plan fallback completed with 7 findings, all resolved (R4-R9).
- Parallelization: 2 lanes then sequential: 2 parallel / 3 sequential.
- Lake Score: 6/8 = answers picking a 10/10 option / answers scored for Completeness (D3 and D6 picked 9/10, the highest offered).

## Amendments after the final review (2026-11-06)

- **C1 (user decision):** the third access check is the focused window's `get_wm_class() === 'org.gnome.Shell.Extensions'`. It replaces the GTK app id check, which could never pass for GNOME's real prefs window.
- **C2:** the page never holds or destroys the prefs window. On close it sends the pending batch, flushes and closes its connection, and lets the close go ahead. This replaces R3's held close and keeps R3's goal.
- **M1:** the prompt closes with `false` after 115 s, or when the asking connection vanishes.
- **M2:** an access loss returns the page to the hidden state.
- **M3:** an older engine shows "Update correctiond to use this".
- **M5:** disable() nulls the word-access state.

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` | Scope & strategy | 0 | — | — |
| Outside Review | native Plan fallback (`/plan-eng-review` outside voice; codex not installed) | Independent 2nd opinion | 1 | completed (in-host fallback; no external coverage) | 7 findings, all resolved (R4-R9) |
| Eng Review | `/plan-eng-review` | Architecture & tests (required) | 1 | issues_open | 3 issues (+7 outside voice), 0 critical gaps, all resolved into approved changes |
| Design Review | `/plan-design-review` | UI/UX gaps | 0 | — | — |
| DX Review | `/plan-devex-review` | Developer experience gaps | 0 | — | — |

- **OUTSIDE COVERAGE:** codex, plan-review phase: unavailable (not installed). The native Claude Plan-subagent fallback completed within 5 minutes, with 7 findings. A native result is not external coverage.
- **VERDICT:** no review is CLEAR. The Eng Review issues all became approved plan changes (R1-R9), so the status stays issues_open: the work is mapped, not failing. The plan is ready to implement with those amendments. eng review required
NO UNRESOLVED DECISIONS
