//! Per-variant engine state machine: classified keys in, actions out.
//!
//! Pure logic, no D-Bus. Runs on every keystroke, so it never panics and
//! never blocks: the store mutex is held only for a single `decide` or
//! `record_*` call.

use corelib::dictionary::Dictionary;
use corelib::layout::LayoutId;
use corelib::learning::{LearnStore, SharedStore};
use corelib::policy::{
    bigram_learnable, effective_layout, is_learnable, terminal_token_ok, LangWindow, Mode, ProseRun,
};
use corelib::scoring::decide;
use corelib::tokenizer::{KeyClass, WordBuffer};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// `LangId` is an index into at most 16 languages. Sizing the window to all
/// 16 is equivalent to the dictionary's own count: candidates and
/// frequencies only ever carry languages that exist in the dictionary.
const MAX_LANGS: usize = 16;

/// What the IBus layer does with the key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Return the event unhandled.
    Pass,
    /// Delete `delete_chars` before the cursor, then commit `commit`.
    /// `consume`: the triggering key is swallowed (its char is inside
    /// `commit`); false means the real key must still reach the app.
    Replace {
        delete_chars: u32,
        commit: String,
        consume: bool,
    },
}

/// Process-wide correction statistics, shared by both layout variants.
#[derive(Debug, Default)]
pub struct Counters {
    pub corrections: AtomicU64,
    pub reverts: AtomicU64,
}

/// The last correction, revertible by the very next Backspace.
#[derive(Debug)]
struct RevertWindow {
    /// The token as typed, periods stripped (blacklist key).
    orig: String,
    /// The replacement word (blacklist value).
    fixed: String,
    /// Exactly what the correction committed: fixed + periods + boundary.
    committed: String,
    /// What the revert commits: typed buffer + boundary.
    original: String,
}

pub struct EngineState {
    dict: Arc<Dictionary>,
    store: SharedStore,
    counters: Arc<Counters>,
    variant: LayoutId,
    buf: WordBuffer,
    prose: ProseRun,
    lang: LangWindow,
    /// The previous committed word, for bigram context.
    prev: Option<String>,
    revert: Option<RevertWindow>,
    /// Char removed by the last mid-word Backspace (slip learning).
    removed: Option<char>,
    /// Off: nothing is written to the store (replay evaluation). Reads,
    /// corrections and the revert window are unchanged.
    learning: bool,
    /// Why the last word boundary did or did not correct, and the language
    /// set it considered (bitmask): for the opt-in trace, never typed text.
    pub last_outcome: &'static str,
    pub last_allowed: u16,
}

fn char_len(s: &str) -> u32 {
    u32::try_from(s.chars().count()).unwrap_or(u32::MAX)
}

/// Letters (and in-word apostrophes) only: no paths, flags or identifiers.
fn plain_word(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_alphabetic() || c == '\'')
}

impl EngineState {
    pub fn new(
        dict: Arc<Dictionary>,
        store: SharedStore,
        counters: Arc<Counters>,
        variant: LayoutId,
    ) -> EngineState {
        EngineState {
            dict,
            store,
            counters,
            variant,
            buf: WordBuffer::default(),
            prose: ProseRun::default(),
            lang: LangWindow::new(MAX_LANGS),
            prev: None,
            revert: None,
            removed: None,
            learning: true,
            last_outcome: "",
            last_allowed: 0,
        }
    }

    /// Learning is on for every new state; the replay evaluator turns it
    /// off so a run cannot change what the next run sees.
    pub fn set_learning(&mut self, on: bool) {
        self.learning = on;
    }

