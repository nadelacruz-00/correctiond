# TODOS

## Language

### Support non-ASCII words (ñ and accented letters)

**What:** Let lexicons, correction and learning handle words containing ñ and other non-ASCII letters.

**Why:** Tagalog words such as "niño" are currently dropped from every lexicon, so they are never protected or corrected.

**Context:** v1 drops non-ASCII words because some IBus clients report surrounding-text offsets in bytes instead of characters on Wayland (ibus#2570 and the octet-offset note in docs/designs/correction-service.md), so a delete could hit the wrong span. Start at `parse_lexicon` (corelib/src/dictionary.rs, Task 3) and the verify-before-delete path (engine state, Task 7): detect per app whether offsets are bytes or chars from SetSurroundingText, then allow non-ASCII only where it is known. Eng review D12.

**Effort:** M
**Priority:** P3
**Depends on:** v1 shipped and the surrounding-text matrix (spec Assignment) recorded in docs/designs/notes.md

### Correct `teh` → `the` with the shipped word list

**What:** Make `decide` (corelib/src/scoring.rs) fix `teh`. Today `ten` (h→n, adjacent keys, dl 0.40) scores within 0.171 of `the` (swap, dl 0.70), below `MARGIN_FOCUSED` 0.3, so nothing is fixed. Options: prefer a swap over a one-key slip when the swapped word is much more frequent, or give frequency more weight than `0.2 × log10`. Test against `data/en_words.txt`, not only the fixture, and check that no other words get worse (also at `MARGIN_OPEN` 0.6, where `bulid`, `recieve`, `adn` and `hte` are not fixed either).
**Why:** `teh` is the most common English typo and the first word most people try. The plan and the README use `bulid` until this is fixed (Docker check, docs/designs/notes.md, 2026-11-08).
**Priority:** P2

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

## Roadmap (from the 2026-11-08 "Lean install and optional modules" review)

Deferred from /office-hours and /autoplan (CEO, design, DX, eng). Plan: `docs/superpowers/plans/2026-11-08-lean-install-and-optional-modules.md`.

### Autocomplete (inline ghost text)

**What:** A stage that offers a completion from learned word pairs and vocabulary as preedit after the cursor, accepted with Tab or Right, in allowlisted apps only.
**Why:** The user's next headline feature; the learned word pairs already exist.
**Context:** GNOME on Wayland may ignore preedit styling (reported for ibus-typing-booster), so prototype in Firefox and GTK4 before designing the UX. Decide the engine Stage seam here (deferred from this round if the gate accepts that).
**Priority:** P1

### GNOME built-in on-screen keyboard support

**What:** Watch `Main.keyboard` visibility and send the same touch state as the GJS OSK watcher, turning `osk.js` into a provider interface.
**Why:** Tablet and convertible users on the stock OSK are a larger audience than GJS OSK users; per-layout touch geometry is correctiond's differentiator.
**Context:** Raised by the independent CEO voice (F3). Order it against autocomplete.
**Priority:** P1

### Older GNOME (46-49)

**What:** Widen `shell-version` after testing the extension on each version (Quick Settings and `extensionManager` APIs).
**Why:** Ubuntu 24.04 LTS ships GNOME 46.
**Priority:** P2

### Package repositories for automatic updates

**What:** Publish through a Launchpad PPA, Fedora COPR and/or openSUSE OBS.
**Why:** Loose GitHub downloads never update, so security fixes don't reach users.
**Priority:** P2

### Engine version in GetStatus and a version-skew hint

**What:** Report the engine version over D-Bus; the extension shows "engine is older than this extension" in the setup guidance.
**Why:** EGO updates the extension independently of the engine package.
**Priority:** P2

### KDE Plasma / Fcitx5

**What:** A Fcitx5 frontend over `corelib`, plus a focused-app provider (KWin script) in place of the GNOME extension.
**Priority:** P3

### Non-GNOME IBus (Sway, Hyprland, XFCE, X11)

**What:** Correction without the GNOME extension: a generic focused-app provider, or an opt-in "correct everywhere except sensitive fields" mode.
**Priority:** P3

### Import as an optional module

**What:** Gate `engine import` and `ImportVocabulary` behind a module, with a new fixed `IMPORT_ERRORS` text and a decision on the Learning page's import report.
**Priority:** P3

### One-click "Add input source"

**What:** A Settings button that adds `correctiond:us` to `org.gnome.desktop.input-sources` with explicit consent.
**Why:** Removes the last manual install step; deferred because silently editing input sources is intrusive.
**Priority:** P3

### AUR PKGBUILD

**What:** A PKGBUILD built from the release tarball.
**Priority:** P3

### User-local (no-root) install

**What:** Engine under `~/.local` plus `IBUS_COMPONENT_PATH` in `~/.config/environment.d/`, for immutable distros and users without sudo.
**Priority:** P3
