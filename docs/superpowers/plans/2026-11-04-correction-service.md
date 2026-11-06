# Correction Service Implementation Plan

> **Note:** the Messenger importer described here now lives in [correctiond-import-cookbook](https://github.com/nadelacruz-00/correctiond-import-cookbook). Paths under `importers/` and `scripts/import-e2e.sh` are historical; the app keeps only the contract in docs/vocab-format.md and docs/import-report.md.


> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust IBus engine that silently autocorrects typos system-wide on GNOME/Wayland, learning the user's vocabulary and layout-specific slip patterns, fully local.

**Architecture:** A pure-logic `corelib` crate (tokenizer, layout-aware scoring, policy, encrypted learning store) is driven by an `engine` crate that implements the IBus engine D-Bus interface via zbus and a control-plane D-Bus service; a thin GJS GNOME Shell extension supplies focused-app wm_class and GJS-OSK visibility. Corrections fire only at word boundaries, only in identified allowlisted apps, via delete_surrounding_text + commit_text with verify-before-delete. Learned data lives in memory on the typing path and is persisted write-behind to a SQLCipher database whose random key is held in the user's GNOME login keyring.

**Tech Stack:** Rust (stable, edition 2021), zbus, rusqlite (`bundled-sqlcipher`, linked against system OpenSSL libcrypto), oo7 (Secret Service client on zbus), getrandom, zeroize, libc, GJS (ESM, GNOME Shell 50), cargo-deb.

**Spec:** `docs/designs/correction-service.md` (copied in Task 1 from the original design doc). Executors read the spec alongside this plan.

## Global Constraints

- Zero network at runtime: the engine and extension open no sockets besides the session D-Bus. Dependencies resolve at build time only.
- An IBus engine crash breaks typing desktop-wide: `process_key_event` handling must never panic (wrap the correction path; on any internal error return "unhandled" and keep passing keys through).
- All persistent data lives under `~/.local/share/correctiond/` with directory mode 0700 and file mode 0600 (including `-wal`/`-shm` files).
- Correction decision p99 < 10 ms on aarch64 CPU (binding criterion; sub-millisecond is the expectation to benchmark).
- **Minimize before encrypting.** Only word-shaped tokens are learned (`is_learnable`, Task 5), and nothing is learned in terminals. `Mode::Off` (no correction, no learning) covers input purposes DIGITS=2, NUMBER=3, PHONE=4, URL=5, EMAIL=6, PASSWORD=8, PIN=9 and the NO_SPELLCHECK and PRIVATE hints. Words and bigrams reach disk only from their second sighting. This minimizes but cannot eliminate secrets: a letters-only passphrase or security answer typed into a field that reports no sensitive purpose (some Electron/Qt apps) is still learnable. That is the residual risk encryption covers.
- **Encryption at rest.** `learn.db` is a SQLCipher database (AES-256 pages, HMAC-SHA512 page authentication) keyed by a random 256-bit raw key passed as `x'<hex>'` (no PBKDF2 cost). A plaintext database is never created, not even as a fallback. Tests assert SQLCipher is actually linked (`PRAGMA cipher_version` non-empty).
- **Key custody.** The key is generated from the OS CSPRNG (`getrandom`) and stored only as a Secret Service item in the user's default (login) collection via `oo7`. Our own copies live only in `zeroize::Zeroizing` buffers and are dropped right after the database opens. SQLCipher holds key material for the open connection and wipes it on close; copies inside libraries (rusqlite's statement text, D-Bus message buffers) are best-effort only. The key never touches config files, logs, our own D-Bus methods, or argv.
- **Never prompt, never block typing.** The engine never calls Unlock and never triggers a keyring prompt. Keyring locked or unavailable ⇒ learning continues in memory only and attaches to the encrypted store once the key is obtainable. Keystrokes are never delayed by keyring or disk I/O.
- **Hot path is RAM only.** `PersonalLookup` reads in-memory maps; disk writes are batched write-behind flushes (every 30 s, on FocusOut, Disable and exit). Encryption cost therefore lands only on background flushes.
- **No typed content in logs, no memory dumps.** Logs never contain characters, words or key material, and nothing is logged in `Mode::Off`. Three separate controls cover memory: `PR_SET_DUMPABLE=0` (blocks ptrace and `/proc/<pid>/mem` for non-root), `RLIMIT_CORE=0` (blocks plain core files), and an apport ignore entry shipped in Task 14. The apport entry is required because this machine runs `fs.suid_dumpable=2` with cores piped to apport, which still writes `/var/crash/*.crash` with full memory for packaged binaries even with the first two set.
- **Threat model scope.** This protects learned data in backups (any backup that includes `$HOME`), on a powered-off stolen device without full-disk encryption, and from other local users. That protection is only as strong as the login password: backups contain both `learn.db` and `~/.local/share/keyrings/login.keyring`, which can be attacked offline, and a login keyring with an empty password stores the key effectively in plaintext. It does not protect against malware running as the user, which can request the key from the Secret Service. Full-disk encryption (LUKS) is the OS-level complement.
- v1 corrects only pure-ASCII original+candidate pairs (octet-offset mitigation, spec Constraints).
- Unidentified app (no wm_class) ⇒ no corrections, observe-only (spec: app identification).
- The `Mode::Off` purposes and the NO_SPELLCHECK/PRIVATE hints are hard-off and ship in the same task as the first live corrections (Task 10), never later.
- The spec's Assignment (surrounding-text probe matrix in Ptyxis/Firefox/gnome-text-editor) gates widening the allowlist beyond Firefox; it does not block Tasks 1–9.
- No changes to gjs-osk (`gjsosk@vishram1123.com`): OSK detection happens from our own extension observing its actor.
- Deliberate v1 cuts from the spec's control-plane wishlist: per-app mute and last-correction peek are post-v1 (allowlist editing in config.toml covers muting); preedit fallback for broken apps degrades to observe-only in v1.
- Commit after every task (steps below); repo is initialized in Task 1.

## Review Focus

- A boundary key with an empty word buffer (double space, leading space) must produce no correction attempt and no state corruption → test in Task 7.
- Non-ASCII input (é, ß, emoji, CJK) anywhere in a word must disqualify that word from correction without panicking on multi-byte chars → tests in Tasks 1 and 4.
- Focus churn mid-word (app closes, window switch) must drop the pending word so no stale delete fires in the newly focused app → test in Task 7.
- A very long alphabetic run (500+ chars, paste-like) must be rejected cheaply (length cap) instead of exploding SymSpell candidate generation → test in Task 3.
- A locked or absent keyring at session start, or a `learn.db` that cannot be opened with the current key (corrupt file, keyring reset, wrong key), must leave the engine typing normally and learning in memory. It must never prompt, never crash, and never delete the unreadable file (quarantine it by renaming) → tests in Task 6; keyring states in Task 9.

---

### Task 1: Workspace scaffold + key classification and word buffer

**Files:**
- Create: `Cargo.toml` (workspace members: `corelib`, `engine`), `corelib/Cargo.toml`, `corelib/src/lib.rs`, `corelib/src/tokenizer.rs`, `.gitignore`, `LICENSE-MIT`, `LICENSE-APACHE`, `docs/designs/correction-service.md` (copy of spec)
- Test: `corelib/src/tokenizer.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: nothing (first task)
- Produces:
  - `pub enum KeyClass { Printable(char), Boundary(Option<char>), Backspace, Invalidate }`
  - `pub fn classify_key(keyval: u32, state: u32) -> KeyClass` — keyvals are X11/IBus keysyms. Exhaustive rule, checked in this order: any Ctrl/Alt/Super modifier in `state` ⇒ `Invalidate`; BackSpace ⇒ `Backspace`; space and sentence punctuation `, ; : ! ?` ⇒ `Boundary(Some(c))`; Return and Tab ⇒ `Boundary(None)` (the word completes but no boundary char is committable — the real key must still reach the app; see Task 7); arrows/Home/End/PgUp/PgDn/Delete/Escape and every other non-printing keysym ⇒ `Invalidate`; EVERY remaining printable keysym — letters, digits, apostrophe, and ASCII symbols including `/ - _ . ~ $ =` — ⇒ `Printable(c)`, so path/flag/identifier characters enter the word buffer and disqualify the token downstream. `.` is Printable, NOT Boundary: `file.txt` must stay one token; sentence-final periods are handled by the trailing-period strip pinned in Task 7. Non-ASCII printable keysyms are also `Printable` (disqualified later by the decision rule, not dropped here)
  - `pub struct WordBuffer` with `push(&mut self, c: char)`, `pop(&mut self) -> Option<char>` (the removed char; None when empty — Task 7 uses it to learn slips, eng review D8), `clear(&mut self)`, `word(&self) -> &str`, `len_chars(&self) -> usize`

- [ ] **Step 1: Initialize repo and workspace**

```bash
cd ~/Development/local-smart-auto-correct-with-learning
git init -b main
mkdir -p docs/designs
# the original design doc is already at docs/designs/correction-service.md
cargo new --lib corelib && rm -rf corelib/.git
```
Write the workspace `Cargo.toml` (`members = ["corelib", "engine"]`, `resolver = "2"`, `[workspace.package] license = "MIT OR Apache-2.0"` inherited by every crate via `license.workspace = true`; comment out `engine` until Task 8 creates it), `.gitignore` (`/target`, `/.gstack/`, `/.superpowers/`), and the standard MIT and Apache-2.0 license texts as `LICENSE-MIT` / `LICENSE-APACHE` (copyright holder "nadelacruz-00"; eng review D6). The GNOME extension zip (Task 11) includes copies of both files, since `metadata.json` has no license field.

- [ ] **Step 2: Write failing tests for `classify_key` and `WordBuffer`**

Test names and assertions (keysym constants: space=0x20, a=0x61, BackSpace=0xff08, Left=0xff51, Return=0xff0d; state bit ControlMask=1<<2):
```rust
#[test] fn letter_is_printable() { assert_eq!(classify_key(0x61, 0), KeyClass::Printable('a')); }
#[test] fn space_is_boundary() { assert_eq!(classify_key(0x20, 0), KeyClass::Boundary(Some(' '))); }
#[test] fn return_is_charless_boundary() { assert_eq!(classify_key(0xff0d, 0), KeyClass::Boundary(None)); }
#[test] fn path_chars_are_printable() { /* '/'=0x2f, '-'=0x2d, '.'=0x2e, '_'=0x5f each ⇒ Printable(that char) */ }
#[test] fn ctrl_combo_invalidates() { assert_eq!(classify_key(0x61, 1 << 2), KeyClass::Invalidate); }
#[test] fn arrow_invalidates() { assert_eq!(classify_key(0xff51, 0), KeyClass::Invalidate); }
#[test] fn backspace_classified() { assert_eq!(classify_key(0xff08, 0), KeyClass::Backspace); }
#[test] fn buffer_push_pop_word() { /* push 't','e','h'; assert word()=="teh"; pop()==Some('h'); word()=="te"; clear(); pop()==None */ }
#[test] fn buffer_handles_multibyte() { /* push 'é'; len_chars()==1; pop()==true; word()=="" — no panic */ }
```

- [ ] **Step 3: Run tests to verify they fail** — `cargo test -p corelib` — expected: compile error (types not defined)
- [ ] **Step 4: Implement `KeyClass`, `classify_key`, `WordBuffer` in `corelib/src/tokenizer.rs`**
- [ ] **Step 5: Run `cargo test -p corelib`** — expected: PASS
- [ ] **Step 6: Commit** — `git add -A && git commit -m "feat: workspace scaffold, key classification and word buffer"`

### Task 2: Layout adjacency tables

**Files:**
- Create: `corelib/src/layout.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: nothing
- Produces:
  - `pub enum LayoutId { Colemak, Qwerty, OskTap }`
  - `pub fn sub_cost(layout: LayoutId, a: char, b: char) -> f32` — 0.0 when `a == b`; 0.4 when `b` is a physical neighbor of `a` on that layout (OskTap uses QWERTY geometry; its neighbor cost is 0.3 because fat-finger substitution dominates touch typing); 1.0 otherwise. Case-insensitive.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn colemak_neighbors_differ_from_qwerty() {
    // On Colemak, 'e' sits where QWERTY 'k' is; its row neighbors are 'n' and 'i'.
    assert_eq!(sub_cost(LayoutId::Colemak, 'e', 'i'), 0.4);
    assert_eq!(sub_cost(LayoutId::Qwerty, 'e', 'i'), 1.0);
    assert_eq!(sub_cost(LayoutId::Qwerty, 'e', 'r'), 0.4);
}
#[test] fn osk_uses_qwerty_geometry_cheaper() { assert_eq!(sub_cost(LayoutId::OskTap, 'e', 'r'), 0.3); }
#[test] fn same_char_zero_other_full() { assert_eq!(sub_cost(LayoutId::Qwerty, 'a', 'a'), 0.0); assert_eq!(sub_cost(LayoutId::Qwerty, 'a', 'p'), 1.0); }
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p corelib layout` — expected: compile error
- [ ] **Step 3: Implement** — encode each layout as rows of key positions (static arrays for QWERTY and Colemak main three rows); neighbors = Chebyshev-adjacent positions (same row ±1 column, adjacent row within ±1 column offset)
- [ ] **Step 4: Run tests** — expected: PASS
- [ ] **Step 5: Commit** — `git commit -am "feat: layout-aware substitution cost tables"`

### Task 3: Multi-language dictionary with SymSpell candidate index

**Files:**
- Create: `corelib/src/dictionary.rs`, `scripts/build-lexicon.py`, `data/en_words.txt` (generated by the script, committed), `data/ATTRIBUTION.md`
- Test: inline `#[cfg(test)]` using small fixture lexicons built in the test; `scripts/test_build_lexicon.py` (`python3 -m unittest`)

**Interfaces:**
- Consumes: nothing
- Produces:
  - **Lexicon file format** (one format for every language, eng review D4): UTF-8 text, one `word<TAB>count` per line, lowercase, `#` comment lines ignored. Entries containing any non-ASCII character are DROPPED at load (octet-offset mitigation; Tagalog words with ñ are therefore not in v1 lexicons). Mark that drop in `parse_lexicon` with the comment `// gstack-shortcut(dec-b420d3ff-d38a-4665-b51d-d7dc094a731c): ASCII-only lexicons, upgrade when per-app byte-vs-char offset detection exists` (eng review D12; see TODOS.md)
  - `pub struct LangId(pub u8)` (index into the configured language list, max 16) and `pub struct LangSet(pub u16)` (bitmask) with `contains`, `insert`, `is_empty`, `ALL(n)`
  - `pub struct Dictionary`
  - `pub fn from_lexicons(lexicons: Vec<(LangId, Vec<(String, u64)>)>) -> Dictionary` — merges all languages into ONE word table and ONE candidate index; a word present in several lexicons carries all their `LangId`s
  - `pub fn parse_lexicon(text: &str) -> Vec<(String, u64)>` (applies the format rules above; malformed lines skipped)
  - `pub fn contains(&self, w: &str) -> bool` (case-insensitive, any language)
  - `pub fn langs_of(&self, w: &str) -> LangSet` (empty when absent)
  - `pub fn primary_lang(&self, w: &str) -> Option<LangId>` — the language with the highest `rel_freq` for that word (ties ⇒ lowest LangId); None when absent. Language detection votes with this, because web corpora for Tagalog and Waray contain common English words ("the", "and") that would otherwise vote for the wrong language
  - `pub fn rel_freq(&self, w: &str, lang: LangId) -> f32` — the word's count in that language per million words of that lexicon's total (0.0 when absent), so lexicons of different sizes compare fairly
  - `pub fn candidates(&self, w: &str, max_edits: u8, allowed: LangSet) -> Vec<&str>` — SymSpell delete-index lookup, max_edits ≤ 2, returning only words with at least one language in `allowed`; input longer than 24 chars returns empty (length cap, Review Focus)
  - `scripts/build-lexicon.py --lang <code> --freq <file> [--freq-format leipzig|plain] [--words <wordset file>] [--min-count N] --out <path>`: writes the lexicon format. `--freq-format leipzig` reads Leipzig Corpora `*-words.txt` (`<rank>\t<word>\t<count>`); `plain` reads `word count`. With `--words`, only words in that set are kept and words missing from the frequency file get count 1. Lowercases, keeps ASCII letters plus apostrophe, drops entries below `--min-count` (default 3), writes 0644 sorted by count
  - `data/en_words.txt` (eng review D3): generated with `--lang en --words /usr/share/dict/american-english --freq <hermitdave en_50k.txt> --freq-format plain`, i.e. known English words come from SCOWL (vetted spellings), frequencies from en_50k when present, else 1
  - `data/ATTRIBUTION.md`: SCOWL (wamerican) notice, hermitdave FrequencyWords en_50k (CC-BY-SA-4.0 data), SQLCipher (BSD-style), OpenSSL (Apache-2.0); and how to build the TL/WAR lexicons locally (they are never committed or shipped)

- [ ] **Step 1: Write the lexicon builder and its test** — `scripts/test_build_lexicon.py` asserts: a Leipzig-format fixture (`1\tAko\t500\n2\tKAMI\t40\n3\tbata1\t9\n4\tñino\t8\n5\trare\t2\n`) with `--min-count 3` yields exactly `ako\t500` and `kami\t40`; a `--words` set `{build, the}` with plain freq `the 1000\nbuld 9\n` yields `the\t1000` and `build\t1`. Run `python3 -m unittest scripts/test_build_lexicon.py` — expected FAIL, then implement `scripts/build-lexicon.py` with the standard library only, then PASS
- [ ] **Step 2: Generate the English lexicon** — download hermitdave FrequencyWords `content/2018/en/en_50k.txt` to a scratch path (a build-time fetch, not a runtime one; do not commit it), run the builder as described above to produce `data/en_words.txt`, commit `data/en_words.txt`, `scripts/build-lexicon.py`, the test and `data/ATTRIBUTION.md`. If offline, use `--words /usr/share/dict/american-english` with no `--freq` (all counts 1) and note it in the commit message
- [ ] **Step 3: Write failing Rust tests** (fixture lexicons: EN `the 1000, build 50, building 30, hello 80, may 40`; TL `ako 500, kami 40, may 300`; WAR `maupay 60, ako 20`)

```rust
#[test] fn contains_and_langs() { /* contains("ako"); langs_of("ako") == {TL, WAR}; langs_of("may") == {EN, TL}; !contains("teh") */ }
#[test] fn rel_freq_per_language() { /* rel_freq("ako", TL) == 500 / 840 * 1e6 (TL fixture total 840); rel_freq("zzz", EN) == 0.0 */ }
#[test] fn primary_lang_by_relative_frequency() { /* add "the 5" to the TL fixture: primary_lang("the") == EN; primary_lang("ako") == TL; primary_lang("zzz") == None */ }
#[test] fn candidates_within_two_edits() { /* candidates("bulid",2,ALL) contains "build"; candidates("teh",2,ALL) contains "the" */ }
#[test] fn candidates_respect_language() { /* candidates("maupy",2,{WAR}) contains "maupay"; candidates("maupy",2,{EN}) does not */ }
#[test] fn long_input_rejected() { /* candidates(&"a".repeat(500), 2, ALL).is_empty() */ }
#[test] fn nonascii_entries_filtered() { /* parse_lexicon("café\t10\nniño\t5\n") is empty */ }
#[test] fn long_word_edits_found() { /* fixture "understanding 10": candidates("understnading",2,ALL) and candidates("undrestanding",2,ALL) both contain "understanding" (edits after and inside the 7-char prefix) */ }
#[test] fn index_size_budget() { /* Dictionary from data/en_words.txt: index_bytes() <= 32 * 1024 * 1024 */ }
```

- [ ] **Step 4: Run to verify failure** — `cargo test -p corelib dictionary` — expected: compile error
- [ ] **Step 5: Implement** (eng review D11) — SymSpell with prefix length 7: for each word, generate the deletes (depth ≤ 2) of its first 7 chars; key each delete by a 64-bit hash (`std::hash::DefaultHasher` with a fixed seed, or `ahash` with fixed keys, so builds are deterministic); store one sorted `Vec<(u64 key, u32 start, u32 len)>` plus one flat `Vec<u32>` of word indices (deduplicated per key). Lookup: hash the deletes (depth ≤ 2) of the input's first 7 chars, binary-search each, gather word indices, drop words outside `allowed`, and return the rest; `decide` (Task 4) verifies every candidate with full `weighted_dl`, so hash collisions and prefix-only matches can only add candidates that verification rejects. Add `pub fn index_bytes(&self) -> usize` (keys table + postings + word table)
- [ ] **Step 6: Run tests** — expected: PASS
- [ ] **Step 7: Commit** — `git commit -am "feat: multi-language dictionary with symspell candidate index"`

