# Quick Settings Status and Learning Controls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the Autocorrect Quick Settings menu shows whether autocorrect is working in the focused app and why, plus counts and a "Learn new words" switch. It lists the last 5 learned words with a ✕ that forgets one word, and moves "Forget all" to Settings.

**Architecture:** the engine owns all state:
- the policy reason lives in corelib;
- the recent list and single-word delete live in `LearnStore` and the persistence actor;
- four new Shell-only D-Bus methods are added (`GetStatus`, `SetLearning`, `RecentWords`, `ForgetWord`).

The GNOME Shell extension is a thin view that asks when the menu opens. Settings reaches the Shell's confirm dialog through a small D-Bus object the extension exports.

**Tech Stack:** Rust (zbus 5, tokio, rusqlite/SQLCipher), GJS (GNOME Shell 50, libadwaita), Python 3 (fake engine), Bash smoke harnesses.

**Spec:** `docs/superpowers/specs/2026-11-06-quick-settings-status-and-learning-design.md`. Its Decision ledger R1-R6 is binding, and its body already includes those amendments.

## Global Constraints

- **Shell-only methods.** `GetStatus`, `SetLearning`, `RecentWords` and `ForgetWord` are Shell-only through the existing `authorize` / `Method::shell_only` (`allow_any_caller` still bypasses). `GetStats` stays open and unchanged.
- **No app identity.** No D-Bus reply carries a WM_CLASS or app name. Only `RecentWords` carries words, and it is Shell-only.
- **Fixed texts.** Logs and error descriptions are fixed texts and never contain a word (existing `_logOnce`, `eprintln!` style).
- **Recent list.** Memory only, capacity 5, newest first. A word enters it only when the user's typing takes its count from 1 to exactly 2 and the word is in no dictionary. Imports never add to it.
- **Pause flag.** Memory only, true at engine start.
- **Extension calls.** Every call uses a 2000 ms timeout.
- **Failure text.** "Could not forget" shows for 3 s.
- **Status texts.** These come verbatim from the spec's "App state line" table, which now includes the `no_replace` row.
- **Error names.** `org.nadelacruz.Correctiond1.Error.ForgetFailed`, with descriptions `"unlock your keyring first"` and `"could not forget"`.
- **Companion object.** It is `org.nadelacruz.CorrectiondCompanion1` at `/org/nadelacruz/CorrectiondCompanion` on the Shell's session connection, with one method, `ConfirmForget()`.

## Review Focus

1. **A word typed in a non-allowlisted app (Observe mode).** It is learned, so it enters the recent list too, the same rule as allowlisted apps. Task 3 pins this.
2. **Forgetting a word that is also imported** (personal vocab from Messenger). It removes the word entirely (memory and disk), including its imported count. Task 2 pins this.
3. **Menu opened before any text field ever focused.** It shows "No text field yet" and never errors. Task 5 (`none`) and Task 6 (vectors row) pin this.
4. **The extension is disabled and re-enabled (screen lock).** The companion D-Bus object is unexported and re-exported without an "object already exported" error, and the pause switch keeps the engine's state. Task 7 pins this.
5. **Same word ✕-clicked twice quickly.** The second call returns `false`, and the menu just refreshes (no error row). Task 5 pins this.

---

### Task 1: Policy reason (corelib)

**Files:**
- Modify: `corelib/src/policy.rs` (around `resolve_mode`, :72-92)
- Test: same file, `#[cfg(test)]`

**Interfaces:**
- Produces: `pub enum Reason { Allowlisted, NotListed, Broken, PrivateField, Terminal, Unidentified, NoReplace }` (derive Debug, Clone, Copy, PartialEq, Eq), with `pub fn code(self) -> &'static str` returning `allowlisted|not_listed|broken|private_field|terminal|unidentified|no_replace`. Also `pub fn resolve_mode_reason(ctx: &Ctx, allowlist: &[String], broken: &[String]) -> (Mode, Reason)`.
- `resolve_mode` becomes `resolve_mode_reason(..).0`.
- `NoReplace` is never returned here. It is set by the engine (Task 5).

