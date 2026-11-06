# Learned Words Page Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a "Words" page in the extension's Settings window to search, sort (A→Z, Z→A, newest, oldest by last use), filter (personal / all) and forget learned words, one or many, with Undo. Access is approved per window through a GNOME Shell prompt.

**Architecture:**
- **Engine:** two new Shell-only D-Bus methods, `ListWords` and `ForgetWords`, plus a single-pass batch forget in corelib and the persistence actor.
- **GNOME Shell companion object** (extension.js): gains `RequestWordAccess`, which runs the process/focus checks and the prompt. It keeps the allowed set of caller connections and passes `ListWords`/`ForgetWords` through to the engine for allowed callers.
- **Words page:** talks only to the companion, on its own private D-Bus connection.

**Tech Stack:** Rust (zbus 5, tokio, rusqlite/SQLCipher), GJS (GNOME Shell 50, GTK 4, libadwaita), Python 3 (fake engine), Bash smoke harnesses.

**Spec:** `docs/superpowers/specs/2026-11-06-learned-words-page-design.md`. The Access, Code, Error handling and Testing sections are the amended, binding design. The Decision ledger R1-R9 records why.

## Global Constraints

- **Engine `ListWords() -> a(sutb)`:**
  - returns (word, count, last_seen, personal) for every stored word with count ≥ 2;
  - `personal` means the word is in no loaded dictionary;
  - unsorted, and no argument;
  - Shell-only, **even with `allow_any_caller`**.
- **Engine `ForgetWords(as) -> u`:**
  - takes 1-10 000 words, each 1-64 bytes; otherwise `ForgetFailed` "could not forget";
  - one persistence request and one DB transaction;
  - returns how many words existed;
  - while locked: `ForgetFailed` "unlock your keyring first";
  - Shell-only, even with `allow_any_caller`.
- **Forgetting a word** removes the vocab entry, every bigram with it on either side, every blacklist entry with it on either side, the dirty entries and the recent entry. That is the same contract as today's `forget_word`.
- **Companion interface** `org.nadelacruz.CorrectiondCompanion1` at `/org/nadelacruz/CorrectiondCompanion`:
  - existing `ConfirmForget()`;
  - new `RequestWordAccess() -> b` (async), `ListWords() -> a(sutb)` and `ForgetWords(as) -> u`.
- **RequestWordAccess checks.** It prompts only if all three hold:
  - the caller PID equals the PID of the owner of `org.gnome.Shell.Extensions`;
  - `global.display.focus_window?.get_pid()` equals the caller PID;
  - `focus_window.get_gtk_application_id() === 'org.gnome.Shell.Extensions'`.

  Otherwise it replies `false` at once, with no prompt. A request while a prompt is open also gets `false`.
- **Prompt text:** "Show your learned words in Settings? Settings can list and forget your learned words until you close it."
  - Buttons: **Cancel** (default, key Escape) and **Allow**.
  - After Allow, the caller must still have a bus owner, or the reply is `false`.
- **Allowed set:** at most 2 unique names, oldest replaced. Each is watched with `Gio.bus_watch_name_on_connection` and dropped when it vanishes. It is cleared on `disable()`, and an open prompt closes with `false` on `disable()`. Not allowed means a D-Bus `org.freedesktop.DBus.Error.AccessDenied`.
- **Words page connection:** one private connection, `Gio.DBusConnection.new_for_address` on `Gio.dbus_address_get_for_bus_sync(Gio.BusType.SESSION, null)` with `AUTHENTICATION_CLIENT | MESSAGE_BUS_CONNECTION`. It is closed when the page or window is destroyed.
  - RequestWordAccess has a 120 000 ms timeout; the other calls have 2000 ms.
- **Page texts:**
  - "Learned words";
  - "Your learned words are hidden until you allow it.";
  - button "Show learned words…";
  - "Not allowed";
  - "Turn on the Autocorrect extension to use this";
  - "Couldn't load your learned words" with button "Retry";
  - switch "Show all stored words";
  - "N words";
  - row subtitle "used C times · last used X";
  - action bar "N selected", "Clear", "Forget selected";
  - confirm "Forget N words?" with Cancel (default) and Forget (destructive);
  - toasts "Forgot kadto" / "Forgot N words" with button "Undo", "Unlock your keyring first", "Could not forget".
- **Sorts:**
  - "A to Z" and "Z to A" compare `toLowerCase()`;
  - "Newest first" and "Oldest first" compare `last_seen`;
  - ties break by A→Z word.