### Task 4: Scoring and correction decision

**Files:**
- Create: `corelib/src/scoring.rs`, `corelib/benches/decide.rs`
- Modify: `corelib/Cargo.toml` (add `criterion` dev-dependency, `[[bench]]`)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `Dictionary`, `LangSet`, `LangId` (Task 3), `sub_cost`/`LayoutId` (Task 2)
- Produces:
  - `pub fn weighted_dl(a: &str, b: &str, layout: LayoutId, personal: &dyn PersonalLookup) -> f32` — Damerau-Levenshtein from typed `a` to candidate `b`, where substituting typed char x for intended char y costs `max(0.1, sub_cost(layout, x, y) - 0.2 * min(1.0, slip_count(layout, x, y) as f32 / 5.0))` (personal slips, eng review D8), insert/delete cost 1.0, adjacent transposition cost 0.7; case-insensitive
  - `pub struct Verdict { pub replacement: String }`
  - `pub struct LangCtx { pub allowed: LangSet, pub margin: f32 }` (built by Task 5's `LangWindow::ctx`)
  - `pub fn decide(word: &str, prev_word: Option<&str>, layout: LayoutId, lang: &LangCtx, dict: &Dictionary, personal: &dyn PersonalLookup) -> Option<Verdict>`
  - `pub trait PersonalLookup { fn personal_freq(&self, w: &str) -> u64; fn is_blacklisted(&self, orig: &str, cand: &str) -> bool; fn bigram_count(&self, w1: &str, w2: &str) -> u64; fn slip_count(&self, layout: LayoutId, typed: char, intended: char) -> u64; }`
  - Decision rule (exact numbers pinned by this plan — the spec leaves thresholds configurable): return None if `dict.contains(word)` (in ANY language — a Tagalog or Waray word is never corrected) or `personal_freq(word) ≥ 2`, or word length < 3 or > 24 (the ≥ 4 floor applies only in terminal mode via `terminal_token_ok`), or word is not pure ASCII alphabetic (apostrophe allowed). Otherwise take `dict.candidates(word, 2, lang.allowed)` and score each candidate c: `score(c) = weighted_dl(word, c, layout, personal) - 0.2 * log10(max(best_rel_freq(c) + 10 * personal_freq(c), 1) as f32) - (0.3 if prev_word given and bigram_count(prev, c) > 0)`, where `best_rel_freq(c)` is the highest `rel_freq(c, l)` over languages l in both `langs_of(c)` and `lang.allowed`. Accept the best candidate iff its `weighted_dl ≤ 1.5`, it is pure ASCII, it is not blacklisted against `word`, and it beats the runner-up score by ≥ `lang.margin` (sole candidate counts as beating). Preserve the original word's leading capitalization in the replacement.

- [ ] **Step 1: Write failing tests** (reuse Task 3 fixture lexicons + a stub `PersonalLookup` returning 0/false; `ALL` = all three languages with margin 0.3 unless stated)

```rust
#[test] fn fixes_transposition() { /* decide("bulid", None, Qwerty, ALL, ...) == Some("build") */ }
#[test] fn known_word_untouched() { /* decide("build", ...) == None */ }
#[test] fn filipino_word_never_corrected() { /* decide("ako", ...) == None and decide("kami", ...) == None even with allowed = {EN} */ }
#[test] fn waray_typo_fixed_toward_waray() { /* allowed {WAR}: decide("maupy", ...) == Some("maupay") */ }
#[test] fn english_blocked_in_filipino_context() { /* allowed {TL}: decide("bulid", ...) == None (no Tagalog candidate) */ }
#[test] fn personal_word_untouched() { /* personal_freq("gstack")=5 ⇒ decide("gstack", ...) == None */ }
#[test] fn blacklisted_pair_skipped() { /* is_blacklisted("bulid","build")=true ⇒ None */ }
#[test] fn ambiguous_margin_rejected() { /* two candidates within margin ⇒ None; same pair with margin 0.6 and a 0.4 gap ⇒ None, with margin 0.3 ⇒ Some */ }
#[test] fn personal_slip_lowers_cost() { /* stub slip_count(Colemak,'n','e') = 5: weighted_dl("bnd","bed",Colemak) is 0.2 lower than with slip_count 0, and never below 0.1 per substitution */ }
#[test] fn capitalization_preserved() { /* decide("Bulid", ...) == Some("Build") */ }
#[test] fn short_and_nonascii_skipped() { /* 2-char "ot" ⇒ None; "café" ⇒ None */ }
```

- [ ] **Step 2: Run to verify failure** — expected: compile error
- [ ] **Step 3: Implement `weighted_dl` and `decide`**
- [ ] **Step 4: Run tests** — expected: PASS
- [ ] **Step 5: Benchmark** — criterion bench calling `decide` on 5 typo fixtures against a dictionary built from `data/en_words.txt` alone (TL/WAR lexicons are local-only and absent in CI); record median AND worst observed in the commit message. Gate: worst observed < 10 ms (matches the binding p99 criterion; expectation: < 1 ms).
- [ ] **Step 6: Commit** — `git commit -am "feat: language-aware weighted scoring and correction decision"`

### Task 5: Context policy (modes, terminal classifier, prose-run)

**Files:**
- Create: `corelib/src/policy.rs`, `data/learnable-vectors.tsv` (shared with Task 13's Python importer so both token filters stay identical, eng review red flag 8)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `KeyClass` (Task 1), `LangId`, `LangSet` (Task 3), `LangCtx` (Task 4)
- Produces:
  - `pub enum Mode { Off, Observe, Silent, ProseAware }` — Off = no correction AND no learning (secrets must never reach the store); Observe = learn-only, never correct (the unidentified/non-allowlisted/broken-app state)
  - `pub struct Ctx { pub purpose: u32, pub hints: u32, pub wm_class: Option<String>, pub osk_visible: bool }` (purpose/hints are IBus `InputPurpose`/`InputHints` raw values; FREE_FORM=0, ALPHA=1, DIGITS=2, NUMBER=3, PHONE=4, URL=5, EMAIL=6, NAME=7, PASSWORD=8, PIN=9, TERMINAL=10 — IBusInputPurpose mirrors GtkInputPurpose. Verify on this machine with `python3 -c 'import gi; gi.require_version("IBus","1.0"); from gi.repository import IBus; P=IBus.InputPurpose; print([(n, int(getattr(P,n))) for n in ("DIGITS","NUMBER","PHONE","URL","EMAIL","PASSWORD","PIN","TERMINAL")])'` and keep the constants in ONE place)
  - `pub const OFF_PURPOSES: [u32; 7] = [2, 3, 4, 5, 6, 8, 9]` (DIGITS, NUMBER, PHONE, URL, EMAIL, PASSWORD, PIN), per spec "Security: data at rest" item 1, which supersedes the spec's older PASSWORD/PIN-only content-type bullet. OTPs, card numbers, phone numbers, email addresses and URLs are never corrected or learned.
  - `pub const HINT_NO_SPELLCHECK: u32` and `pub const HINT_PRIVATE: u32` — IBus InputHints bits; PRIVATE means "do not update personalized data" (set by private/incognito contexts). Expected values `1 << 1` and `1 << 11`; verify both with `python3 -c 'import gi; gi.require_version("IBus","1.0"); from gi.repository import IBus; H=IBus.InputHints; print(int(H.NO_SPELLCHECK), int(H.PRIVATE))'` and fix the constants if they differ
  - `pub const TERMINAL_WM_CLASSES: [&str; 5] = ["org.gnome.ptyxis", "org.gnome.console", "kgx", "gnome-terminal-server", "org.gnome.terminal"]` (lowercased) — a focused app in this list counts as a terminal even if it reports no TERMINAL purpose (Open Question 2 is still unanswered)
  - `pub fn resolve_mode(ctx: &Ctx, allowlist: &[String], broken: &[String]) -> Mode` — evaluated in this order. (1) Purpose in `OFF_PURPOSES`, or the NO_SPELLCHECK or PRIVATE hint set ⇒ **Off** (nothing typed there is corrected OR learned). (2) **Terminal** (purpose TERMINAL, or `wm_class` in `TERMINAL_WM_CLASSES`): ⇒ `ProseAware` only when `wm_class` is Some, allowlisted and not broken; otherwise ⇒ **Off**. A terminal is never Observe, because Observe learns and a passphrase at a hidden `sudo`/`ssh` prompt looks like prose. (3) Non-terminal fields: `wm_class` None ⇒ Observe; wm_class in `broken` ⇒ Observe (preedit fallback is post-v1; v1 degrades to observe); wm_class not in allowlist ⇒ Observe; else Silent
  - `pub struct LangWindow` (eng review D4) — the field's recent language evidence: `new(n_langs: usize)`, `push(&mut self, lang: LangId)` (keeps the last 4 KNOWN words' primary languages), `reset(&mut self)`, `dominant(&self) -> Option<LangId>` (the language with the most votes in the window; tie or empty ⇒ None), and `ctx(&self) -> LangCtx` — `Some(l)` ⇒ `LangCtx { allowed: {l}, margin: 0.3 }`; `None` ⇒ `LangCtx { allowed: ALL(n_langs), margin: 0.6 }`
  - `pub fn effective_layout(variant: LayoutId, osk_visible: bool) -> LayoutId` — OskTap when `osk_visible`, else `variant`
  - `pub struct ProseRun` with `on_word_completed(&mut self, token: &str)`, `reset(&mut self)`, `ready(&self) -> bool` (true when ≥ 2 CLEAN prose words completed BEFORE the candidate — candidate never counts). `on_word_completed` increments ONLY when `terminal_token_ok(token)` holds; a completed token that fails it RESETS the counter to zero (spec: "until two clean prose words are observed again"). So `git commit -m ` never primes the counter.
  - `pub fn terminal_token_ok(token: &str) -> bool` — pure ASCII alphabetic, lowercase or Capitalized (no mixed case), ≥ 4 chars, none of `/ - _ . ~ $ =`
  - `pub fn is_learnable(token: &str, mode: Mode) -> bool` — the data-minimization gate for every write to the store. `Off` ⇒ false. `ProseAware` ⇒ false: nothing typed in a terminal is learned, because a passphrase typed at a no-echo `sudo`/`ssh`/`gpg` prompt is indistinguishable from prose (deliberate, per spec Security item 1). `Silent`/`Observe` ⇒ true only when the token is 3–24 chars, ASCII letters plus apostrophe only, lowercase or Capitalized (no digits, symbols or mixed case, so `hunter2`, `sk-abc123` and `KEY=x` are never learned)
  - `pub fn bigram_learnable(mode: Mode) -> bool` — false for `Off` and `ProseAware` (no word pairs from terminals); true otherwise

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn password_hard_off() { /* purpose PASSWORD ⇒ Off (not Observe) even when allowlisted — no correction, no learning */ }
#[test] fn sensitive_purposes_off() { /* each of DIGITS, NUMBER, PHONE, URL, EMAIL, PIN ⇒ Off even when allowlisted */ }
#[test] fn private_hint_off() { /* allowlisted FREE_FORM with HINT_PRIVATE set ⇒ Off */ }
#[test] fn learnable_rejects_secret_shapes() { /* Silent: "hunter2", "sk-abc123", "KEY=x", "GitHub", "ab" all false; "gstack", "Gstack" true; Off: "gstack" false */ }
#[test] fn learnable_vectors_file() { /* every line of data/learnable-vectors.tsv (`token<TAB>0|1`) matches is_learnable(token, Silent); the file holds at least: gstack 1, Gstack 1, ako 1, maupay 1, hunter2 0, sk-abc123 0, KEY=x 0, GitHub 0, ab 0, don't 1, café 0 */ }
#[test] fn terminal_never_learns() { /* is_learnable("building", ProseAware) == false */ }
#[test] fn terminal_unidentified_is_off() { /* purpose TERMINAL, wm_class None ⇒ Off */ }
#[test] fn terminal_not_allowlisted_is_off() { /* purpose TERMINAL, wm_class "org.gnome.ptyxis", allowlist ["firefox"] ⇒ Off */ }
#[test] fn terminal_by_wm_class() { /* purpose FREE_FORM, wm_class "org.gnome.ptyxis" not allowlisted ⇒ Off; allowlisted ⇒ ProseAware */ }
#[test] fn no_terminal_bigrams() { /* bigram_learnable(ProseAware) false; bigram_learnable(Silent) true; bigram_learnable(Off) false */ }
#[test] fn unidentified_app_observes() { /* wm_class None ⇒ Observe */ }
#[test] fn terminal_purpose_prose_aware() { /* allowlisted, not broken, TERMINAL ⇒ ProseAware */ }
#[test] fn broken_app_observes() { /* wm_class in broken ⇒ Observe */ }
#[test] fn lang_window_majority() { /* push TL,TL,EN ⇒ dominant TL ⇒ ctx {TL}, 0.3; push EN,TL only ⇒ tie ⇒ None ⇒ ctx ALL, 0.6; a 5th push evicts the oldest; reset ⇒ None */ }
#[test] fn osk_overrides_layout() { assert_eq!(effective_layout(LayoutId::Colemak, true), LayoutId::OskTap); }
#[test] fn prose_run_excludes_candidate() { /* new ProseRun: ready()==false; one clean completion: false; two: true; reset ⇒ false */ }
#[test] fn prose_run_rejects_command_priming() { /* on_word_completed("gitx")? no: complete "commit" (clean, +1), "them" (clean, +1) ⇒ ready; but complete "git"(fails len), "-m"(fails chars) ⇒ counter RESET to 0, not ready */ }
#[test] fn command_tokens_rejected() { /* "git"(len 3), "rm", "--force", "a/b", "myVar" all false; "building", "Building" true */ }
```

- [ ] **Step 2: Run to verify failure** — expected: compile error
- [ ] **Step 3: Implement `policy.rs`**
- [ ] **Step 4: Run tests** — expected: PASS
- [ ] **Step 5: Commit** — `git commit -am "feat: context policy, learning gate, terminal classifier, prose-run counter"`

### Task 6: Encrypted learning store (in-memory cache + separate SQLCipher writer)

**Files:**
- Create: `corelib/src/learning.rs` (in-memory cache), `corelib/src/db.rs` (encrypted database writer), `corelib/src/key.rs`, `corelib/benches/write.rs`
- Modify: `corelib/Cargo.toml` (add `rusqlite = { version = "0.40", features = ["bundled-sqlcipher"] }`, which builds SQLCipher and links the system OpenSSL libcrypto — needs `libssl-dev` at build time, already installed here; add `getrandom` and `zeroize`; add `tempfile` as a dev-dependency; add a `[[bench]]` for `write`). Do NOT use `bundled-sqlcipher-vendored-openssl`: linking the distro libcrypto keeps OpenSSL security fixes flowing through apt.
- Test: inline `#[cfg(test)]` using `tempfile` dirs

**Interfaces:**
- Consumes: `LayoutId` (Task 2), `PersonalLookup` trait (Task 4)
- Produces:
  - `corelib/src/key.rs`: `pub struct StoreKey(zeroize::Zeroizing<[u8; 32]>)` with `pub fn generate() -> StoreKey` (32 bytes from `getrandom`), `pub fn from_hex(s: &str) -> Option<StoreKey>` (exactly 64 hex chars), and `pub fn to_hex(&self) -> zeroize::Zeroizing<String>`. No `Debug`, `Display`, `Clone` or `Serialize` impls, so the key cannot leak through formatting or logs.
  - **Cache (`learning.rs`), never touches disk:**
    - `pub struct LearnStore` implementing `PersonalLookup` from in-memory maps (vocab keyed by (word, layout) with freq and last_seen; blacklist set; bigram counts), plus per-row dirty flags. No SQL, file or keyring access anywhere in this type
    - `pub type SharedStore = std::sync::Arc<std::sync::Mutex<LearnStore>>` — one per engine process, shared by both layout variants. Every user locks with `lock().unwrap_or_else(|e| e.into_inner())`, never `unwrap()`, so a poisoned mutex cannot panic inside `process_key_event`
    - `pub fn memory() -> LearnStore` (empty, `is_persistent() == false`); `pub fn set_persistent(&mut self, on: bool)`; `pub fn is_persistent(&self) -> bool`
    - `record_word(&mut self, w: &str)` (increment; sets `last_seen` to now; marks dirty), `record_blacklist(&mut self, orig: &str, cand: &str)`, `bump_bigram(&mut self, w1: &str, w2: &str)`, `record_slip(&mut self, layout: LayoutId, typed: char, intended: char)` (eng review D8). Callers gate these with `is_learnable`/`bigram_learnable` (Task 5); the store does not re-check token shape
    - `pub fn take_dirty(&mut self) -> DirtyBatch` — clones dirty rows and clears their flags, applying the **second-sighting rule**: a vocab word is included only when its freq is ≥ 2; a bigram and a slip pair only at count ≥ 2; blacklist pairs always. Rows below the threshold stay dirty in memory. O(dirty rows) memory copy only
    - `pub fn restore_dirty(&mut self, batch: DirtyBatch)` — re-marks a batch's rows dirty after a failed write, so nothing is lost
    - `pub fn merge_snapshot(&mut self, snap: Snapshot)` — adds snapshot freqs/counts onto the in-memory ones (sums), unions the blacklist, keeps the newer `last_seen`, and marks dirty ONLY rows that already existed in memory before the merge (their totals changed); rows that came only from the snapshot stay clean, so an attach never rewrites the whole database or makes `take_dirty` copy it under the lock
    - `pub fn clear(&mut self)` — drops all maps (used by Purge)
    - `pub fn learned_word_count(&self) -> usize` — distinct words with freq ≥ 2 (feeds GetStats `words_learned`)
    - `PersonalLookup::personal_freq` is the word's freq; `is_blacklisted` exact pair; `bigram_count` exact pair; `slip_count` exact (layout, typed, intended)
  - **Writer (`db.rs`), only ever called off the typing path (Task 9 runs it inside `tokio::task::spawn_blocking` with NO store lock held):**
    - `pub struct Db` (owns the `rusqlite::Connection`), `pub struct Snapshot` and `pub struct DirtyBatch` (plain row vectors), `pub enum AttachOutcome { Opened, Created, QuarantinedAndCreated(PathBuf) }`, `pub enum StoreError { NotSqlcipher, Unreadable, Io(String) }`
    - `pub fn attach(dir: &Path, key: &StoreKey) -> Result<(Db, Snapshot, AttachOutcome), StoreError>` — creates `dir` (0700) if needed; opens or creates `dir/learn.db` (0600) and runs, in this order: `PRAGMA key = "x'<hex>'"` (raw key, no KDF), then `PRAGMA cipher_version` (empty ⇒ `NotSqlcipher`: refuse to continue, the guard against silently linking plain SQLite), then `PRAGMA busy_timeout = 2000`, then a probe `SELECT count(*) FROM sqlite_master`. **Quarantine only when the probe fails with `SQLITE_NOTADB` or `SQLITE_CORRUPT`** (wrong key or a damaged file): rename the file to `learn.db.unreadable-<unix_ts>` together with any `-wal`/`-shm`, create a fresh encrypted DB and return `QuarantinedAndCreated(new_path)`. Never delete it. Any other failure (BUSY/LOCKED after the timeout, EIO, EACCES, disk full) ⇒ `Err(Io(..))` with the file untouched, and the caller retries later. Then `PRAGMA journal_mode=WAL`, `PRAGMA secure_delete=ON`, `PRAGMA temp_store=MEMORY`; prune (delete vocab words whose freq ≤ 2 and whose `last_seen` is older than 90 days; cap vocab at 200k rows and bigrams at 100k rows by lowest count); load all rows into the returned `Snapshot`
    - `pub fn try_open(dir: &Path, key: &StoreKey) -> Result<(Db, Snapshot), StoreError>` — the same open sequence as `attach`, except that it never quarantines: when `learn.db` does not exist it creates a fresh encrypted DB (like `attach`); when the probe fails with NOTADB/CORRUPT it returns `Err(Unreadable)` and leaves the file untouched. `attach` is `try_open` plus the quarantine-and-create fallback on `Unreadable`
    - `pub fn write(&mut self, batch: &DirtyBatch) -> Result<usize, StoreError>` — upserts the batch in ONE transaction and returns the row count, then chmods `-wal`/`-shm` to 0600 if present. Writes are ABSOLUTE: the batch carries current totals, so the upsert is `ON CONFLICT DO UPDATE SET freq = excluded.freq, last_seen = excluded.last_seen` (and `count = excluded.count` for bigrams), never `freq = freq + excluded.freq`
    - `pub fn purge_files(dir: &Path) -> std::io::Result<()>` — a free function that works whether or not any `Db` is open: deletes `learn.db`, `learn.db-wal`, `learn.db-shm` and every `learn.db.unreadable-*` in `dir` (missing files are not errors)
  - Schema: `vocab(word TEXT PRIMARY KEY, freq INTEGER, last_seen INTEGER)`, `slips(layout TEXT, typed TEXT, intended TEXT, count INTEGER, PRIMARY KEY(layout, typed, intended))` (layout stored as `colemak`/`qwerty`/`osktap`), `blacklist(orig TEXT, cand TEXT, PRIMARY KEY(orig, cand))` (deviation from the spec's per-layout blacklist, on purpose: a reverted pair is wrong on every layout, and a global pair is the smaller diff; a second deliberate deviation: accepted correction pairs are not persisted as rows — they surface only as GetStats counters), `bigram(w1 TEXT, w2 TEXT, count INTEGER, PRIMARY KEY(w1, w2))`

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn cipher_version_present() { /* attach to a temp dir ⇒ Ok; a test helper reading PRAGMA cipher_version on the Db returns non-empty */ }
#[test] fn file_is_encrypted() { /* cache: record "gstack" twice; db.write(&cache.take_dirty()); raw bytes of learn.db (and -wal if present) contain no b"gstack" and do not start with b"SQLite format 3\0" */ }
#[test] fn roundtrip_across_reopen() { /* key K: write a batch with "gstack" freq 2; drop Db; attach(K) ⇒ snapshot merged into memory() gives personal_freq("gstack") == 2 */ }
#[test] fn first_sighting_not_taken() { /* record_word once ⇒ take_dirty() has no vocab rows; record again ⇒ it does */ }
#[test] fn slips_roundtrip() { /* record_slip(Colemak,'n','e') once ⇒ not in take_dirty(); twice ⇒ in it; written, reopened ⇒ slip_count == 2; slip_count(Qwerty,'n','e') == 0 */ }
#[test] fn blacklist_and_bigram_thresholds() { /* one blacklist record is taken immediately; a bigram only at count >= 2 */ }
#[test] fn failed_write_restores_dirty() { /* take_dirty, then restore_dirty ⇒ the next take_dirty returns the same rows */ }
#[test] fn try_open_wrong_key_leaves_file() { /* create with K1; try_open(K2) ⇒ Err(Unreadable); learn.db unchanged (same bytes); try_open(K1) ⇒ Ok */ }
#[test] fn wrong_key_quarantines() { /* create with K1; attach(K2) ⇒ QuarantinedAndCreated(p); p exists; the fresh learn.db opens with K2 */ }
#[test] fn corrupt_file_quarantines() { /* garbage bytes as learn.db ⇒ QuarantinedAndCreated */ }
#[test] fn busy_file_not_quarantined() { /* another connection holds BEGIN EXCLUSIVE on learn.db ⇒ attach returns Err(Io(_)) after the busy timeout; learn.db is still present and unrenamed */ }
#[test] fn merge_snapshot_sums() { /* memory(): record x2; merge a snapshot holding freq 3 ⇒ personal_freq == 5; a snapshot-only word is not in the next take_dirty() */ }
#[test] fn rewrite_is_absolute() { /* write freq 2; record once more; write take_dirty() again; reopen ⇒ personal_freq == 3, not 5 */ }
#[test] fn permissions_restrictive() { /* after attach + write: dir mode 0700, learn.db mode 0600 */ }
#[test] fn purge_files_unconditional() { /* with learn.db, -wal and learn.db.unreadable-123 present and NO Db open ⇒ purge_files removes all of them */ }
#[test] fn key_hex_roundtrip() { /* StoreKey::from_hex(&k.to_hex()) is Some; from_hex("zz") is None; two generate() calls differ */ }
```

- [ ] **Step 2: Run to verify failure** — expected: compile error
- [ ] **Step 3: Implement `key.rs`, `learning.rs` and `db.rs`** — apply `PRAGMA key` before any other statement on the connection; distinguish NOTADB/CORRUPT from other errors via `rusqlite::Error::SqliteFailure(e, _)` and `e.code` (`ErrorCode::NotADatabase`, `ErrorCode::DatabaseCorrupt`); set 0600 via `std::os::unix::fs::PermissionsExt` right after creating `learn.db` (do not use `umask`: it is process-wide and races with parallel tests; the 0700 directory already keeps other users out)
- [ ] **Step 4: Run tests** — expected: PASS
- [ ] **Step 5: Benchmark** — `Db::write` with a 1,000-row batch; record median and worst in the commit message. Gate: worst < 50 ms on this machine (it has ARMv8 AES/SHA2/SHA512 instructions, which OpenSSL uses automatically). This cost never reaches typing: the typing path only takes the store lock for `take_dirty`/`merge_snapshot`/`restore_dirty`, which are in-memory copies
- [ ] **Step 6: Commit** — `git commit -am "feat: in-memory learning cache and separate sqlcipher writer"`

### Task 7: Engine state machine (pure logic)

**Files:**
- Create: `engine/Cargo.toml` (depends on `corelib`; uncomment workspace member), `engine/src/main.rs` (stub `fn main() {}`), `engine/src/state.rs`
- Test: inline `#[cfg(test)]` in `state.rs`

**Interfaces:**
- Consumes: `KeyClass`, `WordBuffer` (Task 1), `Dictionary` (Task 3), `decide` (Task 4), `Mode`, `ProseRun`, `terminal_token_ok`, `is_learnable`, `bigram_learnable`, `effective_layout` (Task 5), `LearnStore`, `SharedStore` (Task 6)
- Produces:
  - `pub enum Action { Pass, Replace { delete_chars: u32, commit: String, consume: bool } }` — `Pass` = return the event unhandled; `Replace` = delete `delete_chars` then commit `commit`, with `consume` saying whether the triggering key is swallowed (true when its character is already inside `commit`; false when the real key must still reach the app, i.e. `Boundary(None)`)
  - `pub struct Counters { pub corrections: std::sync::atomic::AtomicU64, pub reverts: std::sync::atomic::AtomicU64 }` (with `Default`); `on_key` increments `corrections` for each correcting Replace and `reverts` for each revert Replace
  - `pub struct EngineState::new(dict: std::sync::Arc<Dictionary>, store: SharedStore, counters: std::sync::Arc<Counters>, variant: LayoutId)` — both layout variants share ONE dictionary (its delete index is the largest allocation in the process), ONE store and ONE `Counters`. Lock the store mutex (`lock().unwrap_or_else(|e| e.into_inner())`) only for the duration of a single `decide` or `record_*` call, never across a D-Bus emit
  - `pub fn on_key(&mut self, k: KeyClass, mode: Mode, osk_visible: bool, surrounding: Option<&str>) -> Action` — `surrounding` is the text before the cursor when the client provides it (None otherwise); used for verify-before-delete
  - `pub fn on_invalidate(&mut self)` — clears word buffer, prose-run, pending revert window and the `LangWindow`
  - Language context (eng review D4): `EngineState` owns one `LangWindow` (sized to the dictionary's language count). Each `decide` call passes `lang_window.ctx()`. After every boundary, if the final committed word is in the dictionary, `push(dict.primary_lang(word))`; words not in any lexicon (including personal vocabulary) push nothing. `FocusIn` resets the window through `on_invalidate`
  - Pinned behaviors (each is a test):
    - **Trailing-period strip:** at a boundary the candidate token is the buffer minus any trailing run of `.` (k ≥ 0 periods); if the remaining token contains any non-alphabetic char (interior `.`, `/`, `-`, digits, ...) ⇒ no correction. A corrected commit re-appends the stripped periods. So `file.txt` is never touched while `bulid.` still corrects.
    - **Boundary(Some(c))** in Silent mode with a confident `decide` ⇒ `Replace { delete_chars: buffered_len, commit: fixed + "."*k + c, consume: true }`, only when `surrounding` is None or ends with the buffered text; mismatch ⇒ `Pass` and record nothing.
    - **Boundary(None)** (Return/Tab — deliberate deviation from the spec's plain "space/punctuation" boundary, so Enter still submits and Tab still navigates). In Silent mode: `Replace { delete_chars: buffered_len, commit: fixed + "."*k, consume: false }` — no boundary char in the commit, and the real key reaches the app after the commit. A Boundary(None) correction opens **NO revert window** (the key that follows the commit — newline/tab, possibly a form submit — sits between the corrected word and the cursor, so a revert delete would hit the wrong span and a failed verify would spuriously blacklist a good pair; blacklist learning for this case is forfeited in v1).
    - **Revert:** after a Boundary(Some) Replace, the very next `Backspace` ⇒ `Replace { delete_chars: committed_len, commit: original_as_committed, consume: true }`, where `original_as_committed` is the original buffer text with the SAME stripped periods and boundary char re-appended that the correction committed, and `committed_len` is the char length of the CORRECTION'S commit (`fixed + "."*k + c` — what must be deleted; it differs from `original_as_committed`'s length when the correction changed the word's length) — the revert must not eat the user's space. Same verify rule as correction: surrounding, when available, must end with the corrected commit; mismatch ⇒ `Pass` but the blacklist pair is still recorded.
    - Mid-word Backspace pops the buffer (`Pass`); Backspace with empty buffer ⇒ `Pass` + invalidate.
    - **Slip learning (eng review D8):** remember the char returned by a mid-word `pop()`. If the very next key is `Printable(c)` with `c` different from the removed char (case-insensitive, both ASCII letters), call `record_slip(effective_layout(variant, osk_visible), removed, c)` — only in `Silent` and `Observe`, never in `Off` or `ProseAware`. Any other key, a second Backspace (which keeps the most recently removed char), a boundary or `on_invalidate` clears the remembered char without recording.
    - ProseAware mode additionally requires `prose_run.ready() && terminal_token_ok(token)`; in ProseAware mode ANY `Boundary(None)` resets the prose-run and NEVER triggers a correction (the state machine cannot and need not distinguish Tab from Return; correcting at Enter would rewrite a command the instant it executes). Learning still records the completed word per the mode rules below.
    - Boundary with empty buffer ⇒ `Pass`, no `decide` call.
    - **Learning is gated by `is_learnable`:** at each boundary in Silent/ProseAware/Observe, `record_word` records the word as finally committed (the original token when no correction fired, including unknown jargon; the corrected word when one did) only when `is_learnable(token, mode)` holds (so nothing is ever learned in ProseAware or Off). `bump_bigram(prev, word)` fires only when `bigram_learnable(mode)` and both words passed `is_learnable`. In `Mode::Off`, NOTHING is recorded: secrets must never reach the store.
    - Cursor-location invalidation arrives via `on_invalidate` (wired in Task 8's `SetCursorLocation`); it drops the pending word, prose-run, and revert window.

- [ ] **Step 1: Write failing tests** (drive with synthetic `KeyClass` sequences; stub dict from Task 3 fixtures; temp store)

```rust
#[test] fn silent_fix_at_boundary() { /* type b,u,l,i,d then Boundary(Some(' ')) with surrounding Some("bulid") ⇒ Replace{5, "build ", consume:true} */ }
#[test] fn verify_mismatch_passes() { /* same but surrounding Some("xxxxx") ⇒ Pass */ }
#[test] fn trailing_period_stripped() { /* "bulid." + Boundary(Some(' ')) ⇒ Replace{6, "build. ", consume:true}; "file.txt" + boundary ⇒ Pass */ }
#[test] fn return_boundary_commits_without_char() { /* "bulid" + Boundary(None) ⇒ Replace{5, "build", consume:false} */ }
#[test] fn no_revert_window_after_charless_boundary() { /* after that, immediate Backspace ⇒ Pass; nothing blacklisted */ }
#[test] fn revert_consumes_and_restores() { /* after silent_fix, Backspace with surrounding Some("build ") ⇒ Replace{6, "bulid ", consume:true}; store.is_blacklisted("bulid","build") */ }
#[test] fn revert_window_closes_on_next_key() { /* after fix, Printable('x') then Backspace ⇒ Pass (normal) */ }
#[test] fn midword_backspace_pops() { /* b,u,l,i,Backspace,d + boundary ⇒ decide sees "buld" */ }
#[test] fn empty_boundary_noop() { /* Boundary with empty buffer ⇒ Pass, no decide */ }
#[test] fn focus_invalidate_drops_pending() { /* mid-word on_invalidate(); boundary ⇒ Pass */ }
#[test] fn prose_aware_gates() { /* ProseAware: first two words pass untouched (counter), third typo'd word corrected; after Invalidate counter resets */ }
#[test] fn observe_never_acts() { /* Mode::Observe ⇒ always Pass, but record_word still fires at boundaries */ }
#[test] fn off_mode_records_nothing() { /* Mode::Off ⇒ always Pass AND no record_word/bump_bigram calls at boundaries */ }
#[test] fn jargon_learned_in_two_sightings() { /* unknown "gstack" completed twice ⇒ personal_freq("gstack") == 2 */ }
#[test] fn secret_shaped_tokens_not_learned() { /* Silent: "hunter2", "KEY=abc123", "sk-abc" + boundary ⇒ personal_freq of each == 0 */ }
#[test] fn terminal_prompt_token_not_learned() { /* ProseAware: lone "swordfish" + Boundary(None) (prose not ready) ⇒ personal_freq == 0 */ }
#[test] fn no_learning_in_terminal() { /* ProseAware with a ready prose-run: two clean words ⇒ personal_freq of each == 0 and bigram_count == 0 */ }
#[test] fn variants_share_store() { /* two EngineState built from the same SharedStore: a word learned via one is visible via the other */ }
#[test] fn filipino_sentence_blocks_english_fix() { /* fixture lexicons from Task 3; type "ako", "kami" (known TL) then "bulid" + boundary ⇒ Pass (dominant TL, no Tagalog candidate) */ }
#[test] fn waray_sentence_fixes_waray_typo() { /* type "maupay" twice (known WAR) then "maupy" ⇒ Replace to "maupay " */ }
#[test] fn invalidate_resets_language() { /* after TL words, on_invalidate(), then "bulid" ⇒ fixed to "build " via ALL with margin 0.6 */ }
#[test] fn backspace_fix_records_slip() { /* Silent, Colemak: b,n,Backspace,e,d ⇒ slip_count(Colemak,'n','e') == 1; Off mode same keys ⇒ 0; OSK visible ⇒ recorded under OskTap */ }
#[test] fn counters_track_fixes_and_reverts() { /* silent_fix then revert ⇒ corrections == 1, reverts == 1 */ }
```
(Construct test stores with `Arc::new(Mutex::new(LearnStore::memory()))` and counters with `Arc::new(Counters::default())`; no disk or keyring is needed for this task.)

- [ ] **Step 2: Run to verify failure** — `cargo test -p engine` — expected: compile error
- [ ] **Step 3: Implement `state.rs`**
- [ ] **Step 4: Run tests** — expected: PASS
- [ ] **Step 5: Commit** — `git commit -am "feat: engine state machine with revert and verify-before-delete"`

### Task 8: IBus engine binding (pass-through milestone)

**Files:**
- Create: `engine/src/ibus.rs`, `engine/src/config.rs`, `data/correctiond.xml`, `scripts/install-dev.sh`, `scripts/gen-ibus-golden.py`, `engine/tests/golden/ibustext_build.json`
- Modify: `engine/src/main.rs`, `engine/Cargo.toml` (add `zbus = { version = "5", default-features = false, features = ["tokio"] }` — without `default-features = false` zbus 5 also pulls in async-io; `tokio` with `rt-multi-thread`, `macros`, `time`, `signal` and `sync`; `libc`; `toml` and `serde` with `derive` for `config.rs`. Task 9's keyring client runs on the same runtime)
- Test: golden unit test for the IBusText value (eng review D9); manual verification for the live D-Bus surface

**Interfaces:**
- Consumes: `EngineState`, `Counters` (Task 7), `Dictionary::from_lexicons`, `parse_lexicon`, `LangId` (Task 3), `LearnStore::memory`, `SharedStore` (Task 6), `resolve_mode`, `Ctx` (Task 5) — not yet wired to corrections; this task is observe/pass-through only
- Produces:
  - `engine/src/config.rs`: `pub struct Config { pub languages: Vec<LangSpec>, pub allowlist: Vec<String>, pub broken: Vec<String>, pub auto_switch_osk: bool, pub allow_any_caller: bool }` and `pub struct LangSpec { pub code: String, pub lexicon: String }`, parsed with the `toml` crate from `~/.config/correctiond/config.toml`; a missing or invalid file ⇒ defaults: `languages = [{code="en", lexicon="builtin:en"}, {code="tl", lexicon="~/.local/share/correctiond/lexicons/tl.txt"}, {code="war", lexicon="~/.local/share/correctiond/lexicons/war.txt"}]`, `allowlist = ["firefox"]`, `broken = []`, `auto_switch_osk = true`, `allow_any_caller = false`. Languages are data, not code (eng review D4): adding a language is one lexicon file plus one `[[languages]]` entry; more than 16 entries ⇒ the extras are ignored with a warning. Unit tests: defaults when the file is absent; a custom `[[languages]]` list round-trips; `~` expands to `$HOME`
  - Process setup in `main`, in this order: the hardening calls (Step 2); load `Config` (below); build ONE `Arc<Dictionary>` inside `tokio::task::spawn_blocking` from the configured languages in order — `builtin:en` is `include_str!("../../data/en_words.txt")` (embedding means the `.deb` needs no data file), other entries are lexicon files read from disk; a missing or unreadable lexicon file is skipped with a one-line warning (no words) and never stops startup; create ONE `SharedStore` from `LearnStore::memory()` and ONE `Arc<Counters>`; only THEN connect to IBus and `RequestName`, so every `CreateEngine` gets a ready dictionary. Log the dictionary build time in ms and `index_bytes()` in MiB (numbers only); if it exceeds 2 s on this machine, record that in `docs/designs/notes.md` as a follow-up (serializing the delete index at build time)
  - `org.freedesktop.IBus.Engine` D-Bus implementation: handles `ProcessKeyEvent(keyval, keycode, state) -> bool` (always returns `false` this task, feeding `classify_key` into `EngineState` with the mode from `resolve_mode` given the last content type, `wm_class: None` and an empty allowlist — that yields `Off` for sensitive fields and `Observe` elsewhere, so nothing typed into a password field is learned even before Task 10), `FocusIn`, `FocusOut`, `Reset`, `Enable`, `Disable`, `SetContentType(purpose, hints)`, `SetSurroundingText(text, cursor_pos, anchor_pos)`, `SetCursorLocation(x, y, w, h)` (calls `EngineState::on_invalidate` unless it arrives immediately after the engine's own commit — track a just-committed flag; this is the spec's "cursor change not caused by our own commits" rule); requests surrounding text once on `Enable` (warm-up, spec constraint)
  - `data/correctiond.xml`: component `org.freedesktop.IBus.Correctiond` with ONE component-level `<exec>` (no arguments — a component runs one process) and TWO `<engine>` entries: `correctiond:colemak` (`layout="us(colemak)"`) and `correctiond:us` (`layout="us"`). The variant is selected at `Factory.CreateEngine(name)` time: the name argument maps `correctiond:colemak` ⇒ `LayoutId::Colemak`, `correctiond:us` ⇒ `LayoutId::Qwerty` when constructing that engine instance's `EngineState` from the shared dictionary, store and counters — one process serves both variants (this is how Super+Space switches without a restart)
  - `scripts/install-dev.sh`: copies the XML to `/usr/share/ibus/component/` (sudo), binary path absolute to `target/release/engine`, then `ibus restart`
  - **Reference (eng review D2):** while writing this task, read librush 0.2.3 (`github.com/fm-elpac/librush`, `src/ibus/engine.rs`, `src/ibus/factory.rs`; pure-Rust IBus module on zbus 5) for the Engine/Factory interface shapes and signal signatures. Do not add it as a dependency and do not copy its code verbatim (LGPL-2.1+/GPL-3.0+). The authoritative IBusText wire form, captured on this machine with python-gi, is `('IBusText', @a{sv} {}, '<text>', <('IBusAttrList', @a{sv} {}, @av [])>)` (signature `(sa{sv}sv)`)
  - Connection bootstrap: read the IBus address from `IBUS_ADDRESS` env or `~/.config/ibus/bus/*`, connect with zbus, and `RequestName` for the component's bus name from the XML — with the XML installed, ibus-daemon execs this binary and expects the name; call `RegisterComponent` ONLY when started with the explicit `--register` flag (dev loop and the Task 10 E2E harness; eng review D10), which registers the same two engines at runtime without an installed XML. Serve `org.freedesktop.IBus.Factory.CreateEngine` returning the engine object path. Note: on the engine interface, `CommitText` and `DeleteSurroundingText` are emitted D-Bus SIGNALS whose text argument is a serialized IBusText variant, not plain-string method calls — crib the wire format from the ibus source (`src/ibustext.c` serialization) or by `busctl --user monitor`-ing an existing engine such as ibus-typing-booster

- [ ] **Step 1: Write `data/correctiond.xml`** with the two engine variants as specified
- [ ] **Step 1b: Golden test for IBusText (eng review D9)** — write `scripts/gen-ibus-golden.py`: with python-gi, build `IBus.Text.new_from_string("build ")`, call `serialize_object()`, and write JSON `{"signature": "(sa{sv}sv)", "fields": ["IBusText", {}, "build ", {"signature": "(sa{sv}av)", "fields": ["IBusAttrList", {}, []]}]}` by walking the GVariant (signature from `get_type_string()`, values recursively); run it once and commit `engine/tests/golden/ibustext_build.json`. Then write the failing Rust test `ibustext_matches_golden`: build our `ibus_text("build ")` zvariant value and assert its signature and nested field values equal the golden JSON. Run `cargo test -p engine ibustext_matches_golden` — FAIL until `ibus.rs` implements `pub fn ibus_text(s: &str) -> zbus::zvariant::Value<'static>`, then PASS
- [ ] **Step 2: Implement the zbus Factory + Engine skeleton** — `ProcessKeyEvent` returns false always. The first lines of `main` call `libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0)` and `setrlimit(RLIMIT_CORE, 0)`, so crash reports cannot capture keystroke buffers or keys. Tracing is opt-in only: when `CORRECTIOND_TRACE=1` is set, append one line per event to `~/.local/state/correctiond/trace.log` (created 0600, truncated once it passes 1 MiB) containing ONLY the `KeyClass` variant name (`Printable`, `Boundary`, `Backspace`, `Invalidate`, never the character), the purpose/hints numbers, and lifecycle calls. Write nothing at all while the resolved mode is `Off`. Without the variable, log only errors, and never with typed content. Never write per-key output to stderr: ibus-daemon's stderr goes to the journal.
- [ ] **Step 3: Build** — `cargo build --release` — expected: compiles
- [ ] **Step 4: Manual verification** — run `scripts/install-dev.sh`; `ibus engine correctiond:colemak`; type in gnome-text-editor and Ptyxis with `CORRECTIOND_TRACE=1` set for the dev session. Expected: typing behaves exactly as plain Colemak (zero behavior change, which also confirms `PR_SET_DUMPABLE=0` breaks nothing); `ibus engine` prints `correctiond:colemak`; `trace.log` shows key classes with no characters, and Ptyxis shows purpose=TERMINAL (record Open Question 2's answer in `docs/designs/notes.md`; if Ptyxis does NOT report TERMINAL, then until Task 11 supplies wm_class it resolves to Observe and learns word-shaped tokens, so avoid typing passphrases in Ptyxis on dev builds between Tasks 9 and 11); `journalctl --user -b | grep -c correctiond` shows no per-key lines; `stat -c %U /proc/$(pgrep -f 'correctiond|target/release/engine')/mem` prints `root` (the visible effect of `PR_SET_DUMPABLE=0`). Switch to `correctiond:us` via Super+Space and verify QWERTY typing. Unset `CORRECTIOND_TRACE` afterwards and delete `trace.log`.
- [ ] **Step 5: Run full test suite** — `cargo test` — expected: PASS (no regressions)
- [ ] **Step 6: Commit** — `git commit -am "feat: ibus engine registration, two layout variants, pass-through"`

### Task 9: Store key custody via GNOME Keyring + the persistence task

**Files:**
- Create: `engine/src/keyring.rs`, `engine/src/persist.rs`
- Modify: `engine/Cargo.toml` (add `oo7 = { version = "0.6", default-features = false, features = ["tokio", "openssl_crypto"] }` — oo7's defaults are `tokio` + `native_crypto`, and enabling both crypto backends is a compile error, so `default-features = false` is required; oo7 0.6 depends on zbus 5.12+, matching Task 8's zbus 5; add tokio `test-util` under `[dev-dependencies]`), `engine/src/main.rs`, `engine/src/ibus.rs` (lifecycle wiring below)
- Test: unit tests in `keyring.rs` and `persist.rs` against a fake backend; one `#[ignore]` live-keyring test; manual verification

**Interfaces:**
- Consumes: `StoreKey`, `Db` (`try_open`, `attach`, `write`), `Snapshot`, `DirtyBatch`, `AttachOutcome`, `StoreError`, `purge_files`, `LearnStore::{take_dirty, restore_dirty, merge_snapshot, set_persistent, clear}`, `SharedStore` (Task 6); the tokio runtime, the shared `SharedStore` and the IBus engine object (Task 8)
- Produces:
  - `keyring.rs`:
    - `#[async_trait::async_trait] pub trait SecretBackend: Send + Sync { async fn locked(&self) -> Result<bool, String>; async fn find(&self, attrs: &HashMap<&'static str, String>) -> Result<Vec<zeroize::Zeroizing<Vec<u8>>>, String>; async fn create(&self, attrs: &HashMap<&'static str, String>, secret: &[u8]) -> Result<(), String>; async fn delete(&self, attrs: &HashMap<&'static str, String>) -> Result<(), String>; }` — use the `async-trait` crate (add it to `engine/Cargo.toml`): plain `async fn` in a trait does not produce `Send` futures, and the actor runs under `tokio::spawn`, which requires them
    - `pub fn key_attributes(purpose: &str) -> HashMap<&'static str, String>` ⇒ exactly `{"xdg:schema": "org.nadelacruz.Correctiond.StoreKey", "purpose": purpose, "version": "1"}`
    - `pub struct Oo7Backend` implementing `SecretBackend` with these exact oo7 0.6 `dbus` calls: `oo7::dbus::Service::new().await`, then `service.with_alias("default").await` — `None` ⇒ `Err("no default collection")`. **NEVER call `default_collection()`**: it falls back to `with_alias_or_create`, which makes gnome-keyring show a new-keyring password dialog. `locked()` = `collection.is_locked()`; `find` = `search_items` and then `item.secret()` per item; `create` = `create_item(label "Correctiond learning store key", attrs, secret, replace = false, None)`; `delete` = `item.delete(None)` per item found. Never call `unlock` or `create_collection`. `create_item` and `delete` are only ever called right after `locked()` returned `false` in the same attempt. A residual race remains: if the user locks the keyring in the milliseconds between the check and the call, gnome-keyring may show its own unlock dialog. That is accepted.
    - `pub enum KeyState { Ready(Vec<StoreKey>), Locked, Unavailable(String) }` — `Ready` carries every candidate key found (normally one); the reason `String` never contains secret material
    - `pub async fn fetch_or_create_key_with<B: SecretBackend>(b: &B, purpose: &str) -> KeyState` — `locked()` true ⇒ `Locked` (no other call). Otherwise `find`: one or more items ⇒ `Ready(keys)`, parsing each with `StoreKey::from_hex` and skipping unparsable ones (all unparsable ⇒ `Unavailable("malformed key item")`, and never overwrite it). NEVER create when `find` returned any item. Zero items ⇒ re-check `locked()`, then `StoreKey::generate()`, `create`, return `Ready(vec![key])`. Any backend error ⇒ `Unavailable`
    - `pub async fn delete_key_with<B: SecretBackend>(b: &B, purpose: &str) -> Result<(), String>` — `locked()` true ⇒ `Err("locked")` with no call issued; else `delete`
  - `persist.rs` — ONE actor task owns all keyring and database work, so attach, flush and purge can never interleave:
    - `pub fn spawn_persistence_with<B: SecretBackend + 'static>(store: SharedStore, dir: PathBuf, backend: B) -> PersistHandle`; `pub fn spawn_persistence(store, dir) -> PersistHandle` uses `Oo7Backend`
    - `PersistHandle` (cheap `Clone`, wraps a `tokio::sync::mpsc` sender + `tokio::sync::Notify` + an `Arc` of stats): `pub fn nudge(&self)` (non-blocking; wakes the actor's backoff sleep — it never starts a parallel attempt); `pub async fn flush_now(&self) -> Result<usize, String>` (awaits the write); `pub async fn purge(&self) -> Result<(), String>`; `pub fn stats(&self) -> PersistStats` with `pub struct PersistStats { pub persistent: bool, pub quarantined_files: u32, pub key_delete_pending: bool }`
    - Actor states, with the **invariant: whenever the store is not persistent, the actor is in Acquire**. In EVERY state the actor `tokio::select!`s over its mpsc receiver, the `Notify` and its current timer, so requests are never stuck behind a backoff sleep: `purge` is handled immediately in any state, and `flush_now` in Acquire replies `Ok(0)` at once
      - **Pending-delete marker:** `key_delete_pending` is backed by a non-secret marker file `dir/key-delete-pending` (0600, empty). It is written BEFORE `purge` returns its error and removed only after a successful `delete_key_with`; at startup the actor sets the flag if the marker exists. So a logout or `ibus restart` before unlocking can never resurrect the forgotten key
      - **Acquire:** if `key_delete_pending`, first try `delete_key_with`; on success clear the flag, on failure stay in Acquire. Then `fetch_or_create_key_with(.., "learn-db-key")`; `Locked`/`Unavailable` ⇒ sleep with backoff 1, 2, 4 … 60 s (woken early by `nudge`). `Ready(keys)` ⇒ inside `tokio::task::spawn_blocking`, with NO store lock held: for each key, `Db::try_open(&dir, &key)`; the first `Ok` wins. If every key returns `Err(Unreadable)`, call `Db::attach(&dir, &keys[0])`, which quarantines and creates (bump `quarantined_files`). On success: lock the store briefly, `merge_snapshot`, `set_persistent(true)`, unlock; drop all keys. If more than one key item exists, keep them all in v1: `delete` matches by attributes and would remove the winning key too, and trying every key on each start is harmless. `Err(Io)` ⇒ stay in Acquire and back off
      - **Persistent:** every 30 s and on `flush_now`: lock briefly, `take_dirty`, unlock; run `db.write(&batch)` in `spawn_blocking`; on error lock, `restore_dirty(batch)`, unlock, and report the error to a waiting `flush_now` caller
      - **Purge** (any state): drop the `Db`; lock, `clear()`, `set_persistent(false)`, unlock; `purge_files(&dir)` in `spawn_blocking`; then `delete_key_with`. If the delete fails (for example the keyring is locked), set `key_delete_pending = true` and return `Err("learned data deleted; keyring item will be removed when the keyring is unlocked")`. Either way, go to Acquire. Because of `key_delete_pending`, Acquire deletes the old item before it can ever find and reuse it
    - The key is dropped after attach; the actor keeps no copy
  - Lifecycle wiring in `ibus.rs`/`main.rs`: `FocusIn` while `!stats().persistent` ⇒ `nudge()`; `FocusOut` and `Disable` ⇒ `flush_now()` fire-and-forget (spawned, not awaited, so the D-Bus handler returns at once); SIGTERM/SIGINT (tokio `signal`) and loss of the IBus connection ⇒ await `flush_now()` with a 2 s timeout, then exit

- [ ] **Step 1: Write failing tests** (all `#[tokio::test(start_paused = true)]`, with a `FakeBackend` that records every call)

```rust
#[test] fn attributes_are_stable() { /* key_attributes("learn-db-key") equals exactly the three pairs above */ }
#[tokio::test(start_paused = true)] async fn locked_issues_no_other_calls() { /* fake locked=true ⇒ Locked; the call log holds only `locked` */ }
#[tokio::test(start_paused = true)] async fn never_creates_when_item_exists() { /* fake find returns one non-hex secret ⇒ Unavailable; the log has no `create` */ }
#[tokio::test(start_paused = true)] async fn locked_stays_memory_then_attaches() { /* fake locked for 2 attempts, then unlocked with no items: the store is non-persistent with no attach, then persistent after advancing time past the backoff; learn.db exists */ }
#[tokio::test(start_paused = true)] async fn nudge_does_not_double_create() { /* nudge() 5x while Acquire is mid-attempt ⇒ exactly one `create` in the log */ }
#[tokio::test(start_paused = true)] async fn flush_writes_second_sightings() { /* persistent; record "gstack" twice; flush_now() == Ok(n >= 1); reopen with the same key ⇒ it is there */ }
#[tokio::test(start_paused = true)] async fn purge_when_locked_marks_pending() { /* persistent, then fake becomes locked; purge() ⇒ Err; learn.db gone; stats().key_delete_pending; after unlock + nudge, the log shows `delete` BEFORE any `find`/`create` */ }
#[tokio::test(start_paused = true)] async fn pending_delete_survives_restart() { /* purge while locked; drop the actor; spawn_persistence_with again on the same dir with the fake now unlocked ⇒ the log shows `delete` before any `find`/`create`, and the marker file is gone afterwards */ }
#[tokio::test(start_paused = true)] async fn requests_not_blocked_by_backoff() { /* Acquire with a locked fake and a 60 s backoff pending: purge() and flush_now() (== Ok(0)) both complete without advancing time */ }
#[tokio::test(start_paused = true)] async fn purge_returns_to_acquire() { /* after a successful purge, the store becomes persistent again under a NEW key (the fake's create was called a second time) */ }
#[tokio::test(start_paused = true)] async fn io_error_does_not_quarantine() { /* dir made read-only ⇒ attach Err(Io); no learn.db.unreadable-* appears; the actor stays in Acquire */ }
#[tokio::test] #[ignore] async fn live_keyring_roundtrip() { /* Oo7Backend, purpose "test-key": fetch twice ⇒ same key hex; delete_key_with ⇒ Ok. Run manually: cargo test -p engine live_keyring -- --ignored */ }
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p engine` — expected: compile error
- [ ] **Step 3: Implement `keyring.rs` and `persist.rs`; in `main`, start `spawn_persistence(store.clone(), dir)` with the single `SharedStore` from Task 8, and wire the lifecycle calls in `ibus.rs`**
- [ ] **Step 4: Run tests** — `cargo test -p engine` then `cargo test -p engine live_keyring -- --ignored` — expected: PASS
- [ ] **Step 5: Manual verification** — restart ibus; type a few words twice in gnome-text-editor (still pass-through until Task 10), then switch focus to flush. Expected: `secret-tool search purpose learn-db-key` (package `libsecret-tools`) shows exactly one item; `ls -l ~/.local/share/correctiond/` shows `learn.db` 0600 in a 0700 dir; `head -c 16 ~/.local/share/correctiond/learn.db | xxd` is NOT `SQLite format 3`. Lock the login keyring in Seahorse, `ibus restart`, and type: typing works and NO prompt of any kind appears. Unlock it and change focus: the store attaches within seconds.
- [ ] **Step 6: Commit** — `git commit -am "feat: keyring-held store key, never prompt, single persistence actor"`

### Task 10: Control plane + live corrections

**Files:**
- Create: `engine/src/control.rs`
- Modify: `engine/src/ibus.rs` (wire `EngineState` actions to `DeleteSurroundingText` + `CommitText`), `engine/src/main.rs`
- Create (eng review D10): `scripts/e2e.sh`, `tests/e2e/ibus_roundtrip.py`
- Test: unit tests for allowlist/mode plumbing in `control.rs`; headless E2E through a real ibus-daemon; manual verification for live correction

**Interfaces:**
- Consumes: `resolve_mode`, `Ctx` (Task 5), `EngineState::on_key`, `Counters` (Task 7), `SharedStore`, `LearnStore::learned_word_count` (Task 6), `PersistHandle::{purge, stats}` (Task 9)
- Produces:
  - D-Bus service `org.nadelacruz.Correctiond1` on the session bus, object `/org/nadelacruz/Correctiond`, methods: `SetFocusedApp(s wm_class)` (empty string ⇒ `wm_class = None`, unidentified), `SetOskVisible(b)`, `SetEnabled(b)`, `Purge()` (calls ONLY `PersistHandle::purge().await`, which serializes the deletion inside the persistence actor; its `Err` becomes a D-Bus error `org.nadelacruz.Correctiond1.Error.PurgePartial` carrying the message, so the user learns that the keyring item is still pending deletion), `GetStats() -> (s json)` (counts and booleans only, never words): `corrections`/`reverts` from `Counters`, `words_learned` = `learned_word_count()`, and `persistent`, `quarantined_files`, `key_delete_pending` from `PersistHandle::stats()`
  - **Caller check:** `SetFocusedApp`, `SetOskVisible`, `SetEnabled` and `Purge` are accepted only when the message sender's unique name equals the current owner of `org.gnome.Shell`. Get it via `org.freedesktop.DBus.GetNameOwner`, cache it, and refresh it on `NameOwnerChanged`. Other callers get `org.freedesktop.DBus.Error.AccessDenied`. `GetStats` is open to any caller. The config key `allow_any_caller = false` (default) exists ONLY for the manual busctl verification below
  - Config: uses `Config` from Task 8 (`allowlist`, `broken`, `auto_switch_osk`, `allow_any_caller`; read at startup; GSettings migration is post-v1 — record as deviation from spec in commit message)
  - `ProcessKeyEvent` now: builds `Ctx` from last `SetContentType` + control-plane state, resolves `Mode`, calls `on_key`; `Action::Replace` ⇒ emit `DeleteSurroundingText(-(delete_chars as i32), delete_chars)` then `CommitText`, and ProcessKeyEvent returns the action's `consume` flag — true when the triggering key's character is inside the commit (returning false there would hand the app the boundary key a second time and produce "build  "), false for Return/Tab so the real key still reaches the app; `Action::Pass` ⇒ return false. The whole path is wrapped so any internal error returns false (Global Constraints: never break typing). The store is the single `SharedStore` created in Task 9 (memory first, attached to `~/.local/share/correctiond/` when the key is available); nothing in this task opens the database or touches the keyring.

- [ ] **Step 1: Write failing unit tests for the plumbing** (no D-Bus: test `Ctx` assembly + mode resolution given control-plane state)

```rust
#[test] fn firefox_allowlisted_silent() { /* wm_class "firefox", purpose FREE_FORM ⇒ Silent */ }
#[test] fn password_in_firefox_off() { /* purpose PASSWORD ⇒ Off (no correction, no learning) */ }
#[test] fn no_wmclass_observes() { /* ⇒ Observe */ }
#[test] fn non_shell_caller_denied() { /* caller-check helper: sender ":1.99" with shell owner ":1.12" ⇒ AccessDenied for SetFocusedApp and Purge; ":1.12" ⇒ allowed; allow_any_caller=true ⇒ allowed */ }
#[test] fn stats_contain_no_words() { /* after learning "gstack" twice, the GetStats JSON contains no "gstack" substring, words_learned == 1, and it has keys persistent, quarantined_files, key_delete_pending */ }
```

- [ ] **Step 2: Run to verify failure** — expected: compile error
- [ ] **Step 3: Implement `control.rs` and wire corrections in `ibus.rs`**
- [ ] **Step 4: Run tests** — `cargo test` — expected: PASS
- [ ] **Step 4b: Headless E2E (eng review D10)** — write `scripts/e2e.sh`: `dbus-run-session --` a subshell that sets `HOME` and `XDG_*` to a temp dir, writes `$HOME/.config/correctiond/config.toml` with `allowlist = ["e2e"]` and `allow_any_caller = true`, starts `ibus-daemon --single --panel=disable --config=disable --address=unix:path=$TMP/ibus` in the background, exports `IBUS_ADDRESS`, starts `target/release/engine --register`, then runs `python3 tests/e2e/ibus_roundtrip.py` and kills both daemons on exit (trap). `ibus_roundtrip.py` (python-gi): connect `IBus.Bus.new_async_client` or `IBus.Bus()`, create an input context with capabilities FOCUS|SURROUNDING_TEXT, `focus_in()`, `set_engine("correctiond:us")`, call `SetFocusedApp("e2e")` with `gdbus`, keep a client-side text model updated from `commit-text` and `delete-surrounding-text` signals (and send it back with `set_surrounding_text` after each key), feed `process_key_event` for b,u,l,i,d,space. Assert: (1) model text == "build "; (2) with `set_content_type(PASSWORD, 0)`, a fresh model ends as "bulid "; (3) after (1), one BackSpace ⇒ "bulid ". Exit non-zero on any failure. Run `bash scripts/e2e.sh` — expected: three PASS lines
- [ ] **Step 5: Manual verification** — set `allow_any_caller = true` in config.toml for this step only, rebuild, reinstall, `ibus restart`. With the extension not yet installed, type `bulid ` in Firefox: expected NO correction (no wm_class ⇒ Observe). Then `busctl --user call org.nadelacruz.Correctiond1 /org/nadelacruz/Correctiond org.nadelacruz.Correctiond1 SetFocusedApp s firefox` and type `bulid ` in Firefox: expected it becomes `build `; immediate Backspace restores `bulid ` and a second attempt at `bulid ` is NOT corrected (blacklisted). In Ptyxis type `rm -rf ./bulid`: expected untouched. In a Firefox password field: untouched. Type `hunter2 ` twice in a Firefox text field and `export KEY=abc123` plus `some plain words here` twice in Ptyxis, then wait 30 s for a flush: GetStats `words_learned` does not grow for any of them. Note the secret from `secret-tool search --all purpose learn-db-key`, call `Purge` via busctl: `words_learned` drops to 0, any `learn.db.unreadable-*` files are gone, `learn.db` is recreated empty, and `secret-tool` shows exactly one item with a DIFFERENT secret. Then lock the keyring and call `Purge` again: it returns `PurgePartial`, `learn.db` is gone, `key_delete_pending` is true; unlock and change focus: the old item is deleted before a new one is created. Set `allow_any_caller = false` again, run `ibus restart` (config is read only at startup), and confirm busctl `SetFocusedApp` now fails with AccessDenied. Record results per app in `docs/designs/notes.md` (this is the spec's Assignment matrix, now run with the real engine).
- [ ] **Step 6: Commit** — `git commit -am "feat: control-plane dbus service and live silent corrections"`

### Task 11: GNOME Shell extension (wm_class + OSK watcher + toggle + forget)

**Files:**
- Create: `extension/metadata.json`, `extension/extension.js`, `scripts/install-extension.sh`
- Test: manual verification

**Interfaces:**
- Consumes: `org.nadelacruz.Correctiond1` D-Bus methods (Task 10)
- Produces:
  - Extension UUID `correctiond-companion@nadelacruz-00.github.io`, `metadata.json` shell-version `["50"]`
  - On `global.display` `notify::focus-window`: reads `focus_window.get_wm_class()` (lowercased — the CLASS string, matching the allowlist's "firefox"; not `get_wm_class_instance()`) and calls `SetFocusedApp`; null window ⇒ `SetFocusedApp("")` (engine treats empty as unidentified)
  - OSK watcher: locate the gjs-osk extension's keyboard actor by walking `Main.layoutManager.uiGroup` children for the actor gjs-osk adds (match by its style class/name at implementation time), connect `notify::visible` and `notify::mapped`, call `SetOskVisible`; also watch `Main.extensionManager` `extension-state-changed` for gjs-osk enable/disable; absent gjs-osk ⇒ never call SetOskVisible(true). Zero changes to gjs-osk code. The extension reads `auto_switch_osk` directly from `~/.config/correctiond/config.toml` via `GLib.KeyFile`-style line parsing (a boolean on one line; no TOML library needed — match `auto_switch_osk = true`). When it is true and the OSK becomes visible: switch the input source to the `correctiond:us` variant via the Shell's InputSourceManager (`imports/ui/status/keyboard.js`, `getInputSourceManager()`) and restore the prior source on hide.
  - Panel indicator (QuickSettings toggle): on/off calls `SetEnabled`; its menu has a "Forget learned words" item that asks for confirmation (a ModalDialog) and then calls `Purge`. Issue every call on `Gio.DBus.session` (e.g. `Gio.DBusProxy.makeProxyWrapper(xml)(Gio.DBus.session, 'org.nadelacruz.Correctiond1', '/org/nadelacruz/Correctiond')`) and never open a new bus connection: gnome-shell holds two session-bus connections here, and only the one behind `Gio.DBus.session` owns `org.gnome.Shell`, so only it passes Task 10's caller check. A `PurgePartial` error from "Forget" is shown to the user as a notification ("Learned words deleted; the key will be removed when your keyring unlocks")

- [ ] **Step 1: Implement `extension.js`** (ESM, GNOME 50 style: `export default class extends Extension`)
- [ ] **Step 2: Install and enable** — `scripts/install-extension.sh` copies to `~/.local/share/gnome-shell/extensions/`, then `gnome-extensions enable correctiond-companion@nadelacruz-00.github.io`; log out/in (Wayland)
- [ ] **Step 3: Manual verification** — focus Firefox: `busctl` monitor (or GetStats) shows wm_class updates arriving without manual busctl calls; typing `bulid ` in Firefox corrects. Open GJS OSK: tap-type a typo in a Firefox text field, expected correction uses QWERTY geometry (and the input source visibly switches to `correctiond:us` when auto_switch is on, restoring on OSK hide). Toggle the indicator off: no corrections anywhere. Use "Forget learned words": after confirming, GetStats `words_learned` is 0 and `learn.db` is recreated empty under a new key.
- [ ] **Step 4: Commit** — `git commit -am "feat: companion shell extension: wm_class, gjs-osk watcher, toggle"`

### Task 12: Standard vocabulary import format + engine-side import

**Files:**
- Create: `docs/vocab-format.md` (the normative format spec), `corelib/src/vocab.rs`
- Modify: `corelib/src/learning.rs` (`add_imported`), `corelib/src/db.rs` (`imports` table), `corelib/Cargo.toml` (add `sha2`), `engine/src/persist.rs` (import message), `engine/src/control.rs` (`ImportVocabulary`), `engine/src/main.rs` (`engine import` client mode)
- Test: inline `#[cfg(test)]` in `vocab.rs`, `db.rs`, `persist.rs`, `control.rs`; manual verification

**Interfaces:**
- Consumes: `is_learnable`, `Mode` (Task 5), `LearnStore`, `Db`, `DirtyBatch` (Task 6), the persistence actor and `PersistHandle` (Task 9), the control-plane service and caller check (Task 10)
- Produces:
  - **Format "correctiond vocab v1"** (eng review D7), files named `*.vocab.tsv`, documented in `docs/vocab-format.md` exactly as follows: UTF-8, LF line ends; the first non-empty line is exactly `#correctiond-vocab v1`; other lines starting with `#` are comments; record `w<TAB><word><TAB><count>` is one word with its count; record `b<TAB><word1><TAB><word2><TAB><count>` is one adjacent word pair; any other first field is ignored (forward compatibility); words are lowercase; counts are positive integers. Readers lowercase words, drop any word that fails `is_learnable(word, Mode::Silent)`, clamp counts to 1..=1,000,000, sum duplicate records, and reject files over 200,000 records or 16 MiB. A converter for any chat app or text source only has to write this format
  - `pub struct VocabBatch { pub words: Vec<(String, u64)>, pub bigrams: Vec<(String, String, u64)> }`, `pub enum VocabError { BadHeader, TooLarge }`, `pub struct VocabStats { pub skipped: u32 }`
  - `pub fn parse_vocab(text: &str) -> Result<(VocabBatch, VocabStats), VocabError>` (applies every reader rule above; malformed lines are skipped and counted)
  - `LearnStore::add_imported(&mut self, batch: &VocabBatch)` — adds counts onto vocab and bigrams and marks those rows dirty (an imported word with count ≥ 2 is persisted at the next flush)
  - `Db`: table `imports(sha256 TEXT PRIMARY KEY, at INTEGER)`; `pub fn has_import(&self, sha: &[u8; 32]) -> Result<bool, StoreError>`; `pub fn record_import(&mut self, sha: &[u8; 32]) -> Result<(), StoreError>`
  - `PersistHandle::import(&self, batch: VocabBatch, sha: [u8; 32]) -> Result<(u32, u32), String>` — handled by the actor: when NOT persistent ⇒ `Err("learning store not available; unlock your keyring and retry")` (so duplicate tracking can never be bypassed); when `has_import(sha)` ⇒ `Err("this file was already imported")`; otherwise lock briefly and `add_imported`, then `record_import` and flush, replying (words, word pairs) imported
  - D-Bus method `ImportVocabulary(h fd) -> (u words, u bigrams)` on `org.nadelacruz.Correctiond1`. It is accepted from ANY same-user caller (exempt from the GNOME Shell caller check, so a terminal command can use it). It reads at most 16 MiB from the fd inside `spawn_blocking`, takes the SHA-256 of the raw bytes, parses with `parse_vocab` and calls `PersistHandle::import`. Errors map to `org.nadelacruz.Correctiond1.Error.ImportFailed` with the message
  - Client mode: `engine import FILE` (when `argv[1] == "import"`, the binary acts as a D-Bus client instead of an IBus engine): opens FILE, passes its fd to `ImportVocabulary`, prints `Imported <words> words and <pairs> word pairs.` or the error, exit status 0 or 1. It never prints file content

- [ ] **Step 1: Write `docs/vocab-format.md`** with the format text above, plus a 4-line example file
- [ ] **Step 2: Write failing tests**

```rust
#[test] fn parse_minimal() { /* "#correctiond-vocab v1\nw\tako\t12\nb\tako\tpo\t3\n" ⇒ words [("ako",12)], bigrams [("ako","po",3)] */ }
#[test] fn bad_header_rejected() { /* missing or different first line ⇒ Err(BadHeader) */ }
#[test] fn comments_unknown_and_malformed() { /* "# note", "x\tfoo\t1", "w\tako" (no count) are skipped; stats.skipped counts the malformed one */ }
#[test] fn secrets_dropped_and_counts_clamped() { /* "w\thunter2\t5" and "w\tKEY=x\t5" dropped; "w\tAko\t9999999" ⇒ ("ako", 1_000_000) */ }
#[test] fn duplicates_summed() { /* two "w\tako\t2" lines ⇒ ("ako", 4) */ }
#[test] fn too_large_rejected() { /* 200,001 records ⇒ Err(TooLarge) */ }
#[test] fn import_is_idempotent() { /* actor test with FakeBackend: import(batch, sha) ⇒ Ok; same sha again ⇒ Err("…already imported") */ }
#[test] fn import_refused_when_not_persistent() { /* locked fake ⇒ Err("learning store not available…") and no words added */ }
#[test] fn import_allowed_from_any_caller() { /* caller-check helper: ImportVocabulary from ":1.99" with shell ":1.12" ⇒ allowed */ }
```

- [ ] **Step 3: Run to verify failure** — `cargo test --workspace` — expected: compile error
- [ ] **Step 4: Implement** `vocab.rs`, `add_imported`, the `imports` table, the actor message, `ImportVocabulary` and the client mode
- [ ] **Step 5: Run tests** — `cargo test --workspace` — expected: PASS
- [ ] **Step 6: Manual verification** — write `/tmp/t.vocab.tsv` with the doc's example, run `target/release/engine import /tmp/t.vocab.tsv`: it prints the counts, and GetStats `words_learned` grows; run it again: "already imported"; lock the keyring and import a different file: "unlock your keyring and retry"
- [ ] **Step 7: Commit** — `git commit -am "feat: correctiond vocab v1 import format and engine-side import"`

### Task 13: Facebook Messenger importer (separate tool, not part of the app)

**Files:**
- Create: `importers/messenger/messenger_to_vocab.py`, `importers/messenger/test_messenger_to_vocab.py`, `importers/messenger/README.md`, `importers/messenger/fixtures/message_1.json`
- Test: `python3 -m unittest discover importers/messenger`

**Interfaces:**
- Consumes: the "correctiond vocab v1" format (`docs/vocab-format.md`, Task 12) and the shared vectors file `data/learnable-vectors.tsv` (Task 5). Nothing else from the app: this tool imports no project code, is not in the Cargo workspace, and is NOT installed by the `.deb` (eng review D7)
- Produces:
  - `python3 importers/messenger/messenger_to_vocab.py --me "<your Messenger display name>" PATH... -o OUT.vocab.tsv` — Python 3 standard library only. PATH may be `message_*.json` files or directories, which are searched recursively for `message_*.json` (Meta's export puts them under `messages/inbox/<thread>/` or `your_facebook_activity/messages/inbox/<thread>/`). For each message with a string `content`: repair Meta's mojibake with `text.encode("latin-1").decode("utf-8")` (keep the original on failure), repair `sender_name` the same way, and keep the message only when `sender_name == --me`. Tokenize on every character that is not an ASCII letter or apostrophe, keep only tokens passing `learnable(token)`, count words and adjacent pairs within each message, and write the vocab v1 header plus `w`/`b` records sorted by count. Create OUT with `os.open(..., 0o600)` and refuse to overwrite an existing file. Print only totals: files read, messages read, your messages, distinct words, distinct pairs. Never print message text
  - `def learnable(token: str) -> bool` — must agree with Rust `is_learnable(token, Mode::Silent)` on every line of `data/learnable-vectors.tsv`
  - `README.md`: how to export from Meta (Accounts Center → Your information and permissions → Download your information → Messages → JSON), that end-to-end-encrypted chats may be missing from that export, how to run the converter, `engine import OUT.vocab.tsv` to load it, and to delete OUT afterwards (it is plaintext word counts)

- [ ] **Step 1: Write the fixture and failing tests** — `fixtures/message_1.json`: two participants ("Owner", "Friend"), messages from both, one with mojibake (`"Maupay nga aga Ã±"` stored latin-1-escaped), one photo-only message without `content`. Tests assert: only Owner's messages are counted; "maupay", "nga", "aga" appear and the mojibake-repaired "ñ" token is dropped (non-ASCII); the Friend's words are absent; output starts with `#correctiond-vocab v1`; OUT mode is 0600; an existing OUT is not overwritten; `learnable` agrees with every row of `data/learnable-vectors.tsv`; stdout contains no message text
- [ ] **Step 2: Run to verify failure** — `python3 -m unittest discover importers/messenger` — expected: FAIL (module missing)
- [ ] **Step 3: Implement `messenger_to_vocab.py` and `README.md`**
- [ ] **Step 4: Run tests** — expected: PASS
- [ ] **Step 5: End-to-end check** — run the converter on the fixture, then `target/release/engine import <out>`: the engine accepts it. (The Rust parser re-validates every word, so a converter bug can never write secrets into the store.)
- [ ] **Step 6: Commit** — `git add importers/ && git commit -m "feat: separate Messenger JSON to vocab v1 importer"`

### Task 14: Packaging and CI

**Files:**
- Create: `.github/workflows/ci.yml`, `.github/workflows/release.yml`, `debian/` or `[package.metadata.deb]` in `engine/Cargo.toml`
- Test: CI run on push; release dry-run

**Interfaces:**
- Consumes: everything prior
- Produces: `.deb` installing `/usr/lib/correctiond/engine`, `/usr/share/ibus/component/correctiond.xml` (binary path updated to the installed location), `/etc/apport/report-ignore/correctiond` containing the single line `/usr/lib/correctiond/engine` (apport skips crash reports for listed executables, which is the only control that stops `/var/crash` memory dumps under `fs.suid_dumpable=2`), and `/usr/share/doc/correctiond/`; the extension ships as a zip artifact (extensions.gnome.org submission is post-v1). `importers/` is never packaged (eng review D7): the `.deb` assets list names files explicitly and contains nothing under `importers/`

- [ ] **Step 1: ci.yml** — on push: `sudo apt-get install -y libssl-dev pkg-config` (SQLCipher links the system libcrypto), then `cargo test --workspace` + `cargo clippy -- -D warnings` on ubuntu-24.04-arm and ubuntu-24.04; then `sudo apt-get install -y ibus python3-gi gir1.2-ibus-1.0 dbus` and `cargo build --release && bash scripts/e2e.sh` (eng review D10); and `python3 -m unittest discover scripts` plus `python3 -m unittest discover importers/messenger`. The suite includes `cipher_version_present`, which fails the build if plain SQLite were ever linked. Set `permissions: contents: read` and pin third-party actions by commit SHA
- [ ] **Step 2: release.yml** — on tag `v*`: build release, `cargo deb -p engine` for arm64 + amd64 with `depends = "$auto"` (dpkg-shlibdeps adds the libssl runtime package, e.g. `libssl3t64`, so OpenSSL security updates arrive through apt), attach `.deb`s and the extension zip to the GitHub release (same tag-triggered pattern as the user's lig workflow). Only the release job gets `permissions: contents: write`
- [ ] **Step 3: Verify** — push to GitHub, confirm CI green; `git tag v0.1.0 && git push --tags`, confirm release artifacts; `sudo apt install ./correctiond_*_arm64.deb` on this machine, `ibus restart`, confirm the engine still works from the packaged path. Crash-report check: `kill -SEGV $(pgrep -f /usr/lib/correctiond/engine)`, then `ls /var/crash` shows no `_usr_lib_correctiond_engine.*` file; `ibus restart` afterwards. (This check must use the packaged binary: apport ignores unpackaged binaries under `target/`, so the dev build would pass even without the ignore entry.)
- [ ] **Step 4: Commit** — `git commit -am "ci: test workflow and tag-triggered deb release"`

---

## Eng Review (plan-eng-review, 2026-11-04)

Target: `docs/superpowers/plans/2026-11-04-correction-service.md` (this file), spec `docs/designs/correction-service.md`. The plan above this line is the working plan; review records below.

### Scope Challenge A — assessment

- **What already solves sub-problems:** [Layer 2] `librush` 0.2.3 (2026-07-19, LGPL-2.1+/GPL-3.0+, zbus ^5.18) is a pure-Rust IBus engine module without GObject — it covers Task 8's Engine/Factory/IBusText plumbing that the plan assumes no crate provides (Task 8 Interfaces: "no maintained high-level Rust IBus crate is assumed"). `/usr/share/dict/american-english` (package `wamerican`, SCOWL) and `/usr/share/hunspell/en_US.dic` are installed. Python `gi.repository.IBus` and `dbus-run-session` + `ibus-daemon` are present, enabling golden and headless integration tests.
- **Minimum change:** no feature cut proposed; the user explicitly requested every feature (silent fix, learning, terminal, multi-layout, OSK, encryption).
- **Complexity:** [estimated] ~30 proposed files (2 crates: corelib 7 modules + 2 benches; engine 7 modules; data 2; extension 2; scripts 2; workflows 2; packaging 1) and 5 new services/actors (IBus engine/factory, persistence actor, control-plane D-Bus service, Shell extension, SecretBackend). Gate trips (≥8 files, ≥2 services).
- **Search:** SQLCipher raw-key performance [Layer 1] (zetetic.net/sqlcipher/performance); oo7 Secret Service on zbus [Layer 2]; librush IBus engine [Layer 2].
- **TODOS:** no `TODOS.md` exists.
- **Distribution:** tag-triggered `.deb` arm64+amd64 (Task 12) covered; **no project license chosen** (matters for the OSS goal and for any LGPL/GPL dependency).

### Reality rows

Premise table:

| premise | claim | receipt | verdict |
|---|---|---|---|
| P1 | IBus purposes DIGITS=2 … PIN=9, TERMINAL=10 | probe `python3 gi IBus.InputPurpose` → `[('DIGITS', 2), …, ('TERMINAL', 10)]` vs plan Task 5 `OFF_PURPOSES` | true |
| P2 | HINT_NO_SPELLCHECK=1<<1, HINT_PRIVATE=1<<11 | probe → `hints 2 2048` vs Task 5 | true |
| P3 | IBusText wire type is a serialized variant | probe → `(sa{sv}sv)` = `('IBusText', {}, 'build ', <('IBusAttrList', {}, [])>)` vs Task 8 note | true |
| P4 | no Rust crate implements the IBus engine side | websearch → librush 0.2.3 implements an IBus module on zbus | **false** |
| P5 | `secret-tool` available for Task 9/10 checks | `command -v secret-tool` → not installed | needs `sudo apt install libsecret-tools` |
| P6 | oo7 0.6 `with_alias`, `is_locked`, `created`; rusqlite 0.40 `bundled-sqlcipher` | ~/.cargo/registry sources grep | true |
| P7 | apport honors `/etc/apport/report-ignore/` | `/usr/lib/python3/dist-packages/apport/report.py:61` `_DENYLIST_DIR = "/etc/apport/report-ignore"` | true |
| P8 | wordlist = "Hunspell wordlists" (spec) | spec Recommended Approach item 1 vs plan Task 3 Step 1 (hermitdave FrequencyWords) | **mismatch** |

unverified=0

REALITY: premise-table finding P4 false (librush exists) docs/designs/correction-service.md:140; P8 wordlist mismatch docs/designs/correction-service.md:117 vs docs/superpowers/plans/2026-11-04-correction-service.md:134; P5 secret-tool missing docs/superpowers/plans/2026-11-04-correction-service.md:413
REALITY: already-done pass no commits exist on any branch (`git log` reports an unborn HEAD) docs/superpowers/plans/2026-11-04-correction-service.md:1
REALITY: surface-check finding `secret-tool` not installed (`command -v secret-tool` empty), used at docs/superpowers/plans/2026-11-04-correction-service.md:413; `ibus engine`/`ibus restart`, `gnome-extensions enable`, `busctl call`, `cargo deb` 3.8.0 verified via --help
REALITY: binding-decisions pass plan honors both recorded decisions (Rust IBus engine + multi-layout; SQLCipher + keyring + no terminal learning) docs/superpowers/plans/2026-11-04-correction-service.md:19
REALITY: numbers finding SymSpell index: 1,548,925 delete keys for 50k words [measured: python probe over /usr/share/dict/american-english] for the design at docs/superpowers/plans/2026-11-04-correction-service.md:145; memory as String-keyed HashMap ~150-200 MB [estimated]
REALITY: numbers pass p99 < 10 ms is a target [estimated] docs/superpowers/plans/2026-11-04-correction-service.md:18
REALITY: numbers pass 50 ms flush gate is a target [estimated] docs/superpowers/plans/2026-11-04-correction-service.md:284
REALITY: numbers pass 2 s dictionary build threshold is a target [estimated] docs/superpowers/plans/2026-11-04-correction-service.md:352
REALITY: deferred-asks finding spec asks for learned slip patterns (docs/designs/correction-service.md:14) but no task learns them; vocab `layout` column (docs/superpowers/plans/2026-11-04-correction-service.md:258) is read by no query (decide at docs/superpowers/plans/2026-11-04-correction-service.md:161 uses personal_freq summed across layouts)
REALITY: pr-count pass one branch, one PR; no .gstack/ship-policy.json (default) docs/superpowers/plans/2026-11-04-correction-service.md:1

### Scope record (B)

feature answers: no cuts proposed (user requested every feature); structure: A (Original arrangement — corelib + engine crates + GJS extension), answer D1; accepted scope: plan as written; pending remedies: S1, S2, S3, A1, T1, T2, P1

## Decision ledger

### S1: Reuse librush for the IBus wire layer
Finding: S1, INFORMATIONAL, confidence 7, docs/designs/correction-service.md:140 ("no maintained high-level Rust IBus crate is assumed"), reviewer: plan-eng-review (Claude)
Plan baseline: Task 8 implements org.freedesktop.IBus.Engine/Factory and IBusText serialization from scratch, cribbing from ibus C source or busctl monitor (plan Task 8 "Connection bootstrap", line 356)
Runtime evidence: librush 0.2.3 (docs.rs, 2026-07-19) "ibus module written in pure rust, without GObject", LGPL-2.1-or-later OR GPL-3.0-or-later, zbus ^5.18; its public API shape and reusability as a library are unverified. Python gi probe gives the exact IBusText wire form `('IBusText', {}, 'build ', <('IBusAttrList', {}, [])>)`.
Comparison grid:
| Choice | Current | A | B | C |
|---|---|---|---|---|
| S1 IBus wire layer source | from scratch (ibus C source / busctl) | from scratch, with librush engine.rs/factory.rs as reading reference + python-gi golden | depend on librush crate | from scratch, as planned |
| S3 project license | unset, pending | unset, pending | GPL-3.0-or-later forced by dependency | unset, pending |
Question D2:
D2 — How should Task 8 get the IBus wire protocol right?
Project/branch/task: main (no commits yet) — eng review, Task 8 (IBus engine binding, plan line 356).
ELI10: Task 8 is the riskiest part of the plan: it has to speak IBus's private D-Bus protocol by hand, including a nested message format for text. One wrong byte there and typing breaks system-wide. I found an existing pure-Rust IBus engine module, librush (0.2.3, July 2026), that already does this on the same zbus version. We can read it as a reference while writing our own, depend on it directly, or ignore it.
Stakes if we pick wrong: Depending on it ties the whole project to GPL-3.0 and an unvetted 0.x crate on every keystroke; ignoring it means rediscovering the protocol's quirks by trial and error.
Recommendation: A because a working reference plus the python-gi golden removes most of Task 8's guesswork without a license lock-in or a new dependency on the keystroke path.
Note: options differ in kind, not coverage — no completeness score.
Pros / cons:
A) Reference librush (recommended)
  ✅ Task 8 copies proven message shapes instead of guessing; fewer broken-typing debug loops
  ✅ No new dependency or license constraint; you keep a free choice of project license
  ❌ Still writes ~300-500 lines of D-Bus plumbing ourselves, so the task stays the largest one
B) Depend on librush
  ✅ Task 8 shrinks to glue code; the protocol handling is someone else's maintained code
  ✅ Shares zbus 5 with the rest of the engine, so no second D-Bus stack in the binary
  ❌ Forces GPL-3.0-or-later (or LGPL static-link terms) and trusts an unvetted 0.x crate with every key
C) Keep plan as-is
  ✅ Zero exposure to another project's design choices or license text while writing the code
  ✅ No time spent reading a foreign codebase written for a different input method
  ❌ Rediscovers IBus quirks (IBusText nesting, signal shapes) by trial and error on a live desktop
Net: a reading reference buys most of the risk reduction of the dependency without its license and trust costs.
Header: IBus layer
Options:
A) Reference librush (recommended)
Read librush's engine.rs/factory.rs and the python-gi golden while writing Task 8 from scratch; no new dependency, no license constraint. Human: −1 day / CC: −20 min vs plan.
B) Depend on librush
Use the crate for Engine/Factory/IBusText; Task 8 shrinks to glue. Forces GPL-3.0-or-later (or LGPL static-link terms) and puts an unvetted 0.x crate on the keystroke path.
C) Keep plan as-is
Write the layer from the ibus C source and busctl captures only; no reference implementation consulted.

State: approved
Actual answer: A) Reference librush (recommended) — D2
Accepted scope: Task 8 adds a reading-reference note: consult librush 0.2.3 (github.com/fm-elpac/librush, src/ibus/engine.rs and factory.rs) and the python-gi golden for message shapes; no dependency added; no code copied verbatim (license LGPL/GPL); S3 license stays pending
History: none

### S2: Dictionary word source
Finding: S2, INFORMATIONAL, confidence 7, docs/superpowers/plans/2026-11-04-correction-service.md:134 (hermitdave FrequencyWords en_50k as the dictionary) vs docs/designs/correction-service.md:117 ("base dictionary (Hunspell wordlists)"), reviewer: plan-eng-review (Claude)
Plan baseline: Task 3 downloads hermitdave FrequencyWords `en_50k.txt` (subtitle-derived word+count) and uses it as both the set of known words and their frequencies; the spec's Hunspell source is not used and the change is not marked as a deviation
Runtime evidence: `/usr/share/dict/american-english` (package wamerican, SCOWL) is installed: 73,445 ASCII-alphabetic words [measured: probe]; `/usr/share/hunspell/en_US.dic` installed. Subtitle frequency lists contain names, slang and misspellings that would count as "known" and never be corrected [estimated, medium confidence: list contents not inspected offline]; the FrequencyWords data license is unverified.
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| S2 known-word set | en_50k subtitle list | SCOWL american-english (vetted spellings) | en_50k subtitle list, marked as a deviation |
| S2 frequencies | en_50k counts | en_50k counts for SCOWL words present there, else 1 | en_50k counts |
| S3 project license | unset, pending | unset, pending | unset, pending |
Question D3:
D3 — Which list decides what counts as a real word?
Project/branch/task: main (no commits yet) — eng review, Task 3 (dictionary, plan line 134).
ELI10: The dictionary decides two things: which words are "real" (never corrected) and how common each word is (which fix wins). The plan uses a list of the 50,000 most common words from movie subtitles for both jobs. The spec said a spelling dictionary. Subtitle lists include names, slang and common misspellings, and anything in the list is treated as correct, so those typos would never be fixed. A vetted spelling list (SCOWL, already installed here) can decide which words are real, while the subtitle counts still rank them.
Stakes if we pick wrong: Typos that happen to appear in subtitles stay uncorrected forever, and the engine "corrects" toward slang or names.
Recommendation: A because it matches the spec, uses a list already on this machine, and keeps the frequency ranking that scoring needs.
Completeness: A=9/10, B=6/10
Pros / cons:
A) Vetted words, subtitle ranks (recommended)
  ✅ Only real spellings count as known, so common typos found in subtitles still get corrected
  ✅ Matches the spec's Hunspell/SCOWL source and needs no extra download for the word set
  ❌ Needs a small build script to merge the two lists, plus an attribution note for the data
B) Keep subtitle list, mark deviation
  ✅ No merge script; Task 3 stays exactly as written
  ✅ Frequency and word set come from one consistent source
  ❌ Names, slang and subtitle misspellings become "known" words that are never corrected
Net: a small merge script buys a clean set of real words without losing the frequency ranking.
Header: Word source
Options:
A) Vetted words, subtitle ranks (recommended)
Known words = SCOWL /usr/share/dict/american-english (ASCII letters + apostrophe); frequency = en_50k count when present, else 1; a committed scripts/build-wordlist.py generates data/en_words.txt; data/ATTRIBUTION.md credits both sources. Human: ~2h / CC: ~10 min.
B) Keep subtitle list, mark deviation
Task 3 unchanged; add a line marking the subtitle list as a deliberate deviation from the spec's Hunspell source.

State: approved
Actual answer: "A amd learn from my native filipino/waray-waray language amd personality of chat" — D3 (option A selected, with an added request)
Accepted scope: option A exactly: known words = SCOWL american-english; frequency = en_50k count when present, else 1; scripts/build-wordlist.py generates data/en_words.txt; data/ATTRIBUTION.md. The added Filipino/Waray-Waray and chat-style request is NOT covered by this answer; it opens new choices L1/L2 below
History: none

### L1: Filipino (Tagalog) and Waray-Waray language support
Finding: L1, CRITICAL, confidence 8, user request in D3 answer ("learn from my native filipino/waray-waray language"); plan Task 4 decide (docs/superpowers/plans/2026-11-04-correction-service.md:161) treats any word missing from the English dictionary as a typo, so Tagalog/Waray words typed in Taglish chat get "corrected" toward English, reviewer: plan-eng-review (Claude)
Plan baseline: English-only dictionary (D3 A: SCOWL + en_50k ranks); unknown words protected only after 2 personal sightings (Task 4 rule `personal_freq(word) ≥ 2`)
Runtime evidence: `myspell-tl` (GPL-2+, 2005 Crúbadán data) and `aspell-tl` exist in apt; Leipzig Corpora Tagalog frequency lists are CC BY 4.0 (Wiktionary: Frequency lists/Tagalog); Leipzig `war_community_2017` corpus has 13.4M tokens / 399,359 types, license not stated (curl.corpora.uni-leipzig.de/languages/war); no open Waray dictionary found
Comparison grid:
| Choice | Current | A | B | C |
|---|---|---|---|---|
| L1 lexicons | English only | EN embedded + TL + WAR lexicons built locally by a script into ~/.local/share/correctiond/lexicons/ (not embedded, not shipped) | EN embedded + TL/WAR word sets used ONLY as a "never correct" list | English only |
| L1 candidate language | any English word | restricted to the field's dominant language (majority of last 4 known words; none yet ⇒ all languages, margin 0.6) | English only | English only |
| L1 Filipino word on first sighting | corrected toward English | never corrected (known in TL/WAR) | never corrected (protected) | corrected toward English |
| L2 chat import | none, pending | pending | pending | pending |
| S3 project license | unset, pending | unset, pending | unset, pending | unset, pending |
Question D4:
D4 — How should Filipino and Waray-Waray work in v1?
Project/branch/task: main (no commits yet) — eng review, Tasks 3/4/7 (dictionary, decide, engine state; plan line 161).
ELI10: Right now the engine only knows English. When you type Taglish or Waray, every Filipino word looks like an English typo, so "ako" or "kami" could get silently swapped for an English word. That is the worst thing an autocorrect can do. We can teach it Tagalog and Waray word lists and make it notice which language the sentence is in, so it fixes a Waray typo toward Waray and never pushes English onto Filipino words. Or we can just protect Filipino words from being touched, or leave it English-only for now.
Stakes if we pick wrong: English-only or protect-only means mixed-language messages either get wrong English "fixes" or Filipino typos never get fixed at all.
Recommendation: A because you type these languages daily, and only A both protects Filipino words and corrects Filipino typos.
Completeness: A=9/10, B=6/10, C=2/10
Pros / cons:
A) Full trilingual support (recommended)
  ✅ Tagalog and Waray words are known on day one, so they are never "fixed" toward English
  ✅ Sentence-language tracking fixes a Waray typo toward a Waray word, not an English one
  ❌ Adds a lexicon build script, language tags in the dictionary and a language window in engine state (human: ~3 days / CC: ~45 min)