- [ ] **Step 1: Write failing tests**
  - `reason_per_rule`, one assertion per row of the spec's reason table:
    - password purpose (8) → `(Off, PrivateField)`;
    - hint `HINT_PRIVATE` → `(Off, PrivateField)`;
    - `org.gnome.ptyxis` not listed → `(Off, Terminal)`;
    - listed and broken terminal → `(Off, Terminal)`;
    - listed terminal → `(ProseAware, Allowlisted)`;
    - `firefox` listed → `(Silent, Allowlisted)`;
    - listed and broken `firefox` → `(Observe, Broken)`;
    - `files` not listed → `(Observe, NotListed)`;
    - no wm_class → `(Observe, Unidentified)`.
  - `reason_wrapper_matches_resolve_mode`: for every `Ctx` built in the existing `resolve_mode` tests, `resolve_mode_reason(..).0 == resolve_mode(..)`.
  - `reason_codes`: each variant's `code()` equals the strings above.
- [ ] **Step 2:** `cargo test -p corelib policy` → FAIL (no `Reason`).
- [ ] **Step 3:** implement. Keep the existing branch order exactly; only attach a reason to each return.
- [ ] **Step 4:** `cargo test -p corelib` → PASS, existing policy tests unchanged.
- [ ] **Step 5:** commit `feat(policy): resolve_mode_reason with a reason per rule`.

### Task 2: Recent list, forget_word, database delete (corelib)

**Files:**
- Modify: `corelib/src/learning.rs` (`LearnStore`, `record_word` :60, `clear` :275)
- Modify: `corelib/src/db.rs` (`impl Db`, next to `write` :394)
- Tests: same files

**Interfaces:**
- Produces:
  - `LearnStore::record_word(&mut self, w: &str) -> u64` returns the new count. Existing callers ignore it.
  - `LearnStore::note_learned(&mut self, w: &str)` pushes the lowercased `w` to the front of `recent`, moves an existing entry to the front, and truncates to 5.
  - `LearnStore::recent_words(&self) -> Vec<String>`, newest first.
  - `LearnStore::forget_word(&mut self, w: &str) -> bool` lowercases `w` and removes the vocab entry, every bigram with `w` as w1 or w2, `w` from `dirty_vocab`, those bigram keys from `dirty_bigrams`, and `w` from `recent`. It returns whether the vocab entry existed.
  - `clear()` also empties `recent`.
  - `Db::forget_word(&mut self, w: &str) -> Result<(), StoreError>` runs in one transaction: `DELETE FROM vocab WHERE word = ?1`, then `DELETE FROM bigram WHERE w1 = ?1 OR w2 = ?1`, then commit, then chmod the `-wal`/`-shm` files as `write` does.
- `LearnStore` gains a `recent: VecDeque<String>` field (`Default` gives it empty).

- [ ] **Step 1: Write failing tests**
  - `record_word_returns_count`: 1, then 2.
  - `recent_list_cap_order_repeat`: push a..f (6 words) → `["f","e","d","c","b"]`; push "c" again → `["c","f","e","d","b"]`; push "Kadto" → front is "kadto".
  - `forget_word_removes_word_pairs_dirty_recent`:
    - record "kadto" twice, bump bigrams (`ako`, `kadto`) and (`kadto`, `na`), `note_learned("kadto")`;
    - `forget_word("KADTO")` is true;
    - `personal_freq("kadto") == 0`, both `bigram_count`s are 0, and `recent_words()` is empty;
    - `take_dirty()` holds no row with "kadto";
    - "ako" is still in vocab;
    - a second `forget_word("kadto")` is false.
  - `forget_word_removes_imported_counts` (Review Focus 2): `add_imported` a batch with `w kadto 12`, then `forget_word` → `personal_freq == 0`.
  - `clear_empties_recent`.
  - db.rs `forget_word_deletes_rows_and_survives_reopen`:
    - `write` a batch with vocab kadto=3, ako=5 and bigram (ako, kadto)=2;
    - `forget_word("kadto")`;
    - reopen with `try_open` → the snapshot has no "kadto" vocab or bigram rows, and "ako" is present.