- **Search:** case-insensitive substring.
- **`lastUsedText`:** "today", "yesterday", "D Mon" this year, "D Mon YYYY" older, "never" for 0.
- **Undo:**
  - one pending batch and one toast; its 5000 ms timer restarts on each delete;
  - Undo restores the whole batch;
  - `PendingForget.take()` empties it, so a word is sent once.
- **Closing with pending words:** a `closing` flag guards `close-request`, which holds the close, sends one ForgetWords, and closes on the reply or after 3000 ms.
- **Logs:** no log line contains a word; logs use fixed texts.

## Review Focus

1. **Search text with regex or special characters** (`.`, `*`, `(`) matches literally, not as a pattern. Task 3 pins this.
2. **Words with uppercase or non-ASCII letters** sort and search case-insensitively without throwing. Task 3 pins this.
3. **Selecting a word, then changing the search so it is hidden, then "Forget selected"** forgets exactly the selected words, including hidden ones, and the confirm count says so. Task 5 pins this.
4. **The engine restarts while the page is open.** The companion's pass-through then fails and the page shows "Couldn't load your learned words" + Retry, with no crash and no stale grant. Task 4 and Task 5 pin this.
5. **An empty store, or no personal words.** The list shows "0 words" and no error. Task 5 pins this.

---

### Task 1: Batch forget and list_words (corelib)

**Files:**
- Modify: `corelib/src/learning.rs` (`forget_word` :91-101, plus a new `list_words`)
- Modify: `corelib/src/db.rs` (`forget_word` :411)
- Test: the `#[cfg(test)]` modules in those files

**Interfaces:**
- Produces:
  - `LearnStore::forget_words(&mut self, ws: &[String]) -> u32`. It lowercases the words into a `HashSet<String>`, removes them from vocab and dirty_vocab, makes one `retain` pass over each of bigrams, blacklist, dirty_bigrams, dirty_blacklist and recent, and returns how many vocab entries existed.
  - `LearnStore::forget_word(&mut self, w: &str) -> bool` becomes `self.forget_words(&[w.to_owned()]) == 1`.
  - `LearnStore::list_words(&self, is_known: &dyn Fn(&str) -> bool) -> Vec<(String, u64, i64, bool)>` returns (word, count, last_seen, personal) for vocab entries with count ≥ `PERSIST_MIN` (2), where `personal = !is_known(word)`, in no particular order.
  - `Db::forget_words(&mut self, ws: &[String]) -> Result<(), StoreError>` uses one transaction with chunks of at most 500 words: `DELETE FROM vocab WHERE word IN (...)`, `DELETE FROM bigram WHERE w1 IN (...) OR w2 IN (...)`, `DELETE FROM blacklist WHERE orig IN (...) OR cand IN (...)`. The words must already be lowercased (doc comment). It chmods `-wal`/`-shm` like `write`.
  - `Db::forget_word(w)` becomes `self.forget_words(&[w.to_owned()])`.

- [ ] **Step 1: Write failing tests**
  - `forget_words_batch_removes_all_sides`:
    - record kadto×2, sige×2 and ako×2;
    - bigrams (ako,kadto), (sige,na), (na,ako);
    - blacklist (kadto,kadto2) and (teh,sige);
    - `forget_words(&["KADTO".into(), "sige".into(), "missing".into()])` returns 2;
    - no row mentions kadto or sige, (na,ako) and ako remain, and `take_dirty` has no row with kadto or sige.
  - `forget_words_1000`: 1 000 learned words plus 10 others; forget the 1 000 and exactly the 10 remain.
  - `list_words_count_and_personal`:
    - "kadto" count 2, "the" count 5, "x1" count 1;
    - with `is_known = |w| w == "the"`, the result is exactly {("kadto",2,_,true), ("the",5,_,false)};
    - imported words are included.
  - db `forget_words_chunks_and_reopen`:
    - write 1 200 vocab rows, a bigram with a forgotten word as w1 and one as w2, and a blacklist row;
    - `forget_words` on 1 100 of them;
    - reopen, and exactly the expected rows remain.
  - The existing forget tests stay unchanged and must keep passing.
- [ ] **Step 2:** `cargo test -p corelib` → the new tests FAIL (missing methods).
- [ ] **Step 3:** Implement. `forget_word` on both types delegates to the batch.
- [ ] **Step 4:** `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test` → PASS.
- [ ] **Step 5:** Commit `feat(learning): single-pass batch forget and list_words`.