B) Protect Filipino words only
  ✅ Stops the destructive case cheaply: words in the Tagalog/Waray lists are never touched
  ✅ No change to scoring or engine state; one extra word list (human: ~1 day / CC: ~15 min)
  ❌ Filipino and Waray typos are never corrected, and unlisted Waray words still get English fixes
C) English only for v1
  ✅ Plan stays exactly as written; fastest path to a working engine
  ✅ Personal learning still protects a Filipino word after you type it twice
  ❌ Every new Filipino word is at risk of a silent English swap until it has been typed twice
Net: A costs about 45 CC-minutes and is the only option that both protects and corrects the languages you actually type.
Header: Languages
Options:
A) Full trilingual support (recommended)
EN (embedded) + TL + WAR lexicons built on your machine by scripts/build-lexicons.py from Leipzig Corpora frequency lists into ~/.local/share/correctiond/lexicons/ (never embedded or shipped, so no data-license entanglement); word in any lexicon or personal vocab ⇒ never corrected; correction candidates restricted to the field's dominant language (majority of the last 4 known words; none yet ⇒ all languages with margin 0.6). Human: ~3 days / CC: ~45 min.
B) Protect Filipino words only
EN unchanged for correction; TL/WAR word sets (same local build) used only as a never-correct list. Human: ~1 day / CC: ~15 min.
C) English only for v1
No change; Filipino words protected only after two personal sightings.