- [ ] **Step 2:** `cargo test -p corelib learning db` → FAIL.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** `cargo test -p corelib` → PASS.
- [ ] **Step 5:** commit `feat(learning): recent list, forget_word, single-word DB delete`.

### Task 3: Recording recently learned words (engine state)

**Files:**
- Modify: `engine/src/state.rs` (`learn` :311-322)
- Test: same file's tests module

**Interfaces:**
- Consumes: `record_word -> u64` and `note_learned` (Task 2).
- `EngineState::learn` calls `note_learned(word)` when the returned count `== 2` and `!self.dict.contains(word)`.

- [ ] **Step 1: Write failing tests**
  - `typing_a_new_word_twice_adds_it_to_recent`: type "kadto " twice in Silent mode → `recent_words() == ["kadto"]`; a third time → still one entry.
  - `dictionary_word_never_enters_recent`: type "house " twice → empty.
  - `observe_mode_word_enters_recent` (Review Focus 1): Observe mode, the same rule → `["kadto"]`.
  - `paused_learning_adds_nothing`: `set_learning(false)`, type twice → empty, and `personal_freq == 0`.
- [ ] **Step 2:** `cargo test -p engine state` → FAIL.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** PASS.
- [ ] **Step 5:** commit `feat(engine): recently learned words from typing`.

### Task 4: ForgetWord in the persistence actor (R3)

**Files:**
- Modify: `engine/src/persist.rs` (`Req` :86, `PersistHandle` :94-160, `step` :355, `acquire` :420, `persistent` :530)
- Tests: same file (use `FakeBackend`, `persistent_actor`, `locked_calls` as in `locked_stays_memory_then_attaches` :732)

**Interfaces:**
- Produces:
  - `pub const FORGET_LOCKED: &str = "unlock your keyring first";`
  - `pub const FORGET_FAILED: &str = "could not forget";`
  - `PersistHandle::forget_word(&self, word: String) -> Result<bool, String>`. `Ok(found)` means it is gone from memory, and from disk when attached. `Err` is always `FORGET_LOCKED`, `FORGET_FAILED` or `STOPPED`.
- `Req::Forget(String, oneshot::Sender<Result<bool, String>>)`.
- **Persistent:** in `spawn_blocking`, run `db.forget_word(&w)`. On `Ok`, call `lock_store().forget_word(&w)` and reply with its result. On `Err`, memory is untouched, the reply is `Err(FORGET_FAILED)`, and a failure is logged once with a fixed text.
- **Acquiring** (in `step` and `acquire`): if `dir.join("learn.db")` exists, reply `Err(FORGET_LOCKED)`. Otherwise reply `Ok(lock_store().forget_word(&w))`.

- [ ] **Step 1: Write failing tests**
  - `forget_when_persistent_deletes_and_survives_restart`:
    - with `persistent_actor`, record "kadto" twice, `flush_now`, `forget_word("kadto")` → `Ok(true)`;
    - drop the handle, start a new actor on the same dir and fake → after attach, `personal_freq("kadto") == 0`.
  - `forget_while_locked_is_refused_and_attach_keeps_word`:
    - create a store with "kadto" (persistent actor, flush, drop);
    - start a new actor with `fake.state().locked_calls = 2` and a fresh store;
    - while locked, `forget_word("kadto")` → `Err("unlock your keyring first")`;
    - after attach, `personal_freq("kadto") >= 2`, and `forget_word` → `Ok(true)`.
  - `forget_without_store_file_is_memory_only`: locked from the start, no `learn.db` → record twice → `Ok(true)`, and freq is 0.
  - `forget_queued_after_dirty_flush_stays_gone`: record "kadto" twice without flushing, then `forget_word` → `Ok(true)` → `flush_now` → restart → gone.