### Task 2: Engine ListWords and ForgetWords

**Files:**
- Modify: `engine/src/persist.rs` (`Req::Forget(String, ForgetReply)` :95, `forget_word` :166, `forget_unattached` :260, the three request-serving sites :406, :489, :611, and `forget` :709)
- Modify: `engine/src/control.rs` (`Method` near :239, `authorize` :287, the interface near :509, the wire tests)
- Test: the tests modules in those files

**Interfaces:**
- Consumes: `LearnStore::{forget_words, list_words}` and `Db::forget_words` (Task 1).
- Produces:
  - **persist:**
    - `Req::Forget(Vec<String>, oneshot::Sender<Result<u32, String>>)`;
    - `PersistHandle::forget_words(&self, words: Vec<String>) -> Result<u32, String>`, which lowercases once;
    - `PersistHandle::forget_word(w) -> Result<bool, String>` becomes `forget_words(vec![w]).map(|n| n == 1)`;
    - the rules are unchanged: the DB commit comes before the memory clear, `FORGET_LOCKED` when a store file exists but isn't attached, and memory-only without a store file.
  - **control:**
    - `Method::ListWords` and `Method::ForgetWords`. Both are Shell-only, and `authorize` ignores `allow_any_caller` for them. Add a `fn word_reading(self) -> bool`, or equivalent, used by `authorize`.
    - D-Bus `list_words()` replies `a(sutb)` (u32 count, saturated; u64 last_seen) and takes the store lock once and uses `shared.dict.contains` as `is_known`.
    - D-Bus `forget_words(words: Vec<String>) -> u32`:
      - 0 words or more than 10 000, or any word empty or over 64 bytes (`MAX_FORGET_BYTES`), gives `ControlError::ForgetFailed(FORGET_FAILED)`;
      - errors map through `forget_error_text`;
      - with no persist actor, it is memory-only.

- [ ] **Step 1: Write failing tests**
  - **persist:**
    - `forget_words_batch_survives_restart` (follow `forget_when_persistent_deletes_and_survives_restart`);
    - `forget_words_refused_while_locked`;
    - `forget_words_memory_only_without_store_file`;
    - the existing forget tests stay green.
  - **control `authorize` rows:**
    - ListWords and ForgetWords are denied for a non-Shell sender;
    - allowed for the Shell owner;
    - **denied with `allow_any_caller = true` for a non-Shell sender**;
    - the other Shell-only methods are still allowed with `allow_any_caller`.
  - **wire tests** (`p2p_control`):
    - ListWords returns `[("kadto",2,_,true)]` for a store with kadto×2, plus a dictionary word with `personal=false`;
    - ForgetWords(["kadto","nope"]) returns 1, then ListWords is empty of kadto;
    - ForgetWords([]) and ForgetWords with a 65-byte word return the ForgetFailed error, and so does ForgetWords with 10 001 words;
    - GetStats and GetStatus still never carry a word.
- [ ] **Step 2:** `cargo test -p engine` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `bash scripts/e2e.sh` and `bash scripts/headless-smoke.sh` → PASS.
- [ ] **Step 5:** Commit `feat(control): Shell-only ListWords and ForgetWords`.

### Task 3: Pure page logic (lib.js)

**Files:**
- Modify: `extension/lib.js`
- Test: `extension/test/lib.test.js`

**Interfaces:**
- Produces:
  - `export const SORTS = ['az', 'za', 'newest', 'oldest']`, with labels for the dropdown: `export const SORT_LABELS = {az: 'A to Z', za: 'Z to A', newest: 'Newest first', oldest: 'Oldest first'}`.
  - `export function compareWords(a, b, sort)`: `a` and `b` are `{word, count, lastSeen, personal}`, and the result is a negative, zero or positive number.
  - `export function matchesSearch(word, query)`: a literal, case-insensitive substring match. An empty query matches everything.
  - `export function lastUsedText(lastSeen, nowMs)`, where `lastSeen` is unix seconds.
  - `export class PendingForget` with `add(words) -> number` (the batch size after adding; duplicates ignored), `undo() -> string[]` (returns and clears), `take() -> string[]` (returns and clears; a second call returns `[]`) and `get size`.