State: approved
Actual answer: "A and add for custom languahr support/extensibility in the future" — D4 (option A, plus an extensibility requirement)
Accepted scope: option A exactly (EN embedded + TL + WAR lexicons built locally by scripts/build-lexicons.py from Leipzig frequency lists into ~/.local/share/correctiond/lexicons/, never shipped; known in any lexicon or personal vocab ⇒ never corrected; candidates restricted to the field's dominant language = majority of the last 4 known words, none ⇒ all languages with margin 0.6) PLUS, per the added request: languages are data, not code — a `[[languages]]` list in config.toml (code + lexicon path, EN as `builtin:en`), language ids assigned at load (max 16, bitmask), one generic lexicon format (`word<TAB>count`, UTF-8, lowercase) and one generic build script (`--lang <code> --input <frequency or plain-text file>`), so a new language is a file plus a config entry with no code change
History: none

### L2: Learn chat style from your exported chats
Finding: L2, INFORMATIONAL, confidence 7, user request in D3 answer ("learn from my ... personality of chat"); plan has no import path — vocabulary is learned only from live typing (Task 7 learning rule), and spec Open Question 5 (bootstrap from the user's own text) is unanswered, reviewer: plan-eng-review (Claude)
Plan baseline: no bootstrap; personal words protected only after two live sightings
Runtime evidence: none (no code yet). Reopened after D5 because the user then asked "can i export my messenger chats? … fb messenger": Meta's Download your information → Messages → JSON yields messages/inbox/<thread>/message_N.json with sender_name/content fields and latin-1-escaped UTF-8 text; end-to-end-encrypted chats may be excluded from that export (unverified)
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| L2 import inputs | none (D5 C) | Facebook Messenger JSON export only (message_*.json files or the export folder) | none |
| L2 whose words | n/a | only messages whose sender_name equals `--me "<name>"` | n/a |
| L2 write path | n/a | CLI parses and tokenizes locally, sends only word/bigram counts over a new D-Bus method ImportCounts; engine merges via the persistence actor (encrypted); source files never copied | n/a |
| L2 filters | n/a | same is_learnable shape rules as live typing; ImportCounts accepted from any same-user caller (not shell-only) | n/a |
Question D7:
D7 — Add a Messenger-only chat import after all?
Project/branch/task: main (no commits yet) — eng review, reopened D5 (chat import), new task after Task 10.
ELI10: You said no to chat import, then asked whether Messenger chats can be exported. They can: Meta's "Download your information" gives JSON files of your conversations. A small import command could read those files, keep only the messages you wrote, and teach the engine your words and Taglish/Waray spellings in one go. Only word counts are kept, encrypted like the rest; the chat files are never copied. Chats that are end-to-end encrypted may be missing from Meta's export.
Stakes if we pick wrong: Without import, your own words get "corrected" until you have typed each twice; with it, a parsing bug could teach it someone else's words.
Recommendation: A because Messenger is the export you actually have, and one format is much smaller than the three-format version you declined.
Completeness: A=8/10, B=3/10
Pros / cons:
A) Messenger import only (recommended)
  ✅ Day-one knowledge of your slang and Filipino/Waray spellings from your real chats
  ✅ One format, your messages only, counts only, encrypted; about a third of the earlier option's work
  ❌ Must fix Messenger's garbled text encoding, and E2EE chats may be absent from the export (human: ~1 day / CC: ~20 min)
B) Keep no import (D5)
  ✅ Nothing extra to build; the plan stays as you left it
  ✅ No code that reads private chat archives at all
  ❌ The engine learns your words only from live typing, two sightings each