- [ ] **Step 2:** `cargo test -p engine persist::tests::forget` → FAIL.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** `cargo test -p engine persist` → PASS (existing tests unchanged).
- [ ] **Step 5:** commit `feat(persist): ForgetWord, refused while the store is locked`.

### Task 5: Control methods, app state on read, IBus wiring (R1, R2, R4, R5)

**Files:**
- Modify: `engine/src/control.rs` (`ControlState` :71-131, `Method` :135-151, `ControlError` :154-184, the interface :284-382)
- Modify: `engine/src/ibus.rs` (`focus_in` / `focus_in_id` :790-798, `set_content_type` :658, `set_capabilities` :772, `handle_key` :577)
- Create: `data/status-vectors.tsv`
- Tests: control.rs tests (`authorize` table, `p2p_control` wire tests :589-724) and ibus.rs tests

**Interfaces:**
- Consumes: `resolve_mode_reason` and `Reason` (Task 1); `PersistHandle::forget_word` (Task 4); `LearnStore::recent_words` (Task 2).
- Produces:
  - **New `ControlState` fields:** `learning: AtomicBool` (true), and `field: Mutex<Option<(u32, u32, bool)>>` for the last IBus field's purpose, hints and can_replace (`None` until a field focuses).
  - **New `ControlState` methods:**
    - `set_learning(&self, on: bool)` and `learning(&self) -> bool`;
    - `set_field(&self, purpose: u32, hints: u32, can_replace: bool)`;
    - `app_state(&self) -> Option<(Mode, Reason)>`: `None` until `set_field`. Otherwise it resolves with the stored field and the current focused wm_class. If the result is (Silent or ProseAware) and `!can_replace`, it becomes (Observe or Off, `NoReplace`).
    - `status_json(&self) -> String` returns `{"app_mode":M,"app_reason":R,"enabled":B,"learning":B}`:
      - `M` is `correct` (Silent/ProseAware), `learn_only` (Observe), `off` (Off) or `none`;
      - `R` is `Reason::code()`, or `""` with `none`;
      - when `enabled` is false, M and R still come from `app_state` (the extension shows "Autocorrect is off" first).
  - **`Method`** gains `GetStatus`, `SetLearning`, `RecentWords` and `ForgetWord`, all Shell-only.
  - **`ControlError::ForgetFailed(&'static str)`** has the name `org.nadelacruz.Correctiond1.Error.ForgetFailed`.
  - **The D-Bus interface:**
    - `get_status() -> s`;
    - `set_learning(b)`;
    - `recent_words() -> as`;
    - `forget_word(s) -> b`. With no persist actor (tests) it is memory-only. An empty word, or one over 64 bytes, returns `false`.
  - **IBus wiring:**
    - `focus_in`, `focus_in_id`, `set_content_type` and `set_capabilities` call `shared.control.set_field(self.purpose, self.hints, self.can_replace())`;
    - `handle_key` calls `self.state.set_learning(self.shared.control.learning())` before `on_key`;
    - focus-out does not touch `field`.
- `data/status-vectors.tsv` holds one row per line: `app_mode<TAB>app_reason<TAB>appLine<TAB>subtitle`. Use the spec table's text with the literal token `NAME`, plus a `#` header comment. It covers every (mode, reason) the engine can emit, plus `none<TAB>` (empty reason).