- [ ] **Step 1: Write failing tests**
  - **Sorts:**
    - az: `["b","A","a2"]` gives `["A","a2","b"]`;
    - za gives the reverse;
    - newest and oldest use lastSeen, with an equal lastSeen ordered by word A→Z;
    - non-ASCII and uppercase words sort without throwing (Review Focus 2).
  - **`matchesSearch`:**
    - ("Kadto","ad") is true;
    - ("kadto","AD") is true;
    - ("a.b","a.b") is true and ("axb","a.b") is false, because the match is literal (Review Focus 1);
    - ("x","(") is false without throwing;
    - ("x","") is true.
  - **`lastUsedText`,** with now = 2026-11-06T12:00 local:
    - the same day gives "today";
    - the day before gives "yesterday";
    - 2026-03-04 gives "4 Mar";
    - 2025-12-31 gives "31 Dec 2025";
    - 0 gives "never".
  - **`PendingForget`:**
    - add(["a","b"]) gives 2, add(["b","c"]) gives 3;
    - take() gives ["a","b","c"], and take() again gives [];
    - undo after add gives the words, and size is 0.
- [ ] **Step 2:** `gjs -m extension/test/lib.test.js` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `gjs -m extension/test/lib.test.js && gjs -m extension/test/config.test.js` → PASS.
- [ ] **Step 5:** Commit `feat(extension): sort, search, last-used and pending-forget logic`.

### Task 4: Companion access in the Shell

**Files:**
- Modify: `extension/extension.js` (`COMPANION_IFACE` :65, the companion `wrapJSObject` :166, `disable()`, the engine proxy XML, a new prompt built like `_confirmForget` :645)
- Modify: `scripts/fake_correctiond.py` (add ListWords, returning `[("kadto",41,<now>,True),("sige",12,<now-86400>,True),("the",500,<now>,False)]`, and ForgetWords, returning the count of the given words it knows; both logged)
- Create: `scripts/smoke_words_client.js`, a test-only gjs `Gtk.Application`. It is installed nowhere and used only by the smoke:
  - `application_id = 'org.gnome.Shell.Extensions'` makes it own that bus name;
  - it presents a window;
  - it opens a private connection and calls `RequestWordAccess`, then `ListWords` and `ForgetWords(['kadto'])`;
  - it prints one result line per call, then a "closed" marker after closing the private connection, and stays alive until it is killed.
- Modify: `scripts/extension-smoke.sh`

**Interfaces:**
- Consumes: the engine's ListWords and ForgetWords (Task 2) and their signatures.
- Produces the companion methods named in the Global Constraints, which Task 5 calls:
  - `RequestWordAccessAsync(params, invocation)`, the GJS async-method form, replying `(b)`;
  - `ListWordsAsync` and `ForgetWordsAsync`, which pass through to the engine proxy with a 2000 ms timeout. An engine error is returned by name and message; a caller not in the allowed set gets `AccessDenied`.

- [ ] **Step 1: Write the failing smoke checks** (marks and Eval as in the existing scenario):
  - a `gdbus call` RequestWordAccess (different PID, no window) returns `(false,)`, and no dialog opens (Eval reports no `_wordsDialog`);
  - companion ListWords from gdbus gives AccessDenied;
  - with `smoke_words_client.js` running, its window focused, and the Allow button clicked via Eval: RequestWordAccess gives true, and ListWords returns the 3 rows;
  - ForgetWords(['kadto']) gives 1, and the fake logs `ForgetWords (['kadto'],)`;
  - closing the client's private connection gives AccessDenied on a new call from the same process's next connection, and the name is dropped from the allowed set (Eval shows size 0);
  - the client again, with Cancel clicked, gives false; with a Return key press, false;
  - a second request while the prompt is open gives false;
  - disable during the prompt gives false;
  - engine restart: the allowed client's ListWords fails with an error and there is no stack trace (Review Focus 4);
  - no stack trace through the extension overall.
- [ ] **Step 2:** `bash scripts/extension-smoke.sh` → the new checks FAIL.
- [ ] **Step 3:** Implement extension.js and the fake. Resolve PIDs with `org.freedesktop.DBus.GetConnectionUnixProcessID` and `GetNameOwner` on `Gio.DBus.session`.
- [ ] **Step 4:** Run `bash scripts/extension-smoke.sh`, `gjs -m extension/test/lib.test.js`, and `bash scripts/pack-extension.sh` (smoke_words_client.js not packed) → PASS.
- [ ] **Step 5:** Commit `feat(extension): Shell-approved access to learned words`.

### Task 5: The Words page

**Files:**
- Create: `extension/wordsPage.js`
- Modify: `extension/prefsWindow.js` (add the page in `fillWindow` :152, after the Learning page :570)
- Modify: `scripts/install-extension.sh` and `scripts/pack-extension.sh` (add `wordsPage.js` to their file lists)
- Test: `extension/test/prefs.smoke.js`