Net: about 20 CC-minutes turns your Messenger history into day-one vocabulary, limited to your own messages.
Header: Messenger import
Options:
A) Messenger import only (recommended)
New task after Task 10: `correctiond import --me "<your name>" PATH...` accepts message_*.json files or the export folder; re-decodes Messenger's latin-1-escaped UTF-8; keeps only messages with sender_name == your name; applies is_learnable; sends word/bigram counts over D-Bus ImportCounts (any same-user caller); engine merges via the persistence actor; prints only totals. Human: ~1 day / CC: ~20 min.
B) Keep no import (D5)
No change from D5 C.

State: approved
Actual answer: "ready for meta/facebook messenger messages export in json format, make the importer a separate thing and do not bundle with the app, though lets create an easy import format standard" — D7 (option A, modified)
Accepted scope: (1) an app-side standard vocabulary import format, "correctiond vocab v1" (`.vocab.tsv`: UTF-8, `#correctiond-vocab v1` header, `w<TAB>word<TAB>count` and `b<TAB>word1<TAB>word2<TAB>count` records, `#` comments, unknown record types ignored), documented in docs/vocab-format.md; the engine re-validates every record with is_learnable and clamps counts; it accepts files through D-Bus `ImportVocabulary(h fd) -> (u words, u bigrams)` (any same-user caller) and an `engine import FILE` client subcommand; duplicate files (same SHA-256) are refused. (2) A SEPARATE Messenger importer, `importers/messenger/` (Python 3 stdlib only), NOT built into the engine binary and NOT shipped in the .deb, converting Meta "Download your information" JSON (message_*.json) into a .vocab.tsv: your messages only (`--me`), latin-1 mojibake repaired, is_learnable-equivalent filtering, output 0600. Ruling: Python stdlib keeps the importer dependency-free and independent of the Rust build; cost if wrong: a rewrite of one ~150-line script.
History: D5 asked a three-format import (plain .txt, WhatsApp, Telegram, Messenger); actual answer C) No import for v1 — D5; accepted scope then: none. Reopened because the user's follow-up question shows the Messenger export was the missing assumption.
### S3: Project license
Finding: S3, INFORMATIONAL, confidence 8, no LICENSE file and no `license` field planned (plan Task 12, docs/superpowers/plans/2026-11-04-correction-service.md Task 12 Produces; cargo-deb and the OSS goal in docs/designs/correction-service.md Distribution Plan need one), reviewer: plan-eng-review (Claude)
Plan baseline: no license chosen
Runtime evidence: dependency licenses: zbus MIT, oo7 MIT, rusqlite MIT, SQLCipher BSD-style (notice required), OpenSSL 3 Apache-2.0, SCOWL permissive (notice), en_50k counts CC-BY-SA-4.0 (attribution, share-alike on the data only); extensions.gnome.org requires a GPL-2.0-or-later-compatible license for extensions (MIT and Apache-2.0 qualify)
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| S3 code license | unset | MIT OR Apache-2.0 (dual) | GPL-3.0-or-later |
| S3 data notices | none | data/ATTRIBUTION.md (SCOWL, en_50k CC-BY-SA, SQLCipher, OpenSSL) | same |
Question D6:
D6 — Which license should the project use?
Project/branch/task: main (no commits yet) — eng review, Task 1 (scaffold) and Task 12 (packaging).
ELI10: The project has no license yet. Without one, nobody else may legally use or contribute to it, and the .deb package metadata is incomplete. MIT/Apache is the usual Rust choice: anyone may reuse the code, even in closed products. GPL-3 is what most Linux input methods use: anyone may reuse it, but their changes must stay open too. Every dependency here works with either.
Stakes if we pick wrong: Relicensing later needs every contributor's agreement, so the first choice tends to stick.
Recommendation: A — this is a taste call, no strong preference either way; MIT OR Apache-2.0 is the Rust default and keeps reuse friction lowest.
Note: options differ in kind, not coverage — no completeness score.
Pros / cons:
A) MIT OR Apache-2.0 (recommended)
  ✅ Matches the Rust ecosystem default, so crates and contributors expect it
  ✅ GPL-compatible, so the GNOME extension can still be published on extensions.gnome.org
  ❌ Someone could fork it into a closed product without sharing improvements back