- [ ] **Step 1: Write failing tests**
  - **authorize table:** the 4 new methods are denied for a non-Shell sender and allowed for the Shell owner and with `allow_any_caller`. `GetStats` is still open.
  - **`app_state_is_computed_on_read`** (R2):
    - `set_focused_app("firefox")` (allowlisted), `set_field(0,0,true)` → `(Silent, Allowlisted)`;
    - then `set_focused_app("org.gnome.Nautilus")` with no new field → `(Observe, NotListed)`.
  - **`no_replace_downgrade`** (R1): firefox, `set_field(0,0,false)` → `(Observe, NoReplace)`; a listed terminal with can_replace false → `(Off, NoReplace)`.
  - **`none_before_any_field`** (Review Focus 3): `status_json()` contains `"app_mode":"none","app_reason":""`.
  - **`status_vectors_cover_every_emitted_pair`** (R5):
    - parse `include_str!("../../data/status-vectors.tsv")`;
    - for every Reason × the reachable Modes (drive `app_state` with contexts for each spec row, plus NoReplace and `none`), assert that the `(app_mode, app_reason)` pair is present;
    - assert that no row's app_mode is outside {correct, learn_only, off, none}.
  - **Wire tests** (`p2p_control`):
    - `SetLearning(false)` then `GetStatus` gives `"learning":false`;
    - `RecentWords` returns the store's recent list;
    - `ForgetWord("kadto")` is true, then false (Review Focus 5);
    - `GetStatus` and `GetStats` replies never contain the test wm_class string.
  - **ibus:**
    - `focus_out_keeps_last_field`;
    - `set_capabilities_without_surrounding_gives_no_replace` (end to end through `Engine`).