    /// Runs `f` with the store locked; the guard drops before returning.
    fn with_store<R>(&self, f: impl FnOnce(&mut LearnStore) -> R) -> R {
        let mut g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut g)
    }

    /// Drops everything tied to the current cursor position.
    pub fn on_invalidate(&mut self) {
        self.buf.clear();
        self.prose.reset();
        self.lang.reset();
        self.prev = None;
        self.revert = None;
        self.removed = None;
    }

    pub fn on_key(
        &mut self,
        k: KeyClass,
        mode: Mode,
        osk_visible: bool,
        surrounding: Option<&str>,
    ) -> Action {
        // Any key closes the revert window; only an immediate Backspace uses it.
        if let Some(w) = self.revert.take() {
            if k == KeyClass::Backspace {
                if let Some(a) = self.revert(w, mode, surrounding) {
                    return a;
                }
            }
        }
        let removed = self.removed.take();
        match k {
            KeyClass::Printable(c) => {
                if let Some(r) = removed {
                    self.maybe_record_slip(r, c, mode, osk_visible);
                }
                self.buf.push(c);
                Action::Pass
            }
            KeyClass::Backspace => {
                match self.buf.pop() {
                    Some(c) => self.removed = Some(c),
                    None => self.on_invalidate(),
                }
                Action::Pass
            }
            KeyClass::Invalidate => {
                self.on_invalidate();
                Action::Pass
            }
            KeyClass::Boundary(b) => self.on_boundary(b, mode, osk_visible, surrounding),
        }
    }

    /// Undo the last correction. `None` means verification failed and the
    /// Backspace falls through to ordinary handling.
    fn revert(&mut self, w: RevertWindow, mode: Mode, surrounding: Option<&str>) -> Option<Action> {
        if mode == Mode::Off {
            return None;
        }
        // Never learn in terminals: a terminal token may be a passphrase.
        if mode == Mode::Silent && self.learning {
            self.with_store(|s| s.record_blacklist(&w.orig, &w.fixed));
        }
        if surrounding.is_some_and(|s| !s.ends_with(&w.committed)) {
            return None;
        }
        self.counters.reverts.fetch_add(1, Ordering::Relaxed);
        self.removed = None;
        self.prev = Some(w.orig);
        Some(Action::Replace {
            delete_chars: char_len(&w.committed),
            commit: w.original,
            consume: true,
        })
    }

    fn maybe_record_slip(&self, removed: char, typed: char, mode: Mode, osk_visible: bool) {
        if !self.learning
            || !matches!(mode, Mode::Silent | Mode::Observe)
            || !removed.is_ascii_alphabetic()
            || !typed.is_ascii_alphabetic()
            || removed.eq_ignore_ascii_case(&typed)
        {
            return;
        }
        let layout = effective_layout(self.variant, osk_visible && cfg!(feature = "touch"));
        self.with_store(|s| s.record_slip(layout, removed, typed));
    }

    fn on_boundary(
        &mut self,
        b: Option<char>,
        mode: Mode,
        osk_visible: bool,
        surrounding: Option<&str>,
    ) -> Action {
        let charless_terminal = mode == Mode::ProseAware && b.is_none();
        if charless_terminal {
            self.prose.reset();
        }
        self.last_allowed = 0;
        if self.buf.word().is_empty() {
            self.last_outcome = "empty";
            return Action::Pass;
        }
        let typed = self.buf.word().to_string();
        self.buf.clear();
        if mode == Mode::Off {
            self.last_outcome = "off";
            // Keep nothing from a sensitive field, not even bigram context.
            self.prev = None;
            return Action::Pass;
        }
        let token = typed.trim_end_matches('.');
        let periods = &typed[token.len()..];

        let may_correct = plain_word(token)
            && match mode {
                Mode::Silent => true,
                Mode::ProseAware => {
                    !charless_terminal && self.prose.ready() && terminal_token_ok(token)
                }
                Mode::Off | Mode::Observe => false,
            };
        self.last_outcome = match mode {
            Mode::Observe => "observe",
            _ if !plain_word(token) => "not-plain",
            _ if !may_correct => "prose-gate",
            _ => "no-candidate",
        };
        let verdict = if may_correct {
            let ctx = self.lang.ctx();
            self.last_allowed = ctx.allowed.0;
            // Tap typos follow the OSK geometry, whatever engine variant is on.
            let layout = effective_layout(self.variant, osk_visible && cfg!(feature = "touch"));
            let prev = self.prev.as_deref();
            let dict = &*self.dict;
            self.with_store(|s| decide(token, prev, layout, &ctx, dict, &*s))
                .filter(|v| v.replacement != token)
        } else {
            None
        };

        let (action, committed_word) = match verdict {
            Some(v) => {
                if surrounding.is_some_and(|s| !s.ends_with(&typed)) {
                    // The app's text disagrees with the buffer: touch and
                    // record nothing, and stop trusting this position.
                    self.on_invalidate();
                    self.last_outcome = "text-mismatch";
                    return Action::Pass;
                }
                let mut commit = format!("{}{periods}", v.replacement);
                if let Some(c) = b {
                    commit.push(c);
                    self.revert = Some(RevertWindow {
                        orig: token.to_string(),
                        fixed: v.replacement.clone(),
                        committed: commit.clone(),
                        original: format!("{typed}{c}"),
                    });
                }
                self.counters.corrections.fetch_add(1, Ordering::Relaxed);
                self.last_outcome = if surrounding.is_some() {
                    "corrected"
                } else {
                    "corrected-unverified"
                };
                let a = Action::Replace {
                    delete_chars: char_len(&typed),
                    commit,
                    consume: b.is_some(),
                };
                (a, v.replacement)
            }
            None => (Action::Pass, token.to_string()),
        };

        if !charless_terminal {
            self.prose.on_word_completed(token);
        }
        self.learn(&committed_word, mode);
        if let Some(l) = self.dict.primary_lang(&committed_word) {
            self.lang.push(l);
        }
        self.prev = Some(committed_word);
        action
    }

    /// Store writes for a completed word, gated by the data-minimization rules.
    fn learn(&self, word: &str, mode: Mode) {
        if !self.learning || !is_learnable(word, mode) {
            return;
        }
        let in_dict = self.dict.contains(word);
        self.with_store(|s| {
            if s.record_word(word) == 2 && !in_dict {
                s.note_learned(word);
            }
        });
        if !bigram_learnable(mode) {
            return;
        }
        if let Some(p) = self.prev.as_deref().filter(|p| is_learnable(p, mode)) {
            self.with_store(|s| s.bump_bigram(p, word));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corelib::dictionary::{Dictionary, LangId};
    #[cfg(feature = "touch")]
    use corelib::layout::LayoutId::OskTap;
    use corelib::layout::LayoutId::{self, Colemak, Qwerty};
    use corelib::learning::LearnStore;
    use corelib::policy::Mode::{self, Observe, Off, ProseAware, Silent};
    use corelib::scoring::PersonalLookup;
    use corelib::tokenizer::KeyClass::{self, Backspace, Boundary, Printable};
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    const EN: LangId = LangId(0);
    const TL: LangId = LangId(1);
    const WAR: LangId = LangId(2);
    const SP: KeyClass = Boundary(Some(' '));
    const RET: KeyClass = Boundary(None);

    fn lex(items: &[(&str, u64)]) -> Vec<(String, u64)> {
        items.iter().map(|(w, c)| (w.to_string(), *c)).collect()
    }

    /// Task 3 fixture lexicons.
    fn dict() -> Arc<Dictionary> {
        Arc::new(Dictionary::from_lexicons(vec![
            (
                EN,
                lex(&[
                    ("the", 1000),
                    ("build", 50),
                    ("building", 30),
                    ("hello", 80),
                    ("may", 40),
                ]),
            ),
            (TL, lex(&[("ako", 500), ("kami", 40), ("may", 300)])),
            (WAR, lex(&[("maupay", 60), ("ako", 20)])),
        ]))
    }

    struct T {
        e: EngineState,
        store: SharedStore,
        counters: Arc<Counters>,
    }

    fn new_with(variant: LayoutId) -> T {
        let store: SharedStore = Arc::new(Mutex::new(LearnStore::memory()));
        let counters = Arc::new(Counters::default());
        T {
            e: EngineState::new(dict(), store.clone(), counters.clone(), variant),
            store,
            counters,
        }
    }

    fn new() -> T {
        new_with(Qwerty)
    }

    impl T {
        fn key(&mut self, k: KeyClass, mode: Mode, sur: Option<&str>) -> Action {
            self.e.on_key(k, mode, false, sur)
        }
        /// Types `w` as Printable keys; each must Pass.
        fn typ(&mut self, w: &str, mode: Mode) {
            for c in w.chars() {
                assert_eq!(self.key(Printable(c), mode, None), Action::Pass, "{c}");
            }
        }
        /// Types `w` then the boundary `b`; returns the boundary's action.
        fn word(&mut self, w: &str, b: KeyClass, mode: Mode, sur: Option<&str>) -> Action {
            self.typ(w, mode);
            self.key(b, mode, sur)
        }
        fn freq(&self, w: &str) -> u64 {
            self.store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .personal_freq(w)
        }
        fn blacklisted(&self, o: &str, c: &str) -> bool {
            self.store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_blacklisted(o, c)
        }
        fn bigram(&self, a: &str, b: &str) -> u64 {
            self.store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .bigram_count(a, b)
        }
        fn slips(&self, l: LayoutId, t: char, i: char) -> u64 {
            self.store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .slip_count(l, t, i)
        }
    }

    fn rep(d: u32, s: &str, consume: bool) -> Action {
        Action::Replace {
            delete_chars: d,
            commit: s.to_string(),
            consume,
        }
    }

    #[test]
    fn silent_fix_at_boundary() {
        let mut t = new();
        let a = t.word("bulid", SP, Silent, Some("bulid"));
        assert_eq!(a, rep(5, "build ", true));
        // Unknown surrounding text also corrects.
        let a = t.word("bulid", SP, Silent, None);
        assert_eq!(a, rep(5, "build ", true));
        // Capitalization preserved; other boundary chars committed.
        let a = t.word("Bulid", Boundary(Some(',')), Silent, Some("x Bulid"));
        assert_eq!(a, rep(5, "Build,", true));
    }

    #[test]
    fn verify_mismatch_passes() {
        let mut t = new();
        assert_eq!(t.word("bulid", SP, Silent, Some("xxxxx")), Action::Pass);
        assert_eq!(t.freq("bulid"), 0);
        assert_eq!(t.freq("build"), 0);
        assert_eq!(t.counters.corrections.load(Ordering::Relaxed), 0);
        // No revert window either.
        assert_eq!(t.key(Backspace, Silent, Some("bulid ")), Action::Pass);
        assert!(!t.blacklisted("bulid", "build"));
        // Same rule for a char-less boundary.
        assert_eq!(t.word("bulid", RET, Silent, Some("bulix")), Action::Pass);
    }

    #[test]
    fn trailing_period_stripped() {
        let mut t = new();
        let a = t.word("bulid.", SP, Silent, Some("bulid."));
        assert_eq!(a, rep(6, "build. ", true));
        let a = t.word("bulid...", SP, Silent, None);
        assert_eq!(a, rep(8, "build... ", true));
        for w in [
            "file.txt", "bu.lid", "a/bulid", "bu-lid", "bulid2", "bulid2.",
        ] {
            assert_eq!(t.word(w, SP, Silent, None), Action::Pass, "{w}");
        }
    }

    #[test]
    fn return_boundary_commits_without_char() {
        let mut t = new();
        assert_eq!(
            t.word("bulid", RET, Silent, Some("bulid")),
            rep(5, "build", false)
        );
        assert_eq!(t.word("bulid.", RET, Silent, None), rep(6, "build.", false));
        assert_eq!(t.counters.corrections.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn no_revert_window_after_charless_boundary() {
        let mut t = new();
        assert_eq!(t.word("bulid", RET, Silent, None), rep(5, "build", false));
        assert_eq!(t.key(Backspace, Silent, Some("build\n")), Action::Pass);
        assert!(!t.blacklisted("bulid", "build"));
        assert_eq!(t.counters.reverts.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn revert_consumes_and_restores() {
        let mut t = new();
        assert_eq!(
            t.word("bulid", SP, Silent, Some("bulid")),
            rep(5, "build ", true)
        );
        let a = t.key(Backspace, Silent, Some("build "));
        assert_eq!(a, rep(6, "bulid ", true));
        assert!(t.blacklisted("bulid", "build"));
        // The pair never fires again.
        assert_eq!(t.word("bulid", SP, Silent, None), Action::Pass);
    }

    #[test]
    fn revert_deletes_the_correction_length() {
        // "buld" -> "build ": the correction grew the word by one char.
        let mut t = new();
        assert_eq!(t.word("buld", SP, Silent, None), rep(4, "build ", true));
        assert_eq!(t.key(Backspace, Silent, None), rep(6, "buld ", true));
        // Stripped periods and the boundary char are restored too.
        let mut t = new();
        assert_eq!(
            t.word("buld.", Boundary(Some('!')), Silent, None),
            rep(5, "build.!", true)
        );
        assert_eq!(
            t.key(Backspace, Silent, Some("x build.!")),
            rep(7, "buld.!", true)
        );
        assert!(t.blacklisted("buld", "build"));
    }

    #[test]
    fn revert_verify_mismatch_still_blacklists() {
        let mut t = new();
        assert_eq!(t.word("bulid", SP, Silent, None), rep(5, "build ", true));
        assert_eq!(t.key(Backspace, Silent, Some("bulid ")), Action::Pass);
        assert!(t.blacklisted("bulid", "build"));
        assert_eq!(t.counters.reverts.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn terminal_revert_does_not_blacklist() {
        let mut t = new();
        t.word("hello", SP, ProseAware, None);
        t.word("hello", SP, ProseAware, None);
        assert_eq!(
            t.word("bulid", SP, ProseAware, None),
            rep(5, "build ", true)
        );
        let a = t.key(Backspace, ProseAware, Some("build "));
        assert_eq!(a, rep(6, "bulid ", true));
        assert!(!t.blacklisted("bulid", "build"));
        assert_eq!(t.counters.reverts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn revert_window_closes_on_next_key() {
        let mut t = new();
        assert_eq!(t.word("bulid", SP, Silent, None), rep(5, "build ", true));
        assert_eq!(t.key(Printable('x'), Silent, None), Action::Pass);
        assert_eq!(t.key(Backspace, Silent, None), Action::Pass);
        assert!(!t.blacklisted("bulid", "build"));
        // A second Backspace after a revert is ordinary too.
        let mut t = new();
        t.word("bulid", SP, Silent, None);
        assert_eq!(t.key(Backspace, Silent, None), rep(6, "bulid ", true));
        assert_eq!(t.key(Backspace, Silent, None), Action::Pass);
        assert_eq!(t.counters.reverts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn midword_backspace_pops() {
        let mut t = new();
        t.typ("buli", Silent);
        assert_eq!(t.key(Backspace, Silent, None), Action::Pass);
        t.typ("d", Silent);
        // decide sees "buld" (4 chars), not "bulid".
        assert_eq!(t.key(SP, Silent, Some("buld")), rep(4, "build ", true));
    }

    #[test]
    fn empty_backspace_invalidates() {
        // A ready prose-run would let "bulid" be corrected...
        let mut t = new();
        t.word("hello", SP, ProseAware, None);
        t.word("hello", SP, ProseAware, None);
        t.typ("x", ProseAware);
        assert_eq!(t.key(Backspace, ProseAware, None), Action::Pass);
        // ...but a Backspace with an empty buffer invalidates (resetting it).
        assert_eq!(t.key(Backspace, ProseAware, None), Action::Pass);
        assert_eq!(t.word("bulid", SP, ProseAware, None), Action::Pass);
        // Mid-word pops alone do not invalidate.
        let mut t = new();
        t.word("hello", SP, ProseAware, None);
        t.word("hello", SP, ProseAware, None);
        t.typ("bulidx", ProseAware);
        assert_eq!(t.key(Backspace, ProseAware, None), Action::Pass);
        assert_eq!(t.key(SP, ProseAware, None), rep(5, "build ", true));
    }

    #[test]
    fn empty_boundary_noop() {
        let mut t = new();
        assert_eq!(t.key(SP, Silent, None), Action::Pass);
        assert_eq!(t.key(RET, Silent, None), Action::Pass);
        assert_eq!(t.key(Boundary(Some(',')), Silent, Some("")), Action::Pass);
        // A boundary after a word leaves no residue for the next one.
        assert_eq!(t.word("bulid", SP, Silent, None), rep(5, "build ", true));
        assert_eq!(t.key(SP, Silent, None), Action::Pass);
        assert_eq!(t.counters.corrections.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn focus_invalidate_drops_pending() {
        let mut t = new();
        t.typ("bul", Silent);
        t.e.on_invalidate();
        assert_eq!(t.word("id", SP, Silent, None), Action::Pass);
        t.typ("bul", Silent);
        assert_eq!(t.key(KeyClass::Invalidate, Silent, None), Action::Pass);
        assert_eq!(t.word("id", SP, Silent, None), Action::Pass);
        // Invalidation also closes the revert window.
        t.word("bulid", SP, Silent, None);
        t.e.on_invalidate();
        assert_eq!(t.key(Backspace, Silent, None), Action::Pass);
        assert!(!t.blacklisted("bulid", "build"));
    }

    #[test]
    fn prose_aware_gates() {
        let mut t = new();
        // Prose-run not ready: the typo passes untouched.
        assert_eq!(t.word("bulid", SP, ProseAware, None), Action::Pass);
        assert_eq!(t.word("hello", SP, ProseAware, None), Action::Pass);
        // Two clean words completed: the third is corrected.
        assert_eq!(
            t.word("bulid", SP, ProseAware, None),
            rep(5, "build ", true)
        );
        t.e.on_invalidate();
        assert_eq!(t.word("hello", SP, ProseAware, None), Action::Pass);
        assert_eq!(t.word("bulid", SP, ProseAware, None), Action::Pass);
        // A command-shaped token resets the run.
        let mut t = new();
        t.word("hello", SP, ProseAware, None);
        t.word("hello", SP, ProseAware, None);
        t.word("-m", SP, ProseAware, None);
        assert_eq!(t.word("bulid", SP, ProseAware, None), Action::Pass);
    }

    #[test]
    fn prose_aware_never_corrects_at_charless_boundary() {
        let mut t = new();
        t.word("hello", SP, ProseAware, None);
        t.word("hello", SP, ProseAware, None);
        assert_eq!(t.word("bulid", RET, ProseAware, None), Action::Pass);
        // The Return also reset the prose-run.
        t.word("hello", SP, ProseAware, None);
        assert_eq!(t.word("bulid", SP, ProseAware, None), Action::Pass);
        // Even an empty-buffer Return resets it.
        let mut t = new();
        t.word("hello", SP, ProseAware, None);
        t.word("hello", SP, ProseAware, None);
        assert_eq!(t.key(RET, ProseAware, None), Action::Pass);
        assert_eq!(t.word("bulid", SP, ProseAware, None), Action::Pass);
        assert_eq!(t.counters.corrections.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn observe_never_acts() {
        let mut t = new();
        assert_eq!(t.word("bulid", SP, Observe, Some("bulid")), Action::Pass);
        assert_eq!(t.word("hello", RET, Observe, None), Action::Pass);
        assert_eq!(t.freq("bulid"), 1);
        assert_eq!(t.freq("hello"), 1);
        assert_eq!(t.bigram("bulid", "hello"), 1);
        assert_eq!(t.counters.corrections.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn off_mode_records_nothing() {
        let mut t = new();
        for w in ["bulid", "gstack", "hello", "gstack"] {
            assert_eq!(t.word(w, SP, Off, Some(w)), Action::Pass);
        }
        assert_eq!(t.key(Backspace, Off, None), Action::Pass);
        for w in ["bulid", "gstack", "hello"] {
            assert_eq!(t.freq(w), 0, "{w}");
        }
        assert_eq!(t.bigram("gstack", "hello"), 0);
        assert_eq!(t.bigram("hello", "gstack"), 0);
        // An Off word is never the first half of a later bigram.
        t.word("gstack", SP, Off, None);
        t.word("hello", SP, Observe, None);
        assert_eq!(t.bigram("gstack", "hello"), 0);
    }

    #[test]
    fn learning_records_committed_word_and_bigrams() {
        let mut t = new();
        t.word("hello", SP, Silent, None);
        // The corrected form is what is learned.
        assert_eq!(t.word("bulid", SP, Silent, None), rep(5, "build ", true));
        assert_eq!(t.freq("build"), 1);
        assert_eq!(t.freq("bulid"), 0);
        assert_eq!(t.bigram("hello", "build"), 1);
        // A non-learnable word breaks the bigram chain on both sides.
        t.word("hunter2", SP, Silent, None);
        t.word("hello", SP, Silent, None);
        assert_eq!(t.bigram("build", "hunter2"), 0);
        assert_eq!(t.bigram("hunter2", "hello"), 0);
        assert_eq!(t.bigram("build", "hello"), 0);
        // Trailing periods are not part of the learned word.
        t.word("gstack.", SP, Silent, None);
        assert_eq!(t.freq("gstack"), 1);
        assert_eq!(t.bigram("hello", "gstack"), 1);
    }

    fn recent(t: &T) -> Vec<String> {
        t.store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recent_words()
    }

    #[test]
    fn typing_a_new_word_twice_adds_it_to_recent() {
        let mut t = new();
        t.word("kadto", SP, Silent, None);
        assert!(recent(&t).is_empty());
        t.word("kadto", SP, Silent, None);
        assert_eq!(recent(&t), ["kadto"]);
        t.word("kadto", SP, Silent, None);
        assert_eq!(recent(&t), ["kadto"]);
    }

    #[test]
    fn dictionary_word_never_enters_recent() {
        // "hello" stands in for the brief's "house": the fixture dictionary has no "house".
        let mut t = new();
        t.word("hello", SP, Silent, None);
        t.word("hello", SP, Silent, None);
        // Learned twice (the second-sighting rule is met), so only the
        // dictionary check keeps it out of the list.
        assert_eq!(t.freq("hello"), 2);
        assert!(recent(&t).is_empty());
    }

    #[test]
    fn observe_mode_word_enters_recent() {
        let mut t = new();
        t.word("kadto", SP, Observe, None);
        t.word("kadto", SP, Observe, None);
        assert_eq!(recent(&t), ["kadto"]);
    }

    #[test]
    fn paused_learning_adds_nothing() {
        let mut t = new();
        t.e.set_learning(false);
        t.word("kadto", SP, Silent, None);
        t.word("kadto", SP, Silent, None);
        assert!(recent(&t).is_empty());
        assert_eq!(t.freq("kadto"), 0);
    }

    #[test]
    fn jargon_learned_in_two_sightings() {
        let mut t = new();
        assert_eq!(t.word("gstack", SP, Silent, None), Action::Pass);
        assert_eq!(t.word("gstack", RET, Silent, None), Action::Pass);
        assert_eq!(t.freq("gstack"), 2);
    }

    #[test]
    fn secret_shaped_tokens_not_learned() {
        let mut t = new();
        for w in ["hunter2", "KEY=abc123", "sk-abc"] {
            assert_eq!(t.word(w, SP, Silent, None), Action::Pass, "{w}");
            assert_eq!(t.freq(w), 0, "{w}");
        }
    }

    #[test]
    fn terminal_prompt_token_not_learned() {
        let mut t = new();
        assert_eq!(t.word("swordfish", RET, ProseAware, None), Action::Pass);
        assert_eq!(t.freq("swordfish"), 0);
    }

    #[test]
    fn no_learning_in_terminal() {
        let mut t = new();
        t.word("hello", SP, ProseAware, None);
        t.word("building", SP, ProseAware, None);
        t.word("hello", SP, ProseAware, None);
        assert_eq!(
            t.word("bulid", SP, ProseAware, None),
            rep(5, "build ", true)
        );
        for w in ["hello", "building", "build", "bulid"] {
            assert_eq!(t.freq(w), 0, "{w}");
        }
        assert_eq!(t.bigram("hello", "building"), 0);
        assert_eq!(t.bigram("building", "hello"), 0);
        assert_eq!(t.bigram("hello", "build"), 0);
    }

    #[test]
    fn variants_share_store() {
        let d = dict();
        let store: SharedStore = Arc::new(Mutex::new(LearnStore::memory()));
        let counters = Arc::new(Counters::default());
        let mut a = EngineState::new(d.clone(), store.clone(), counters.clone(), Colemak);
        let mut b = EngineState::new(d, store.clone(), counters.clone(), Qwerty);
        for _ in 0..2 {
            for c in "bulid".chars() {
                a.on_key(Printable(c), Observe, false, None);
            }
            a.on_key(SP, Observe, false, None);
        }
        assert_eq!(store.lock().unwrap().personal_freq("bulid"), 2);
        // Learned via the Colemak variant, so the Qwerty one keeps it.
        for c in "bulid".chars() {
            b.on_key(Printable(c), Silent, false, None);
        }
        assert_eq!(b.on_key(SP, Silent, false, None), Action::Pass);
        // Counters are shared too.
        let mut c2 = EngineState::new(dict(), store, counters.clone(), Qwerty);
        for c in "buld".chars() {
            c2.on_key(Printable(c), Silent, false, None);
        }
        assert_eq!(c2.on_key(SP, Silent, false, None), rep(4, "build ", true));
        assert_eq!(counters.corrections.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn filipino_sentence_blocks_english_fix() {
        let mut t = new();
        assert_eq!(t.word("ako", SP, Silent, None), Action::Pass);
        assert_eq!(t.word("kami", SP, Silent, None), Action::Pass);
        assert_eq!(t.word("bulid", SP, Silent, None), Action::Pass);
    }

    #[test]
    fn waray_sentence_fixes_waray_typo() {
        let mut t = new();
        t.word("maupay", SP, Silent, None);
        t.word("maupay", SP, Silent, None);
        assert_eq!(t.word("maupy", SP, Silent, None), rep(5, "maupay ", true));
    }

    #[test]
    fn unknown_words_push_no_language() {
        // Personal vocabulary and unknown words do not dilute the window.
        let mut t = new();
        t.word("ako", SP, Silent, None);
        t.word("kami", SP, Silent, None);
        t.word("gstack", SP, Silent, None);
        t.word("gstack", SP, Silent, None);
        t.word("zzzz", SP, Silent, None);
        assert_eq!(t.word("bulid", SP, Silent, None), Action::Pass);
    }

    #[test]
    fn invalidate_resets_language() {
        let mut t = new();
        t.word("ako", SP, Silent, None);
        t.word("kami", SP, Silent, None);
        t.e.on_invalidate();
        assert_eq!(t.word("bulid", SP, Silent, None), rep(5, "build ", true));
    }

    #[cfg(feature = "touch")]
    #[test]
    fn backspace_fix_records_slip() {
        let mut t = new_with(Colemak);
        t.typ("bn", Silent);
        assert_eq!(t.key(Backspace, Silent, None), Action::Pass);
        t.typ("ed", Silent);
        assert_eq!(t.slips(Colemak, 'n', 'e'), 1);
        assert_eq!(t.slips(Colemak, 'e', 'd'), 0);
        // Observe records too.
        t.e.on_invalidate();
        t.typ("bn", Observe);
        t.key(Backspace, Observe, None);
        t.typ("e", Observe);
        assert_eq!(t.slips(Colemak, 'n', 'e'), 2);
        // Off and ProseAware never record.
        for mode in [Off, ProseAware] {
            let mut t = new_with(Colemak);
            t.typ("bn", mode);
            t.key(Backspace, mode, None);
            t.typ("e", mode);
            assert_eq!(t.slips(Colemak, 'n', 'e'), 0, "{mode:?}");
        }
        // OSK visible on the retyped key => recorded under OskTap.
        let mut t = new_with(Colemak);
        t.typ("bn", Silent);
        t.key(Backspace, Silent, None);
        t.e.on_key(Printable('e'), Silent, true, None);
        assert_eq!(t.slips(OskTap, 'n', 'e'), 1);
        assert_eq!(t.slips(Colemak, 'n', 'e'), 0);
    }

    #[cfg(feature = "touch")]
    #[test]
    fn osk_taps_are_scored_with_osk_geometry() {
        // "hwllp" for "hello": w and p neighbor e and o on the QWERTY OSK
        // (2 x 0.3), but not on Colemak (2 x 1.0 > the 1.5 cap). The
        // Colemak engine stays selected while GJS OSK is up.
        let typo = |osk: bool| {
            let mut t = new_with(Colemak);
            for c in "hwllp".chars() {
                assert_eq!(t.e.on_key(Printable(c), Silent, osk, None), Action::Pass);
            }
            t.e.on_key(SP, Silent, osk, None)
        };
        assert!(
            matches!(typo(true), Action::Replace { ref commit, .. } if commit == "hello "),
            "OSK tap typo not fixed"
        );
        assert_eq!(typo(false), Action::Pass);
    }

    #[cfg(not(feature = "touch"))]
    #[test]
    fn osk_flag_is_ignored_without_touch() {
        // Same input as osk_taps_are_scored_with_osk_geometry: without the
        // touch feature the OSK flag changes nothing.
        let typo = |osk: bool| {
            let mut t = new_with(Colemak);
            for c in "hwllp".chars() {
                assert_eq!(t.e.on_key(Printable(c), Silent, osk, None), Action::Pass);
            }
            t.e.on_key(SP, Silent, osk, None)
        };
        assert_eq!(typo(true), typo(false));
    }

    #[test]
    fn slip_memory_rules() {
        // Case-insensitive same char: not a slip.
        let mut t = new_with(Colemak);
        t.typ("bn", Silent);
        t.key(Backspace, Silent, None);
        t.typ("N", Silent);
        assert_eq!(t.slips(Colemak, 'n', 'n'), 0);
        // Second Backspace keeps the most recently removed char.
        let mut t = new_with(Colemak);
        t.typ("bxn", Silent);
        t.key(Backspace, Silent, None);
        t.key(Backspace, Silent, None);
        t.typ("e", Silent);
        assert_eq!(t.slips(Colemak, 'x', 'e'), 1);
        assert_eq!(t.slips(Colemak, 'n', 'e'), 0);
        // Only the very next key counts.
        t.typ("f", Silent);
        assert_eq!(t.slips(Colemak, 'x', 'f'), 0);
        // Non-letters on either side: not recorded.
        let mut t = new_with(Colemak);
        t.typ("b1", Silent);
        t.key(Backspace, Silent, None);
        t.typ("e", Silent);
        t.key(Backspace, Silent, None);
        t.typ("2", Silent);
        assert_eq!(t.slips(Colemak, '1', 'e'), 0);
        assert_eq!(t.slips(Colemak, 'e', '2'), 0);
        // A boundary or invalidation clears the remembered char.
        let mut t = new_with(Colemak);
        t.typ("bn", Silent);
        t.key(Backspace, Silent, None);
        t.key(SP, Silent, None);
        t.typ("e", Silent);
        t.typ("bn", Silent);
        t.key(Backspace, Silent, None);
        t.e.on_invalidate();
        t.typ("e", Silent);
        t.typ("bn", Silent);
        t.key(Backspace, Silent, None);
        t.key(KeyClass::Invalidate, Silent, None);
        t.typ("e", Silent);
        assert_eq!(t.slips(Colemak, 'n', 'e'), 0);
    }

    #[test]
    fn learning_off_records_nothing() {
        let mut t = new();
        t.e.set_learning(false);
        t.word("hello", SP, Silent, None);
        t.word("gstack", SP, Silent, None);
        t.word("gstack", SP, Observe, None);
        // A correction still happens, and its revert still works.
        assert_eq!(t.word("bulid", SP, Silent, None), rep(5, "build ", true));
        assert_eq!(t.key(Backspace, Silent, None), rep(6, "bulid ", true));
        // A Backspace fix would be a slip.
        t.typ("bn", Silent);
        t.key(Backspace, Silent, None);
        t.typ("e", Silent);
        let s = t.store.lock().unwrap();
        assert_eq!(s.learned_word_count(), 0);
        for w in ["hello", "gstack", "build", "bulid"] {
            assert_eq!(s.personal_freq(w), 0, "{w}");
        }
        assert_eq!(s.bigram_count("hello", "gstack"), 0);
        assert_eq!(s.bigram_count("gstack", "build"), 0);
        assert!(!s.is_blacklisted("bulid", "build"));
        assert_eq!(s.slip_count(Qwerty, 'n', 'e'), 0);
        drop(s);
        assert_eq!(t.counters.corrections.load(Ordering::Relaxed), 1);
        assert_eq!(t.counters.reverts.load(Ordering::Relaxed), 1);
        // Nothing waits for a flush either.
        let dirty = t.store.lock().unwrap().take_dirty();
        assert!(
            dirty.vocab.is_empty()
                && dirty.bigrams.is_empty()
                && dirty.blacklist.is_empty()
                && dirty.slips.is_empty()
        );
    }

    #[test]
    fn counters_track_fixes_and_reverts() {
        let mut t = new();
        assert_eq!(
            t.word("bulid", SP, Silent, Some("bulid")),
            rep(5, "build ", true)
        );
        assert_eq!(
            t.key(Backspace, Silent, Some("build ")),
            rep(6, "bulid ", true)
        );
        assert_eq!(t.counters.corrections.load(Ordering::Relaxed), 1);
        assert_eq!(t.counters.reverts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn poisoned_store_does_not_panic() {
        let mut t = new();
        let s = t.store.clone();
        let _ = std::thread::spawn(move || {
            let _g = s.lock().unwrap();
            panic!("poison");
        })
        .join();
        assert!(t.store.is_poisoned());
        assert_eq!(t.word("bulid", SP, Silent, None), rep(5, "build ", true));
        assert_eq!(t.key(Backspace, Silent, None), rep(6, "bulid ", true));
        t.word("gstack", SP, Silent, None);
    }
}