B) GPL-3.0-or-later
  ✅ Keeps every fork's improvements open, like ibus-typing-booster and most input methods
  ✅ Would also allow depending on librush later without a license change
  ❌ Some companies and Rust contributors avoid GPL code, which can shrink contributions
Net: permissive maximizes reuse, copyleft guarantees improvements come back; both work with every dependency here.
Header: License
Options:
A) MIT OR Apache-2.0 (recommended)
Task 1 adds LICENSE-MIT, LICENSE-APACHE and `license = "MIT OR Apache-2.0"` in each Cargo.toml; extension metadata uses the same; data/ATTRIBUTION.md lists third-party data and library notices.
B) GPL-3.0-or-later
Task 1 adds LICENSE (GPL-3.0 text) and `license = "GPL-3.0-or-later"`; extension the same; same data/ATTRIBUTION.md.

State: approved
Actual answer: A) MIT OR Apache-2.0 (recommended) — D6
Accepted scope: Task 1 adds LICENSE-MIT, LICENSE-APACHE and `license = "MIT OR Apache-2.0"` in each Cargo.toml; extension metadata uses the same; data/ATTRIBUTION.md lists third-party data and library notices
History: none

### Scope Challenge C — findings and dispositions

1. S1 [INFORMATIONAL] (confidence 7) librush exists for the IBus wire layer — accepted D2 A (reference only).
2. S2 [INFORMATIONAL] (confidence 7) dictionary source deviates from spec — accepted D3 A (SCOWL words, en_50k ranks; one generic `scripts/build-lexicon.py` serves D3 and D4).
3. L1 [CRITICAL] (confidence 8) English-only engine would "correct" Tagalog/Waray words — accepted D4 A + data-driven language list.
4. L2 [INFORMATIONAL] (confidence 7) chat-style learning — D5 C (no import), reopened, accepted D7 (standard vocab format in-app; separate Messenger importer, not shipped).
5. S3 [INFORMATIONAL] (confidence 8) no project license — accepted D6 A (MIT OR Apache-2.0).