- [ ] **Step 2:** `cargo test -p engine` → FAIL.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test` → PASS, and `bash scripts/e2e.sh` and `bash scripts/headless-smoke.sh` → PASS.
- [ ] **Step 5:** commit `feat(control): GetStatus, SetLearning, RecentWords, ForgetWord`.

### Task 6: statusText (extension lib)

**Files:**
- Modify: `extension/lib.js`
- Test: `extension/test/lib.test.js`

**Interfaces:**
- Consumes: the `data/status-vectors.tsv` format (Task 5).
- Produces:
  - `export function statusText(stats, status, appName)`:
    - `stats` is the parsed GetStats object or `null`;
    - `status` is the parsed GetStatus object or `null` (`null` for either means the engine is down);
    - `appName` is a string or `null`;
    - it returns `{subtitle, appLine, countsLine}`. `countsLine` is `null` when the engine is down.
  - `export function fillName(template, appName)` replaces `NAME`. With `appName` null it drops `" in NAME"`.
  - `export const STATUS_ROWS`: a frozen object keyed `` `${app_mode}\t${app_reason}` `` → `{appLine, subtitle}`, hand-copied from the vectors file. The test proves it matches.
- **Order** (first match wins):
  1. engine down → "Engine not running" for both, `countsLine` null;
  2. `enabled === false` → "Autocorrect is off" / "Off";
  3. the row from `STATUS_ROWS`;
  4. then, if `learning === false` and not row 1 or 2, the subtitle is "Learning paused".
- **`countsLine`** is `` `${c} corrections · ${r} undone · ${w} words learned` `` using `toLocaleString('en-US')`.

- [ ] **Step 1: Write failing tests**
  - **`statusText` matches every vectors row:** read `../../data/status-vectors.tsv` via `GLib.file_get_contents` relative to `import.meta.url`. For each row, `statusText({...counts}, {app_mode, app_reason, enabled:true, learning:true}, 'Firefox')` → `appLine === fillName(row.appLine,'Firefox')` and the same for subtitle. Also assert that `STATUS_ROWS` has exactly the file's keys.
  - **Learning paused:** `learning:false` with `correct/allowlisted` → subtitle "Learning paused", and appLine is unchanged.
  - **Off wins over paused:** `enabled:false` with `learning:false` → "Off".
  - **Engine down:** `statusText(null, null, 'Firefox')` → subtitle "Engine not running", `countsLine === null`.
  - **Unknown app name:** `correct/allowlisted` with `appName null` → "Correcting".
  - **Counts:** `{corrections:12, reverts:3, words_learned:9000}` → "12 corrections · 3 undone · 9,000 words learned".
- [ ] **Step 2:** `gjs -m extension/test/lib.test.js` → FAIL.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** PASS.
- [ ] **Step 5:** commit `feat(extension): statusText from the shared status vectors`.

### Task 7: The menu, the companion D-Bus object, and the smoke regression contract (R6)

**Files:**
- Modify: `extension/extension.js` (`IFACE` :32-40, `AutocorrectToggle` :43-51, `enable` / `disable`, the owner-change handler `_onOwner`, `_confirmForget` :354)
- Modify: `scripts/fake_correctiond.py` (XML and `call`)
- Modify: `scripts/extension-smoke.sh`
- Create: `scripts/smoke-unsafe@correctiond.test/metadata.json` and `extension.js`, a test-only helper extension installed only by the smoke into its temp HOME. Its enable() sets `global.context.unsafe_mode = true` so `org.gnome.Shell.Eval` works in the headless shell. It is never shipped: not in `install-extension.sh` or `pack-extension.sh`.

**Interfaces:**
- Consumes: `statusText` and `fillName` (Task 6); the D-Bus methods (Task 5).
- **`IFACE`** adds `GetStatus` (out s), `SetLearning` (in b), `RecentWords` (out as) and `ForgetWord` (in s, out b).
- **Menu items** (per the spec's Menu block), kept as fields for the smoke:
  - `this._appItem` and `this._countsItem` (non-reactive `PopupMenu.PopupMenuItem`);
  - `this._learnSwitch` (`PopupSwitchMenuItem` "Learn new words");
  - `this._recentSection` (`PopupMenuSection`);
  - "Autocorrect Settings…", which calls `this.openPreferences()`.
- **Behavior:**
  - The "Forget learned words" action is removed.
  - `menu.connect('open-state-changed', (_m, open) => open && this._refresh())`.
  - `_refresh()` calls GetStats, GetStatus and RecentWords with 2000 ms timeouts and fills the items via `statusText`.
  - The subtitle refresh on `notify::focus-window` uses GetStatus only, debounced by `GLib.timeout_add` of 150 ms.
  - ✕ calls `ForgetWord`, then `_refresh()`. On error, the row label becomes "Could not forget", or "Unlock your keyring first" when the error message is `unlock your keyring first`, and resets after 3000 ms.
  - On engine owner appear, re-send SetEnabled(current) and SetLearning(switch state).
- **Companion object:** `Gio.DBusExportedObject.wrapJSObject(COMPANION_IFACE, {ConfirmForget: () => this._confirmForget()})`, exported in `enable()` on `Gio.DBus.session` at `/org/nadelacruz/CorrectiondCompanion` and unexported in `disable()`.
- **fake_correctiond.py:**
  - it answers `GetStatus` with `{"app_mode":"correct","app_reason":"allowlisted","enabled":true,"learning":true}`, `RecentWords` with `["kadto","sige"]` and `ForgetWord` with `true`;
  - it logs every call as today.

- [ ] **Step 1: Write the failing smoke checks**
  - Inside `--inner`, after the existing marks, add marks and drive via `gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell -m org.gnome.Shell.Eval '<js>'`, where the JS reaches the toggle through `Main.panel.statusArea.quickSettings` and the extension's fields. Add:
    - `toggle-off`: set `toggle.checked=false`;
    - `toggle-on`: set `toggle.checked=true`;
    - `menu-open`: `toggle.menu.open()`;
    - `learn-off`: `_learnSwitch.setToggleState(false)` plus emit `toggled`;
    - `forget-word`: activate the first ✕;
    - `confirm-cancel`: `gdbus call -o /org/nadelacruz/CorrectiondCompanion -m org.nadelacruz.CorrectiondCompanion1.ConfirmForget`, then Eval-click the dialog's Cancel;
    - `confirm-forget`: ConfirmForget, then click Forget;
    - `restart-engine`: kill and restart the fake;
    - `end`.
  - Install the helper extension and add its uuid to `enabled-extensions`.
  - The checks:
    - `between toggle-off toggle-on "SetEnabled (False,)"`;
    - `between toggle-on menu-open "SetEnabled (True,)"`;
    - `between menu-open learn-off "GetStatus"`, `"GetStats"` and `"RecentWords"`;
    - `between learn-off forget-word "SetLearning (False,)"`;
    - `between forget-word confirm-cancel "ForgetWord ('kadto',)"`;
    - no `Purge` between confirm-cancel and confirm-forget;
    - exactly one `Purge` between confirm-forget and restart-engine;
    - `between restart-engine end "SetEnabled (True,)"` and `"SetLearning (False,)"`;
    - disable then re-enable (the existing marks) leaves no "already exported" error in shell.log (Review Focus 4);
    - `no_trace` (no stack trace).
- [ ] **Step 2:** `bash scripts/extension-smoke.sh` → the new checks FAIL.
- [ ] **Step 3:** implement extension.js and fake_correctiond.py.
- [ ] **Step 4:** `bash scripts/extension-smoke.sh` → "extension smoke passed", and `gjs -m extension/test/lib.test.js` → PASS.
- [ ] **Step 5:** commit `feat(extension): status menu, learning switch, recent words, ConfirmForget`.

### Task 8: Forget all in Settings, docs

**Files:**
- Modify: `extension/prefsWindow.js` (the Learning page, near `controller.learningRows` :575-593)
- Modify: `extension/test/prefs.smoke.js`
- Modify: `README.md` (where the menu is described)
- Modify: `scripts/install-extension.sh` and `scripts/pack-extension.sh` only to make sure the helper from Task 7 is NOT included (no change expected; verify)

**Interfaces:**
- Consumes: the companion object (Task 7).
- Produces:
  - an `Adw.ButtonRow` (or ActionRow with a button) titled "Forget all learned words…", exposed as `controller.forgetRow`;
  - `controller.forgetAll()` calls `ConfirmForget` on bus name `org.gnome.Shell` with `opts.companionCall ?? defaultCall`, where `opts.companionCall` is injectable for the smoke as `(callback) => void`;
  - on failure, `forgetRow.subtitle = 'Turn on the Autocorrect extension to use this'`.

- [ ] **Step 1: Write failing checks in prefs.smoke.js**
  - `forget row calls ConfirmForget`: a stub records one call.
  - `forget row shows the disabled message when the call fails`: the stub throws, and the subtitle is that exact text.
- [ ] **Step 2:** `gjs -m extension/test/prefs.smoke.js` → FAIL.
- [ ] **Step 3:** implement, and update the README menu description (status line, counts, "Learn new words", Recently learned, Forget all now in Settings).
- [ ] **Step 4:** `gjs -m extension/test/prefs.smoke.js`, `gjs -m extension/test/config.test.js` and `bash scripts/pack-extension.sh` → PASS. The pack listing has no `smoke-unsafe` file.
- [ ] **Step 5:** commit `feat(prefs): Forget all learned words in Settings`.

### Task 9: Desktop checklist (manual, after all tasks)

- [ ] `bash scripts/install-dev.sh`, `bash scripts/install-extension.sh`, then log out and back in.
- [ ] Open the menu in Firefox, in Files and in Ptyxis → the right line for each. Switch windows without typing and reopen → the line follows the new window.
- [ ] Make a correction and a Backspace revert → the counts move.
- [ ] Turn "Learn new words" off, type a new word twice → it is not in Recently learned and `words_learned` stays flat. `ibus restart` → the switch is on again.
- [ ] Type a new word twice → it appears. ✕ → it is gone, and still gone after relogin.
- [ ] Settings → Learning → Forget all → the Shell dialog. Cancel does nothing.