**Interfaces:**
- Consumes: `SORTS`, `SORT_LABELS`, `compareWords`, `matchesSearch`, `lastUsedText` and `PendingForget` (Task 3), and the companion methods (Task 4).
- Produces: `export function addWordsPage(window, opts)`, called from `fillWindow`.
  - It returns a controller: `{showButton, statusLabel, searchEntry, sortRow, allSwitch, countLabel, listModel, selected (Set), forgetSelected(), clearSelection(), toast, requestAccess(), reload()}`.
  - `opts.wordsCalls = {open() -> conn, requestAccess(conn) -> Promise<bool>, listWords(conn) -> Promise<rows>, forgetWords(conn, words) -> Promise<number>, close(conn)}` is injectable. The default uses the private connection from the Global Constraints.
- **Refresh:** a Refresh button (icon `view-refresh-symbolic`) beside the sort dropdown calls `reload()`, which calls listWords again on the same connection.
- **Toasts:** the page uses the window's toast overlay (`window.add_toast`), keeps one `Adw.Toast` with `timeout: 0` for the batch, and dismisses it in code.
- **Close:** `close-request` on the window follows the Global Constraints rule.

- [ ] **Step 1: Write failing prefs.smoke tests** (stub `wordsCalls`):
  - **States:**
    - initially the status is hidden text, and there is no list;
    - requestAccess false gives "Not allowed";
    - requestAccess throwing gives "Turn on the Autocorrect extension to use this";
    - listWords throwing gives "Couldn't load your learned words" and a Retry that calls requestAccess again;
    - an empty list gives "0 words" (Review Focus 5).
  - **Loaded with the 3 stub rows:**
    - the default shows 2 personal rows, "2 words";
    - the switch on gives 3, with no second listWords call;
    - search "AD" gives 1 row;
    - each of the 4 sorts gives the expected order;
    - `reload()` calls listWords a second time.
  - **Delete:**
    - row delete of kadto gives the toast "Forgot kadto", the row is gone, and there is no forgetWords call yet;
    - two more deletes within 5 s give "Forgot 3 words";
    - after 5 s of the main loop, exactly one forgetWords with 3 words;
    - Undo before 5 s restores all 3, with no call.
  - **Locked:** forgetWords rejecting with ForgetFailed "unlock your keyring first" brings the rows back, with the toast "Unlock your keyring first".
  - **Multi-select:**
    - select 2 words, change the search to hide one, then forgetSelected → the confirm "Forget 2 words?";
    - Forget gives a batch of 2 (Review Focus 3);
    - Cancel gives nothing.
  - **Close:**
    - close with pending sends forgetWords once before destroy, and the close finishes;
    - a stuck stub closes after 3 s;
    - `close(conn)` is called once.
- [ ] **Step 2:** `gjs -m extension/test/prefs.smoke.js` → FAIL.
- [ ] **Step 3:** Implement `wordsPage.js` with a `Gtk.ListView` over `Gio.ListStore` → `Gtk.FilterListModel` (`Gtk.CustomFilter` from the switch and `matchesSearch`) → `Gtk.SortListModel` (`Gtk.CustomSorter` with `compareWords`). Rows are a check button, the word, a subtitle and a delete button.
- [ ] **Step 4:** Run `gjs -m extension/test/prefs.smoke.js`, `gjs -m extension/test/config.test.js`, `bash scripts/pack-extension.sh` (now 9 files, including wordsPage.js) and `bash scripts/extension-smoke.sh` → PASS.
- [ ] **Step 5:** Commit `feat(prefs): Words page to search, sort and forget learned words`.

### Task 6: Desktop checklist (manual, after all tasks)

- [ ] `bash scripts/install-dev.sh`, `bash scripts/install-extension.sh`, then log out and back in.
- [ ] Settings → Words → Show learned words… → Allow. With "Show all stored words" on, the count matches the menu's words learned.
- [ ] Search "kad". Try each sort. Forget one word, then Undo. Forget one for real, and it is gone after relogin.
- [ ] Select 3 words, then Forget selected, through the confirm.
- [ ] Cancel the Shell prompt, and the page stays hidden.
- [ ] Run `gdbus call --session -d org.gnome.Shell -o /org/nadelacruz/CorrectiondCompanion -m org.nadelacruz.CorrectiondCompanion1.RequestWordAccess` from a terminal. It returns `(false,)`, with no prompt.