Scope Challenge result: scope accepted as-is (the review added scope at the user's request; nothing was cut).

### Section 1 — Architecture: design-twice

New boundaries: the `corelib` public surface and the persistence actor. Shape 1 (chosen in the plan): `corelib` owns pure logic plus the encrypted `Db` writer; `engine` owns IBus, keyring and the actor. Shape 2 (sketched): a separate `correctiond-store` daemon process owning the DB and keyring, with the engine talking to it over D-Bus — `trait StoreClient { fn record(&self, w: &str); fn lookup(&self, w: &str) -> u64; }`, bodies not implemented. Pick: Shape 1. Depth argument: Shape 2 puts a D-Bus round trip (or a second cache) behind every lookup and splits ownership of learned state across two processes; Shape 1 hides persistence, keys and quarantine behind `PersistHandle`'s four methods inside one process.

Red flags:
- Flag 8 (hand-synced list): the Messenger importer's token filter (Python) re-implements `is_learnable` (Rust). Fix (required proof of the approved D7 behavior, no new decision): a shared vector file `docs/vocab-format-vectors.tsv` (`token<TAB>learnable 0|1`), read by both a Rust test in corelib and the importer's Python test, so the two filters cannot drift.
- Flags 1–7: none found (one owner per state: `LearnStore` behind one mutex; IBus purpose constants in one place; config parsed only in Task 8's `config.rs`).

### A1: Learn personal slip patterns
Finding: A1, INFORMATIONAL, confidence 8, spec docs/designs/correction-service.md:14 ("learning their personal vocabulary and slip patterns") has no implementing task; the `vocab.layout` column (docs/superpowers/plans/2026-11-04-correction-service.md Task 6 Schema) is written by `record_word(w, layout)` but read by no query (`personal_freq` sums across layouts), reviewer: plan-eng-review (Claude)
Plan baseline: static per-layout adjacency tables only (Task 2); vocab rows keyed by (word, layout)
Runtime evidence: none (no code yet)
Comparison grid:
| Choice | Current | A | B | C |
|---|---|---|---|---|
| A1 slip learning | none | learned from your own mid-word fixes: when you backspace a character and type a different one, record (layout, typed, intended); `sub_cost` subtracts up to 0.2 for frequent pairs | none (TODO for post-v1) | none |
| A1 vocab key | (word, layout) | word only; layout lives in the new `slips(layout, typed, intended, count)` table | word only | (word, layout), unused |
| A1 persistence | n/a | slips ride the same DirtyBatch write-behind, encrypted | n/a | n/a |
Question D8:
D8 — Should it learn your personal finger slips per layout?
Project/branch/task: main (no commits yet) — eng review, Tasks 2/6/7 (layout tables, store, engine state).
ELI10: The spec promises the tool learns your slip patterns, like you on Colemak often hitting "n" when you meant "e". The plan never does that: it only uses fixed keyboard-neighbor tables, and it stores a per-layout copy of every word that no code ever reads. There is a clean way to learn slips: whenever you backspace one letter mid-word and type a different one, that pair (typed, meant) is a real slip, observed straight from your fingers. Those pairs make that specific mistake cheaper to fix, per layout.
Stakes if we pick wrong: Without it, the "learns how you type" promise is only vocabulary; learning from accepted autocorrections instead would reinforce its own mistakes.
Recommendation: A because your own backspace fixes are the most honest slip signal available, it gives the per-layout data a real use, and it costs about 20 CC-minutes.
Completeness: A=9/10, B=6/10, C=4/10
Pros / cons:
A) Learn slips from your fixes (recommended)
  ✅ Learns your real Colemak, QWERTY and on-screen-keyboard slips, so frequent mistakes get fixed more reliably
  ✅ Signal comes from your own corrections, not from autocorrect's, so it cannot reinforce its own errors
  ❌ One more table, one more dirty-row type and a cost tweak in scoring (human: ~1 day / CC: ~20 min)
B) Simplify now, defer slips
  ✅ Drops the unused per-layout word column, so the store and second-sighting rule get simpler
  ✅ Leaves a clear TODO for slip learning after v1 is in daily use
  ❌ The spec's "learns your slip patterns" stays unimplemented for v1
C) Keep plan as-is
  ✅ No change to tasks or schema
  ✅ Nothing new to test
  ❌ Stores per-layout word copies that nothing reads, and still no slip learning
Net: about 20 CC-minutes delivers the spec's slip learning from the cleanest possible signal and gives the layout data a purpose.
Header: Slip learning
Options:
A) Learn slips from your fixes (recommended)
WordBuffer remembers the char removed by a mid-word Backspace; the next Printable that differs records (layout, typed=removed, intended=new) in LearnStore; `slips(layout TEXT, typed TEXT, intended TEXT, count INTEGER, PRIMARY KEY(layout, typed, intended))` persisted via DirtyBatch (written at count ≥ 2); `sub_cost(layout,a,b)` subtracts `0.2 * min(1, count/5)`, floor 0.1; vocab keyed by word only. Not learned in Off or ProseAware. Human: ~1 day / CC: ~20 min.
B) Simplify now, defer slips
Vocab keyed by word only (drop layout column and parameter); slip learning recorded as a TODO.
C) Keep plan as-is
No change.

State: approved
Actual answer: A) Learn slips from your fixes (recommended) — D8
Accepted scope: WordBuffer.pop returns the removed char; Task 7 records (layout, typed=removed, intended=next differing Printable) via LearnStore::record_slip in Silent/Observe only; `slips(layout TEXT, typed TEXT, intended TEXT, count INTEGER, PRIMARY KEY(layout, typed, intended))` persisted via DirtyBatch at count ≥ 2; PersonalLookup::slip_count; weighted_dl substitution cost = max(0.1, sub_cost(layout, typed, intended) − 0.2·min(1, slip_count/5)); vocab keyed by word only (layout column and parameter removed)
History: none

### Section 1 — findings and dispositions

- A1 [INFORMATIONAL] (confidence 8) slip learning missing; vocab layout column unused — accepted D8 A (Tasks 1, 4, 6, 7 amended).
- Red flag 8 [INFORMATIONAL] (confidence 8) Python/Rust token filters would drift — fixed as required proof of D7: `data/learnable-vectors.tsv` read by Task 5 and Task 13 tests.
- Key flow (keystroke → correction), for the implementers:

```
key event ─▶ ibus.rs ProcessKeyEvent ─▶ classify_key ─▶ resolve_mode(ctx)
                                                    │
                    ┌───────────── Off ◀────────────┤ (sensitive field/hint, or terminal not allowlisted)
                    │  Pass, learn nothing          │
                    ▼                               ▼
                 return false        EngineState::on_key (WordBuffer, ProseRun, LangWindow)
                                                    │ boundary
                                                    ▼
                          decide(word, LangWindow.ctx(), dict, store) ── None ─▶ Pass (+ learn if allowed)
                                                    │ Some(fix)
                                                    ▼
                        verify surrounding ─ mismatch ─▶ Pass
                                                    │ ok
                                                    ▼
                   DeleteSurroundingText + CommitText, return consume ─▶ revert window opens
```

### Section 2 — Code quality: findings and dispositions

- Q1 [INFORMATIONAL] (confidence 7) docs/superpowers/plans/2026-11-04-correction-service.md Task 5 LangWindow: "a word in several lexicons counts for each" lets English words inside the Tagalog/Waray corpora outvote English, so English sentences never reach English dominance — fixed as necessary implementation of the approved D4 contract ("majority of the last 4 known words"), no new choice: `Dictionary::primary_lang` (argmax relative frequency) is the vote; Tasks 3, 5, 7 amended with tests.
- No other issues found: error paths (missing lexicon ⇒ warning, invalid config ⇒ defaults with warning, store/keyring failures ⇒ memory-only) are specified with tests in Tasks 6, 8, 9.

### Section 3 — Test review

Framework: none yet (no Cargo.toml); plan specifies Rust `cargo test` + criterion, Python `unittest` for scripts/importer.

```
CODE PATHS                                             USER FLOWS
[+] corelib tokenizer/layout/dictionary/scoring        [+] Type a typo in Firefox, space
  ├── [★★★ PLANNED] classify_key, WordBuffer (T1)        ├── [★★ PLANNED] fix + revert (Task 7 unit; Task 10 manual)
  ├── [★★★ PLANNED] sub_cost, slips (T2, T4)             └── [GAP] [→E2E] real IBus round trip (T2 below)
  ├── [★★★ PLANNED] lexicons, candidates, lang filter    [+] Taglish / Waray sentence
  └── [★★★ PLANNED] decide incl. Filipino guards         └── [★★ PLANNED] Task 4/7 unit; manual in Firefox
[+] corelib policy/learning/db/vocab                   [+] Terminal typing (Ptyxis)
  └── [★★★ PLANNED] modes, gates, encryption, quarantine └── [★★ PLANNED] Task 5/7 unit; manual
[+] engine ibus.rs (Task 8)                            [+] Keyring locked at login
  ├── [GAP] IBusText / signal serialization             └── [★★★ PLANNED] Task 9 fake-backend tests
  └── [GAP] [→E2E] Factory/Engine over a real daemon   [+] Forget learned words
[+] engine persist/keyring/control (Tasks 9-10, 12)        └── [★★★ PLANNED] Task 9/10 tests + manual
  └── [★★★ PLANNED] fake-backend actor tests           [+] Messenger import
[+] extension.js (Task 11)                                 └── [★★★ PLANNED] Task 13 fixture tests
  └── [GAP] manual only (accepted: no GJS harness)

COVERAGE (planned): 13/16 paths with ★★+ tests | GAPS: 3 (1 unit, 1 E2E, 1 accepted manual)
```

### T1: Golden test for IBus message serialization
Finding: T1, CRITICAL, confidence 8, docs/superpowers/plans/2026-11-04-correction-service.md Task 8 ("Test: manual verification (D-Bus surface; no unit harness for IBus exists)"); a wrong IBusText encoding makes every commit fail and breaks typing system-wide, reviewer: plan-eng-review (Claude)
Plan baseline: Task 8 has no automated test; serialization correctness is checked only by typing on the desktop
Runtime evidence: python-gi on this machine serializes `IBus.Text.new_from_string("build ")` as `('IBusText', @a{sv} {}, 'build ', <('IBusAttrList', @a{sv} {}, @av [])>)` with signature `(sa{sv}sv)` [measured: probe]
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| T1 serialization proof | manual typing only | unit test: Rust `ibus_text("build ")` serializes to bytes equal to a golden produced by python-gi (`tests/golden/ibustext_build.bin`, generated by `scripts/gen-ibus-golden.py`, both committed) | manual typing only |
| T2 E2E harness | none, pending | pending | pending |
Question D9:
D9 — Add a golden test for the IBus text format?
Project/branch/task: main (no commits yet) — eng review, Task 8 (IBus engine binding).
ELI10: Every correction is sent to the app as an IBus "text" message with a nested structure. If our Rust code builds that structure even slightly wrong, the app rejects it and typing breaks everywhere. The plan only checks this by typing on the desktop. IBus's own Python library on this machine can produce the exact correct bytes, so we can save those once and make a unit test that fails the moment our encoding differs.
Stakes if we pick wrong: An encoding mistake surfaces as "typing stopped working" on your desktop instead of a failing test.
Recommendation: A because it turns the single most dangerous byte layout into a 10-second automated check using IBus's own library as the reference.
Completeness: A=9/10, B=4/10
Pros / cons:
A) Add the golden test (recommended)
  ✅ Catches a wrong IBus text encoding in `cargo test` before it can break typing on the desktop
  ✅ The reference bytes come from IBus's own library, so the test cannot agree with our own bug
  ❌ One small generator script and a binary fixture to keep in the repo (human: ~2h / CC: ~5 min)
B) Manual typing only
  ✅ No extra files; Task 8 stays as written
  ✅ Typing on the real desktop still exercises the full path
  ❌ Encoding regressions are found only by breaking your own typing
Net: five CC-minutes buys an automated check on the byte layout every keystroke depends on.
Header: Golden test
Options:
A) Add the golden test (recommended)
Task 8 adds `scripts/gen-ibus-golden.py` (python-gi: serialize IBus.Text "build " and an empty attr list to GVariant bytes) and `tests/golden/ibustext_build.bin`, plus a Rust unit test asserting our zbus serialization of the same text equals the golden bytes. Value: protects=IBusText wire layout; fails_when=field order, signature or attr-list nesting changes; why_new=Task 8 has no automated test; seam=none; no_claim=signal emission and daemon acceptance (covered by T2 if approved). Human: ~2h / CC: ~5 min.
B) Manual typing only
No change to Task 8.

State: approved
Actual answer: A) Add the golden test (recommended) — D9
Accepted scope: Task 8 adds `scripts/gen-ibus-golden.py` (python-gi) and the committed golden `engine/tests/golden/ibustext_build.json`, plus a Rust unit test asserting that our IBusText value for "build " has exactly the golden's signatures and nested field values
History: Factual correction after the answer (no behavior change): the option said "bytes equal to a golden produced by python-gi", but GVariant serialization is not the D-Bus wire format, so a raw-byte comparison would be wrong. The golden therefore records signature strings plus nested field values; byte-level D-Bus marshalling stays zbus's responsibility.

### T2: Headless end-to-end test through a real IBus daemon
Finding: T2, CRITICAL, confidence 7, docs/superpowers/plans/2026-11-04-correction-service.md Tasks 8 and 10 (live corrections verified only by manual typing); the Factory/Engine registration, ProcessKeyEvent return values, DeleteSurroundingText/CommitText signals and the Off-mode gate have no automated end-to-end check, reviewer: plan-eng-review (Claude)
Plan baseline: manual verification in Firefox/Ptyxis (Task 10 Step 5); unit tests below the D-Bus layer
Runtime evidence: `ibus-daemon` here supports `--single`, `--panel=disable`, `--config=disable`, `--address`; python-gi `IBus.InputContext` has `process_key_event` and `set_surrounding_text`, and `IBus.Capabilite.SURROUNDING_TEXT` exists [measured: probe]; `dbus-run-session` is installed
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| T2 E2E harness | none | `scripts/e2e.sh` + `tests/e2e/ibus_roundtrip.py`: private session bus and temp HOME, headless ibus-daemon, the engine in dev registration mode, a python-gi client; runs locally and in CI | none |
| T1 golden test | approved D9 | approved D9 (unchanged) | approved D9 (unchanged) |
Question D10:
D10 — Add an automated end-to-end test through a real IBus daemon?
Project/branch/task: main (no commits yet) — eng review, Tasks 8, 10 and 14 (engine binding, live corrections, CI).
ELI10: Unit tests check the logic, and the golden test checks the message layout, but nothing automatically checks that the real IBus daemon accepts our engine and that a typed "bulid " really comes back as "build ". The plan relies on you typing in Firefox after each change. We can run a private, invisible IBus daemon in a test, plug our engine into it, type into it from a script, and check exactly what text comes out, including that a password field gets no correction.
Stakes if we pick wrong: Without it, any regression in the D-Bus wiring (wrong return value, double space, missing signal) is found by breaking your own typing, and CI can never catch it.
Recommendation: A because it is the only automated check of the whole keystroke-to-correction path, and the tools are already on this machine.
Completeness: A=9/10, B=5/10
Pros / cons:
A) Add the headless E2E test (recommended)
  ✅ Proves "bulid " becomes "build " and password fields stay untouched, through the real daemon
  ✅ Runs in CI on every push, so wiring regressions never reach your desktop
  ❌ Headless IBus setup can be fiddly to get stable (human: ~1 day / CC: ~30 min)
B) Manual verification only
  ✅ No harness to build or keep stable
  ✅ Manual checks still run in the real apps you use
  ❌ D-Bus wiring bugs are caught only by hand, never by CI
Net: about 30 CC-minutes buys CI coverage of the full path from keystroke to corrected text.
Header: E2E test
Options:
A) Add the headless E2E test (recommended)
Task 10 adds `scripts/e2e.sh` (dbus-run-session; temp HOME with config.toml `allowlist = ["e2e"]`, `allow_any_caller = true`; `ibus-daemon --single --panel=disable --config=disable --address=<private>`; engine started in dev registration mode) and `tests/e2e/ibus_roundtrip.py` (python-gi client: SURROUNDING_TEXT capability, `set_engine("correctiond:us")`, SetFocusedApp("e2e"), keys for "bulid " with surrounding updates). Asserts: final text "build "; PASSWORD purpose ⇒ "bulid " unchanged; Backspace right after the fix restores "bulid ". Task 14 CI installs ibus, python3-gi, gir1.2-ibus-1.0 and runs it. Value: protects=keystroke-to-commit path; fails_when=registration, return values or signals regress; why_new=only manual checks exist; seam=dev registration mode (already planned in Task 8); no_claim=GNOME Shell extension and real apps. Human: ~1 day / CC: ~30 min.
B) Manual verification only
No change.

