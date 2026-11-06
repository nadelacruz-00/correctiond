# TODOS

## Language

### Support non-ASCII words (ñ and accented letters)

**What:** Let lexicons, correction and learning handle words containing ñ and other non-ASCII letters.

**Why:** Tagalog words such as "niño" are currently dropped from every lexicon, so they are never protected or corrected.

**Context:** v1 drops non-ASCII words because some IBus clients report surrounding-text offsets in bytes instead of characters on Wayland (ibus#2570 and the octet-offset note in docs/designs/correction-service.md), so a delete could hit the wrong span. Start at `parse_lexicon` (corelib/src/dictionary.rs, Task 3) and the verify-before-delete path (engine state, Task 7): detect per app whether offsets are bytes or chars from SetSurroundingText, then allow non-ASCII only where it is known. Eng review D12.

**Effort:** M
**Priority:** P3
**Depends on:** v1 shipped and the surrounding-text matrix (spec Assignment) recorded in docs/designs/notes.md

## Deferred from the final review (2026-11-04)

Findings from the v1 final review left for after v1.

### M3: redacting `Debug` on word-bearing rows

**What:** Replace the derived `Debug` on store rows that hold words with a hand-written one that prints counts, not words.
**Why:** Not reachable today (no non-test `{:?}`, and the panic hook prints only file:line), so this is hardening against a future `unwrap` or log line.
**Priority:** P3

### M4: `StoreKey::generate` returns `Result`

**What:** Return an error instead of panicking when the OS RNG fails (corelib/src/key.rs), and report it as `KeyState::Unavailable`.
**Why:** An RNG failure kills the persistence actor silently and leaves the store memory-only until restart; rare, but it should be visible.
**Priority:** P3

### M5: un-record a reverted correction

**What:** Defer `record_word` of a corrected word until the revert window closes, or decrement it on revert (engine/src/state.rs).
**Why:** Personal vocabulary slowly gains words the user explicitly rejected; low impact because a rejected pair is also blacklisted.
**Priority:** P3

### M6: timeout on Purge's keyring delete

**What:** Run Purge's keyring delete under `step` or a timeout (engine/src/persist.rs).
**Why:** A hung Secret Service stalls the actor and the Purge reply; the extension then shows "not running" after the D-Bus timeout.
**Priority:** P3

### M7: `try_acquire` for imports

**What:** Refuse a second concurrent `ImportVocabulary` with a fixed "busy" text instead of queueing it (engine/src/control.rs).
**Why:** Each queued call keeps its fd open; only a same-user caller can queue them, so this is tidiness, not a leak.
**Priority:** P3