State: approved
Actual answer: A) Add the headless E2E test (recommended) — D10
Accepted scope: Task 10 adds scripts/e2e.sh and tests/e2e/ibus_roundtrip.py exactly as in option A (three assertions: fix, PASSWORD unchanged, revert); Task 8 gains an explicit `--register` flag for the dev registration mode the harness uses; Task 14 CI installs ibus, python3-gi, gir1.2-ibus-1.0 and runs scripts/e2e.sh
History: none

Section 3 findings and dispositions:
- T1 [CRITICAL] (confidence 8) IBusText layout untested — accepted D9 A (with factual correction: structure golden, not raw bytes).
- T2 [CRITICAL] (confidence 7) no automated keystroke-to-commit test — accepted D10 A.
- T3 [INFORMATIONAL] (confidence 6) extension.js manual-only — accepted as-is: no GJS test harness exists here, and the extension's correctness-critical outputs (wm_class, OSK flag) are covered at the engine boundary by Task 10 tests; no new choice.
Test plan artifact: (kept outside the repo)

### Section 4 — Performance review

### P1: Candidate index memory and build time
Finding: P1, CRITICAL, confidence 8, docs/superpowers/plans/2026-11-04-correction-service.md Task 3 Step 5 ("precompute all deletes up to depth 2 per dictionary word into a `HashMap<String, Vec<u32>>`"), reviewer: plan-eng-review (Claude)
Plan baseline: full-word depth-2 deletes, String keys, one Vec per key; built at engine startup before IBus registration (Task 8)
Runtime evidence: 50k English words ⇒ 1,548,925 unique delete keys and 1,950,551 postings [measured: python probe over american-english]; with SymSpell's prefix length 7 ⇒ 406,086 keys and 1,229,799 postings [measured: same probe]. String-keyed HashMap ≈ 150–200 MB for English alone [estimated]; the D4 lexicons (Tagalog list + Waray corpus with 399,359 types before filtering) multiply that [estimated]. Prefix-7 with u64 hashed keys and one flat postings array ≈ 10–20 MB for English [estimated].
Comparison grid:
| Choice | Current | A | B |
|---|---|---|---|
| P1 index layout | String keys, full-word deletes, Vec per key | SymSpell prefix length 7; keys = 64-bit hash of the delete string; one sorted key table + one flat `Vec<u32>` of postings; every candidate verified with full `weighted_dl` (hash collisions only add candidates that verification rejects) | as planned |
| P1 lexicon cap | none | none | none |
| P1 budget gate | none | Task 3 test: index for `data/en_words.txt` ≤ 32 MiB by `index_bytes()`; Task 8 logs build time and index size (numbers only) | none |
Question D11:
D11 — Shrink the spelling index so the engine stays light?
Project/branch/task: main (no commits yet) — eng review, Task 3 (dictionary index), affects Task 8 startup.
ELI10: To find likely fixes quickly, the engine precomputes every way to delete one or two letters from every dictionary word. As planned, that is about 1.5 million entries for English alone, each stored as its own small string, which is very roughly 150-200 MB of memory just for English, more once Tagalog and Waray are added. The standard trick in this algorithm (SymSpell) only indexes the first 7 letters of each word, and storing short number fingerprints instead of strings shrinks it about 10x. It finds the same fixes, because every suggestion is still double-checked against the full word.
Stakes if we pick wrong: A keyboard engine using hundreds of MB on a low-memory tablet, with slower startup before typing works, versus a smaller index that must be implemented carefully.
Recommendation: A because it is the algorithm's standard optimization, keeps memory about 10x lower with the same fixes, and adds a size check that stops regressions.
Completeness: A=9/10, B=5/10
Pros / cons:
A) Prefix-7 hashed index (recommended)
  ✅ Measured 406k index keys instead of 1.55M for English; roughly 10-20 MB instead of 150-200 MB
  ✅ Same suggestions, because every candidate is verified against the full word before use
  ❌ More careful code: hashing, a flat postings array and prefix handling for long words (human: ~1 day / CC: ~15 min)
B) Keep the planned index
  ✅ Simplest code; a direct translation of the algorithm
  ✅ Fine for a small English-only dictionary on a machine with plenty of RAM
  ❌ Roughly 150-200 MB for English and much more with Tagalog and Waray; slower startup
Net: about 15 CC-minutes keeps a three-language engine small enough to forget it is running.
Header: Index size
Options:
A) Prefix-7 hashed index (recommended)
Task 3 Step 5 builds deletes of each word's first 7 chars (depth ≤ 2), keys are 64-bit hashes (std `DefaultHasher` or `ahash`), stored as a sorted `Vec<(u64, u32 start, u32 len)>` plus one flat `Vec<u32>` of word indices; lookup hashes the input prefix's deletes, gathers candidates, filters by language and verifies with full `weighted_dl`; adds `pub fn index_bytes(&self) -> usize` and a test asserting ≤ 32 MiB for `data/en_words.txt`; Task 8 logs build ms and index MiB. Human: ~1 day / CC: ~15 min.
B) Keep the planned index
No change.

State: approved
Actual answer: A) Prefix-7 hashed index (recommended) — D11
Accepted scope: Task 3 Step 5 builds the prefix-7, 64-bit-hashed, flat-postings index exactly as in option A, with `index_bytes()` and a ≤ 32 MiB test for data/en_words.txt; Task 8 logs build ms and index MiB (numbers only)
History: none

Section 4 findings and dispositions:
- P1 [CRITICAL] (confidence 8) candidate index ~150–200 MB as planned [estimated from measured key counts] — accepted D11 A.
- No other issues found: the typing path reads in-memory maps only (store lock held for single calls); disk and keyring work runs off the typing path in `spawn_blocking`; the bigram/vocab caps (100k/200k) bound memory; no queries in loops.

### Outside voice

Outside voice unavailable: Codex CLI is not installed (`CODEX_MODE: not_installed`), and the native Plan-agent fallback produced no complete report within the five-minute limit (stopped; no late-result credit). Fix: install the Codex CLI (`codex login`) for a true second model. Continuing to planning decisions and Approval readiness. (OUTSIDE_VOICE_UNAVAILABLE)

### TD1: TODO — non-ASCII words (ñ and others) for Tagalog/Waray
Finding: TD1, INFORMATIONAL, confidence 8, Task 3 lexicon format ("Entries containing any non-ASCII character are DROPPED at load … Tagalog words with ñ are therefore not in v1 lexicons"), reviewer: plan-eng-review (Claude)
Plan baseline: v1 corrects and lexicons hold only ASCII words (octet-offset mitigation, Global Constraints)
Runtime evidence: none (no code yet)
Comparison grid:
| Choice | Current | A | B | C |
|---|---|---|---|---|
| TD1 non-ASCII words | dropped in v1, no TODO | TODOS.md entry (P3) | no TODO | build now in v1 |
Question D12:
D12 — Track non-ASCII Filipino words (ñ) as a TODO?
Project/branch/task: main (no commits yet) — eng review, TODOS.md (new file).
ELI10: To avoid one IBus bug that can delete the wrong letters when text contains accented characters, v1 only handles plain A–Z words. That means Tagalog words with ñ (like "niño") are not in the word lists and are never corrected. Supporting them later means detecting how each app counts characters. This asks whether to write that down as a TODO so it is not forgotten.
Stakes if we pick wrong: Skipping it means words with ñ are quietly never handled; building it now adds the riskiest kind of text editing to v1.
Recommendation: A because it is real but low-frequency, and building it now would add risk to v1 for a handful of words.
Completeness: A=7/10, B=3/10, C=9/10
Pros / cons:
A) Add to TODOS.md (recommended)
  ✅ The ñ limitation is written down with its cause and a starting point for later
  ✅ v1 keeps the simpler, safer plain-letters rule for deleting text
  ❌ Words with ñ stay unhandled until someone picks the TODO up
B) Skip
  ✅ No extra file or tracking overhead
  ✅ The limitation is still stated in the plan's lexicon format note
  ❌ Easy to forget; nothing points back to it after v1 ships
C) Build it now
  ✅ Full Tagalog spelling coverage in v1, including ñ words
  ✅ No later rework of the lexicon format
  ❌ Needs per-app detection of character vs byte offsets, the exact bug the ASCII rule avoids (human: ~2 days / CC: ~40 min)
Net: a one-paragraph TODO keeps v1 safe without losing track of the ñ gap.
Header: TODO: ñ words
Options:
A) Add to TODOS.md (recommended)
Create TODOS.md with a "## Language" section item "Support non-ASCII words (ñ)": What/Why/Context (octet vs character offsets; start at Task 3 parse_lexicon and the verify-before-delete path), Effort M, Priority P3.
B) Skip
No TODO.
C) Build it now
Accept non-ASCII words in v1 with per-app offset detection (new scope).

State: approved
Actual answer: A) Add to TODOS.md (recommended) — D12
Accepted scope: TODOS.md created with the "## Language" item "Support non-ASCII words (ñ and accented letters)" (Effort M, Priority P3); Task 3 marks the ASCII drop in code with a `gstack-shortcut` comment (accepted shortcut: completeness 7/10)
History: none

### TD2: TODO — more chat importers (WhatsApp, Telegram) for vocab v1
Finding: TD2, INFORMATIONAL, confidence 7, D5 offered WhatsApp/Telegram/plain-text import; D7 approved Messenger only plus the "correctiond vocab v1" format "so a converter for any chat app or text source only has to write this format" (Task 12), reviewer: plan-eng-review (Claude)
Plan baseline: only the Messenger importer exists (Task 13)
Runtime evidence: none (no code yet)
Comparison grid:
| Choice | Current | A | B | C |
|---|---|---|---|---|
| TD2 other importers | none, no TODO | TODOS.md entry (P4) | no TODO | build now |
Question D13:
D13 — Track WhatsApp and Telegram importers as a TODO?
Project/branch/task: main (no commits yet) — eng review, TODOS.md.
ELI10: The plan now has one importer (Messenger) and a simple standard file format that any importer can write. WhatsApp and Telegram exports could get their own small converters later, each producing that same file. This asks whether to note them as a future TODO, skip them, or build them now.
Stakes if we pick wrong: Small either way: skipping only loses a reminder; building now adds two more parsers before v1 is in daily use.
Recommendation: A because the format makes these easy later, and you only use Messenger today.
Completeness: A=7/10, B=5/10, C=9/10
Pros / cons:
A) Add to TODOS.md (recommended)
  ✅ Records that new importers only need to write the vocab v1 format, so they stay small
  ✅ No extra code before v1 is in daily use
  ❌ WhatsApp and Telegram history is not importable until someone builds them
B) Skip
  ✅ Nothing to track for apps you do not use for chatting
  ✅ The format document already explains how to write a converter
  ❌ The idea is easy to lose once v1 ships
C) Build them now
  ✅ All three common chat exports work on day one
  ✅ Reuses the same format and the shared learnable-vectors test file
  ❌ Two more parsers and fixtures to maintain for apps you may not use (human: ~1 day / CC: ~20 min)
Net: a reminder costs nothing; building now spends effort on exports you may never have.
Header: TODO: importers
Options:
A) Add to TODOS.md (recommended)
TODOS.md "## Importers" item "WhatsApp and Telegram importers": What/Why/Context (write vocab v1 per docs/vocab-format.md; mirror importers/messenger incl. learnable-vectors test), Effort S, Priority P4.
B) Skip
No TODO.
C) Build them now
Two more separate importers under importers/ (new scope).

State: approved
Actual answer: B) Skip — D13
Accepted scope: none (no TODO; docs/vocab-format.md already explains how to write a converter)
History: none

Approval readiness: PASS — S1 (D2), S2 (D3), S3 (D6), L1 (D4), L2 (D7, reopening D5), A1 (D8), T1 (D9), T2 (D10), P1 (D11), TD1 (D12), TD2 (D13); Q1 and red flag 8 applied as necessary implementation of D4 and D7; T3 accepted as-is (no change).

### NOT in scope

- WhatsApp/Telegram importers — D13 skipped; any converter can target docs/vocab-format.md later.
- Non-ASCII words (ñ) — deferred to TODOS.md (D12): the byte-vs-character offset risk.
- Depending on librush — D2: reference only, to keep license freedom and keep an unvetted crate off the keystroke path.
- Shipping Tagalog/Waray lexicon data — built locally only (D4), because of the Leipzig data licenses.
- Preedit fallback, per-app mute, last-correction peek, GSettings — post-v1 (plan Global Constraints, unchanged).

### What already exists

- librush 0.2.3 — reading reference for Task 8 (D2), not reused as code.
- SCOWL `/usr/share/dict/american-english` (installed) — English word set (D3), reused as data.
- python-gi IBus (installed) — golden generator (D9) and E2E client (D10).
- `ibus-daemon`, `dbus-run-session` (installed) — headless E2E harness (D10).
- Leipzig Corpora frequency lists — Tagalog/Waray sources (D4), fetched by the user.
- Accepted shared-code choice: `data/learnable-vectors.tsv` shared by the Rust and Python token filters (red flag 8), so neither re-implements the rule untested.

### Failure modes

| Path | Realistic failure | Test / handling | User sees |
|---|---|---|---|
| IBusText encoding (Task 8) | field order or nesting wrong | golden test (D9), E2E (D10) | nothing; CI fails |
| Language detection (Tasks 3/5/7) | English words in TL corpus outvote English | Q1 primary_lang + tests | correct English fixes |
| Missing TL/WAR lexicon file (Task 8) | user has not built them yet | skipped with warning, English only | English-only behavior, no crash |
| Vocab import (Task 12) | same file imported twice | SHA-256 dedupe test | "already imported" |
| Vocab import while keyring locked | store not persistent | refusal test | "unlock your keyring and retry" |
| Messenger converter (Task 13) | friend's messages counted, or mojibake | fixture tests; engine re-validates | only your words learned |
| Slip learning (Task 7) | slips learned in a password field | Off-mode test | nothing learned |
| Index build (Task 3/8) | memory or startup regression | ≤ 32 MiB test, build-time log | light engine |

Critical gaps: 0 (every new path has a test or explicit handling, and none fails silently).

### Worktree parallelization strategy

| Step | Modules touched | Depends on |
|------|----------------|------------|
| Tasks 1–7 | corelib/ | — (sequential within corelib) |
| Tasks 8–10, 12 | engine/, corelib/ | Tasks 1–7 |
| Task 11 | extension/ | Task 10 (D-Bus API) |
| Task 13 | importers/messenger/ | Task 5 (vectors file), Task 12 (format doc) |
| Task 14 | .github/, packaging | all |

Lane A: Tasks 1→10, 12 (shared corelib/engine). Lane B: Task 11 after Task 10. Lane C: Task 13 after Task 12. Execution order: subagent-driven development runs tasks sequentially by design (never parallel implementers), so: sequential implementation in plan order. Conflict flags: none beyond shared corelib/engine, which stay in one lane.

## Implementation Tasks
Synthesized from this review's findings. Each task derives from a specific finding above; all are already written into the plan tasks named.

- [ ] **T1 (P1, human: ~1 day / CC: ~20 min)** — Task 8 — Read librush as reference; capture IBusText golden and test it
  - Surfaced by: Scope Challenge S1 (D2) and Test review T1 (D9)
  - Files: engine/src/ibus.rs, scripts/gen-ibus-golden.py, engine/tests/golden/ibustext_build.json
  - Verify: `cargo test -p engine ibustext_matches_golden`
- [ ] **T2 (P1, human: ~3 days / CC: ~45 min)** — Tasks 3/4/5/7/8 — Trilingual lexicons, primary-language detection, data-driven language config
  - Surfaced by: Scope Challenge L1 (D4) and Code quality Q1
  - Files: corelib/src/dictionary.rs, scoring.rs, policy.rs; engine/src/state.rs, config.rs; scripts/build-lexicon.py
  - Verify: `cargo test --workspace`; `python3 -m unittest discover scripts`
- [ ] **T3 (P1, human: ~2h / CC: ~10 min)** — Task 3 — SCOWL word set with en_50k ranks + attribution
  - Surfaced by: Scope Challenge S2 (D3)
  - Files: scripts/build-lexicon.py, data/en_words.txt, data/ATTRIBUTION.md
  - Verify: `python3 -m unittest scripts/test_build_lexicon.py`
- [ ] **T4 (P1, human: ~1 day / CC: ~15 min)** — Task 3 — Prefix-7 hashed SymSpell index with size budget
  - Surfaced by: Performance P1 (D11)
  - Files: corelib/src/dictionary.rs
  - Verify: `cargo test -p corelib index_size_budget long_word_edits_found`
- [ ] **T5 (P2, human: ~1 day / CC: ~20 min)** — Tasks 1/4/6/7 — Learn slips from backspace fixes; vocab keyed by word
  - Surfaced by: Architecture A1 (D8)
  - Files: corelib/src/tokenizer.rs, scoring.rs, learning.rs, db.rs; engine/src/state.rs
  - Verify: `cargo test --workspace slips_roundtrip backspace_fix_records_slip personal_slip_lowers_cost`
- [ ] **T6 (P1, human: ~1 day / CC: ~30 min)** — Task 10/14 — Headless IBus E2E harness in CI
  - Surfaced by: Test review T2 (D10)
  - Files: scripts/e2e.sh, tests/e2e/ibus_roundtrip.py, .github/workflows/ci.yml
  - Verify: `bash scripts/e2e.sh`
- [ ] **T7 (P2, human: ~1.5 days / CC: ~30 min)** — Tasks 12/13 — Vocab v1 format + engine import; separate Messenger importer
  - Surfaced by: Scope Challenge L2 (D7) and red flag 8
  - Files: docs/vocab-format.md, corelib/src/vocab.rs, engine/src/control.rs, importers/messenger/, data/learnable-vectors.tsv
  - Verify: `cargo test --workspace`; `python3 -m unittest discover importers/messenger`
- [ ] **T8 (P2, human: ~30min / CC: ~5 min)** — Task 1 — MIT OR Apache-2.0 license files and metadata
  - Surfaced by: Scope Challenge S3 (D6)
  - Files: LICENSE-MIT, LICENSE-APACHE, Cargo.toml
  - Verify: `cargo metadata --format-version 1 | grep -c '"license":"MIT OR Apache-2.0"'`

### Unresolved decisions

None in this review.

### Completion summary
- Step 0: Scope Challenge — scope accepted as-is
- Reality rows: 7/7 applicable rows emitted (`PLAN_REALITY: phase=eng rows=7/7 missing=none verdict=complete`), 0 unverified premises
- Architecture Review: 1 issue found (A1)
- Code Quality Review: 1 issue found (Q1)
- Test Review: diagram produced, 3 gaps identified (T1, T2, T3)
- Performance Review: 1 issue found (P1)
- NOT in scope: written
- What already exists: written
- TODOS.md updates: 2 items proposed to user (1 added, 1 skipped)
- Failure modes: 0 critical gaps flagged
- Unresolved decisions: 0 in this review
- Outside voice: codex, unavailable (CLI not installed; native fallback produced no report within 5 minutes)
- Parallelization: 3 lanes, 0 parallel / 3 sequential (subagent-driven development is sequential)
- Lake Score: 0/10 = answers picking a 10/10 option / answers scored for Completeness (no option was scored 10/10; the highest-completeness option was chosen in 7 of 10 scored answers: D3, D4, D7, D8, D9, D10, D11)

Suppressed findings (appendix):
- [INFORMATIONAL] (confidence 4) install-dev.sh and the .deb both write /usr/share/ibus/component/correctiond.xml; dpkg replaces an unowned file, so impact is low.

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` | Scope & strategy | 0 | — | — |
| Outside Review | codex (plan-eng-review outside voice) | Independent 2nd opinion | 1 | unavailable | codex not installed; native fallback timed out |
| Eng Review | `/plan-eng-review` | Architecture & tests (required) | 1 | ISSUES OPEN | 6 issues, 0 critical gaps (all resolved into plan tasks; 5 Scope Challenge findings reported separately) |
| Design Review | `/plan-design-review` | UI/UX gaps | 0 | — | — |
| DX Review | `/plan-devex-review` | Developer experience gaps | 0 | — | — |

- **OUTSIDE COVERAGE:** codex, plan-review phase, unavailable (CLI not installed; native Plan-agent fallback produced no complete report within five minutes); no outside findings.
- **VERDICT:** no reviews CLEAR yet — Eng Review logged as issues_open because findings were found (all 6 resolved into the plan with your approval); eng review required. Run `/plan-eng-review` again for a clean pass if you want a CLEARED dashboard.

NO UNRESOLVED DECISIONS
