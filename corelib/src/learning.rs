//! In-memory learning cache. Never touches disk: persistence is `db.rs`'s job,
//! driven off the typing path through `take_dirty` / `merge_snapshot` /
//! `restore_dirty`.

use crate::db::{BigramRow, BlacklistRow, DirtyBatch, SlipRow, Snapshot, VocabRow};
use crate::layout::LayoutId;
use crate::scoring::PersonalLookup;
use crate::vocab::VocabBatch;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

/// One store per engine process. Lock with
/// `lock().unwrap_or_else(|e| e.into_inner())`, never `unwrap()`.
pub type SharedStore = Arc<Mutex<LearnStore>>;

/// Second-sighting rule: counts below this are not persisted.
const PERSIST_MIN: u64 = 2;

/// Recent-list capacity.
const RECENT_CAP: usize = 5;

#[derive(Default)]
pub struct LearnStore {
    persistent: bool,
    vocab: HashMap<String, (u64, i64)>,
    blacklist: HashSet<(String, String)>,
    bigrams: HashMap<(String, String), u64>,
    slips: HashMap<(LayoutId, char, char), u64>,
    dirty_vocab: HashSet<String>,
    dirty_blacklist: HashSet<(String, String)>,
    dirty_bigrams: HashSet<(String, String)>,
    dirty_slips: HashSet<(LayoutId, char, char)>,
    recent: VecDeque<String>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn lc(s: &str) -> String {
    s.to_lowercase()
}

fn lc_char(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

impl LearnStore {
    pub fn memory() -> LearnStore {
        LearnStore::default()
    }

    pub fn set_persistent(&mut self, on: bool) {
        self.persistent = on;
    }

    pub fn is_persistent(&self) -> bool {
        self.persistent
    }

    /// Count one sighting of `w`; returns the new count.
    pub fn record_word(&mut self, w: &str) -> u64 {
        let w = lc(w);
        let e = self.vocab.entry(w.clone()).or_insert((0, 0));
        e.0 = e.0.saturating_add(1);
        e.1 = now();
        let n = e.0;
        self.dirty_vocab.insert(w);
        n
    }

    /// Put `w` at the front of the recent list (memory only, capacity 5).
    pub fn note_learned(&mut self, w: &str) {
        let w = lc(w);
        self.recent.retain(|x| *x != w);
        self.recent.push_front(w);
        self.recent.truncate(RECENT_CAP);
    }

    /// Recently learned words, newest first.
    pub fn recent_words(&self) -> Vec<String> {
        self.recent.iter().cloned().collect()
    }

    /// Remove `w` entirely (vocab incl. imported counts, bigrams and
    /// blacklist pairs on either side, dirty flags, recent entry). True if
    /// the vocab entry existed.
    pub fn forget_word(&mut self, w: &str) -> bool {
        self.forget_words(&[w.to_owned()]) == 1
    }

    /// `forget_word` for many words in one pass over each collection.
    /// Returns how many vocab entries existed.
    pub fn forget_words(&mut self, ws: &[String]) -> u32 {
        let set: HashSet<String> = ws.iter().map(|w| lc(w)).collect();
        let mut had = 0;
        for w in &set {
            if self.vocab.remove(w).is_some() {
                had += 1;
            }
            self.dirty_vocab.remove(w);
        }
        self.bigrams
            .retain(|k, _| !set.contains(&k.0) && !set.contains(&k.1));
        self.blacklist
            .retain(|k| !set.contains(&k.0) && !set.contains(&k.1));
        self.dirty_bigrams
            .retain(|k| !set.contains(&k.0) && !set.contains(&k.1));
        self.dirty_blacklist
            .retain(|k| !set.contains(&k.0) && !set.contains(&k.1));
        self.recent.retain(|x| !set.contains(x));
        had
    }

    /// (word, count, last_seen, personal) for every vocab entry with count
    /// at least `PERSIST_MIN`; `personal` means `is_known` rejects it.
    /// Unordered.
    pub fn list_words(&self, is_known: &dyn Fn(&str) -> bool) -> Vec<(String, u64, i64, bool)> {
        self.vocab
            .iter()
            .filter(|(_, v)| v.0 >= PERSIST_MIN)
            .map(|(w, v)| (w.clone(), v.0, v.1, !is_known(w)))
            .collect()
    }

    pub fn record_blacklist(&mut self, orig: &str, cand: &str) {
        let k = (lc(orig), lc(cand));
        self.blacklist.insert(k.clone());
        self.dirty_blacklist.insert(k);
    }

    pub fn bump_bigram(&mut self, w1: &str, w2: &str) {
        let k = (lc(w1), lc(w2));
        let c = self.bigrams.entry(k.clone()).or_insert(0);
        *c = c.saturating_add(1);
        self.dirty_bigrams.insert(k);
    }

    pub fn record_slip(&mut self, layout: LayoutId, typed: char, intended: char) {
        let k = (layout, lc_char(typed), lc_char(intended));
        let c = self.slips.entry(k).or_insert(0);
        *c = c.saturating_add(1);
        self.dirty_slips.insert(k);
    }

    /// Add imported counts onto vocab and bigrams (keys lowercased) and mark
    /// those rows dirty. The second-sighting rule is unchanged: a total of
    /// at least 2 reaches the next flush.
    pub fn add_imported(&mut self, batch: &VocabBatch) {
        let t = now();
        for (w, c) in &batch.words {
            let w = lc(w);
            let e = self.vocab.entry(w.clone()).or_insert((0, 0));
            e.0 = e.0.saturating_add(*c);
            e.1 = e.1.max(t);
            self.dirty_vocab.insert(w);
        }
        for (w1, w2, c) in &batch.bigrams {
            let k = (lc(w1), lc(w2));
            let e = self.bigrams.entry(k.clone()).or_insert(0);
            *e = e.saturating_add(*c);
            self.dirty_bigrams.insert(k);
        }
    }

    /// The rows an import would leave at or above the second-sighting
    /// threshold, with totals = memory + imported (keys lowercased). Read
    /// only: memory and dirty flags are untouched.
    pub fn import_totals(
        &self,
        words: &[(String, u64)],
        bigrams: &[(String, String, u64)],
    ) -> DirtyBatch {
        let t = now();
        let mut b = DirtyBatch::default();
        for (w, c) in words {
            let w = lc(w);
            let (freq, seen) = self.vocab.get(&w).copied().unwrap_or((0, 0));
            let freq = freq.saturating_add(*c);
            if freq >= PERSIST_MIN {
                b.vocab.push(VocabRow {
                    word: w,
                    freq,
                    last_seen: seen.max(t),
                });
            }
        }
        for (w1, w2, c) in bigrams {
            let k = (lc(w1), lc(w2));
            let count = self
                .bigrams
                .get(&k)
                .copied()
                .unwrap_or(0)
                .saturating_add(*c);
            if count >= PERSIST_MIN {
                b.bigrams.push(BigramRow {
                    w1: k.0,
                    w2: k.1,
                    count,
                });
            }
        }
        b
    }

    /// Clone the dirty rows that pass the second-sighting rule and clear their
    /// flags. Rows below the threshold stay dirty.
    pub fn take_dirty(&mut self) -> DirtyBatch {
        let mut b = DirtyBatch::default();
        let vocab = &self.vocab;
        self.dirty_vocab.retain(|w| match vocab.get(w) {
            Some(&(freq, last_seen)) if freq >= PERSIST_MIN => {
                b.vocab.push(VocabRow {
                    word: w.clone(),
                    freq,
                    last_seen,
                });
                false
            }
            Some(_) => true,
            None => false,
        });
        for k in self.dirty_blacklist.drain() {
            b.blacklist.push(BlacklistRow {
                orig: k.0,
                cand: k.1,
            });
        }
        let bigrams = &self.bigrams;
        self.dirty_bigrams.retain(|k| match bigrams.get(k) {
            Some(&count) if count >= PERSIST_MIN => {
                b.bigrams.push(BigramRow {
                    w1: k.0.clone(),
                    w2: k.1.clone(),
                    count,
                });
                false
            }
            Some(_) => true,
            None => false,
        });
        let slips = &self.slips;
        self.dirty_slips.retain(|k| match slips.get(k) {
            Some(&count) if count >= PERSIST_MIN => {
                b.slips.push(SlipRow {
                    layout: k.0,
                    typed: k.1,
                    intended: k.2,
                    count,
                });
                false
            }
            Some(_) => true,
            None => false,
        });
        b
    }

    /// Re-mark a failed batch's rows dirty. The in-memory totals stay
    /// authoritative; only the flags are restored.
    pub fn restore_dirty(&mut self, batch: DirtyBatch) {
        for r in batch.vocab {
            if self.vocab.contains_key(&r.word) {
                self.dirty_vocab.insert(r.word);
            }
        }
        for r in batch.blacklist {
            let k = (r.orig, r.cand);
            if self.blacklist.contains(&k) {
                self.dirty_blacklist.insert(k);
            }
        }
        for r in batch.bigrams {
            let k = (r.w1, r.w2);
            if self.bigrams.contains_key(&k) {
                self.dirty_bigrams.insert(k);
            }
        }
        for r in batch.slips {
            let k = (r.layout, r.typed, r.intended);
            if self.slips.contains_key(&k) {
                self.dirty_slips.insert(k);
            }
        }
    }

    /// Sum a database snapshot into memory. Only rows that already existed in
    /// memory become dirty (their totals changed); snapshot-only rows are clean.
    pub fn merge_snapshot(&mut self, snap: Snapshot) {
        for r in snap.vocab {
            match self.vocab.get_mut(&r.word) {
                Some(e) => {
                    e.0 = e.0.saturating_add(r.freq);
                    e.1 = e.1.max(r.last_seen);
                    self.dirty_vocab.insert(r.word);
                }
                None => {
                    self.vocab.insert(r.word, (r.freq, r.last_seen));
                }
            }
        }
        for r in snap.blacklist {
            self.blacklist.insert((r.orig, r.cand));
        }
        for r in snap.bigrams {
            let k = (r.w1, r.w2);
            match self.bigrams.get_mut(&k) {
                Some(c) => {
                    *c = c.saturating_add(r.count);
                    self.dirty_bigrams.insert(k);
                }
                None => {
                    self.bigrams.insert(k, r.count);
                }
            }
        }
        for r in snap.slips {
            let k = (r.layout, r.typed, r.intended);
            match self.slips.get_mut(&k) {
                Some(c) => {
                    *c = c.saturating_add(r.count);
                    self.dirty_slips.insert(k);
                }
                None => {
                    self.slips.insert(k, r.count);
                }
            }
        }
    }

    /// Drop everything (Purge).
    pub fn clear(&mut self) {
        self.vocab.clear();
        self.blacklist.clear();
        self.bigrams.clear();
        self.slips.clear();
        self.dirty_vocab.clear();
        self.dirty_blacklist.clear();
        self.dirty_bigrams.clear();
        self.dirty_slips.clear();
        self.recent.clear();
    }

    /// Distinct words seen at least twice.
    pub fn learned_word_count(&self) -> usize {
        self.vocab.values().filter(|v| v.0 >= PERSIST_MIN).count()
    }
}

impl PersonalLookup for LearnStore {
    fn personal_freq(&self, w: &str) -> u64 {
        self.vocab.get(&lc(w)).map_or(0, |v| v.0)
    }
    fn is_blacklisted(&self, orig: &str, cand: &str) -> bool {
        self.blacklist.contains(&(lc(orig), lc(cand)))
    }
    fn bigram_count(&self, w1: &str, w2: &str) -> u64 {
        self.bigrams.get(&(lc(w1), lc(w2))).copied().unwrap_or(0)
    }
    fn slip_count(&self, layout: LayoutId, typed: char, intended: char) -> u64 {
        self.slips
            .get(&(layout, lc_char(typed), lc_char(intended)))
            .copied()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_word_returns_count() {
        let mut s = LearnStore::memory();
        assert_eq!(s.record_word("kadto"), 1);
        assert_eq!(s.record_word("kadto"), 2);
    }

    #[test]
    fn recent_list_cap_order_repeat() {
        let mut s = LearnStore::memory();
        for w in ["a", "b", "c", "d", "e", "f"] {
            s.note_learned(w);
        }
        assert_eq!(s.recent_words(), ["f", "e", "d", "c", "b"]);
        s.note_learned("c");
        assert_eq!(s.recent_words(), ["c", "f", "e", "d", "b"]);
        s.note_learned("Kadto");
        assert_eq!(s.recent_words()[0], "kadto");
    }

    #[test]
    fn forget_word_removes_word_pairs_dirty_recent() {
        let mut s = LearnStore::memory();
        s.record_word("ako");
        s.record_word("kadto");
        s.record_word("kadto");
        s.bump_bigram("ako", "kadto");
        s.bump_bigram("kadto", "na");
        s.note_learned("kadto");
        assert!(s.forget_word("KADTO"));
        assert_eq!(s.personal_freq("kadto"), 0);
        assert_eq!(s.bigram_count("ako", "kadto"), 0);
        assert_eq!(s.bigram_count("kadto", "na"), 0);
        assert!(s.recent_words().is_empty());
        let d = s.take_dirty();
        assert!(d.vocab.iter().all(|r| r.word != "kadto"));
        assert!(d.bigrams.iter().all(|r| r.w1 != "kadto" && r.w2 != "kadto"));
        assert_eq!(s.personal_freq("ako"), 1);
        assert!(!s.forget_word("kadto"));
    }

    #[test]
    fn forget_word_clears_blacklist_rows_on_either_side() {
        let mut s = LearnStore::memory();
        s.record_blacklist("teh", "the");
        s.record_blacklist("adn", "and");
        s.record_word("teh");
        s.record_word("teh");
        assert!(s.forget_word("teh"));
        assert!(!s.is_blacklisted("teh", "the"));
        assert!(s.is_blacklisted("adn", "and"));
        let d = s.take_dirty();
        assert!(d
            .blacklist
            .iter()
            .all(|r| r.orig != "teh" && r.cand != "teh"));
        assert!(d.blacklist.iter().any(|r| r.orig == "adn"));

        // Forgetting the candidate side clears the row too.
        let mut s = LearnStore::memory();
        s.record_blacklist("teh", "the");
        s.forget_word("THE");
        assert!(!s.is_blacklisted("teh", "the"));
        assert!(s.take_dirty().blacklist.is_empty());
    }

    #[test]
    fn forget_word_removes_imported_counts() {
        let mut s = LearnStore::memory();
        let mut b = VocabBatch::default();
        b.words.push(("kadto".into(), 12));
        s.add_imported(&b);
        assert_eq!(s.personal_freq("kadto"), 12);
        assert!(s.forget_word("kadto"));
        assert_eq!(s.personal_freq("kadto"), 0);
    }

    #[test]
    fn forget_words_batch_removes_all_sides() {
        let mut s = LearnStore::memory();
        for w in ["kadto", "sige", "ako"] {
            s.record_word(w);
            s.record_word(w);
        }
        s.bump_bigram("ako", "kadto");
        s.bump_bigram("sige", "na");
        s.bump_bigram("na", "ako");
        s.record_blacklist("kadto", "kadto2");
        s.record_blacklist("teh", "sige");
        s.note_learned("kadto");
        let n = s.forget_words(&["KADTO".into(), "sige".into(), "missing".into()]);
        assert_eq!(n, 2);
        assert_eq!(s.personal_freq("kadto"), 0);
        assert_eq!(s.personal_freq("sige"), 0);
        assert_eq!(s.personal_freq("ako"), 2);
        assert_eq!(s.bigram_count("na", "ako"), 1);
        assert_eq!(s.bigram_count("ako", "kadto"), 0);
        assert_eq!(s.bigram_count("sige", "na"), 0);
        assert!(!s.is_blacklisted("kadto", "kadto2"));
        assert!(!s.is_blacklisted("teh", "sige"));
        assert!(s.recent_words().is_empty());
        let d = s.take_dirty();
        let bad = |w: &str| w == "kadto" || w == "sige";
        assert!(d.vocab.iter().all(|r| !bad(&r.word)));
        assert!(d.bigrams.iter().all(|r| !bad(&r.w1) && !bad(&r.w2)));
        assert!(d.blacklist.iter().all(|r| !bad(&r.orig) && !bad(&r.cand)));
    }

    #[test]
    fn forget_words_1000() {
        let mut s = LearnStore::memory();
        let many: Vec<String> = (0..1000).map(|i| format!("w{i}")).collect();
        for w in many.iter().chain(
            ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]
                .map(String::from)
                .iter(),
        ) {
            s.record_word(w);
            s.record_word(w);
        }
        assert_eq!(s.forget_words(&many), 1000);
        let mut left: Vec<String> = s.list_words(&|_| false).into_iter().map(|r| r.0).collect();
        left.sort();
        assert_eq!(left, ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
    }

    #[test]
    fn list_words_count_and_personal() {
        let mut s = LearnStore::memory();
        s.record_word("kadto");
        s.record_word("kadto");
        for _ in 0..5 {
            s.record_word("the");
        }
        s.record_word("x1");
        let mut b = VocabBatch::default();
        b.words.push(("imported".into(), 4));
        s.add_imported(&b);
        let mut got = s.list_words(&|w| w == "the");
        got.sort();
        assert_eq!(got.len(), 3);
        assert_eq!(
            (got[0].0.as_str(), got[0].1, got[0].3),
            ("imported", 4, true)
        );
        assert_eq!((got[1].0.as_str(), got[1].1, got[1].3), ("kadto", 2, true));
        assert_eq!((got[2].0.as_str(), got[2].1, got[2].3), ("the", 5, false));
    }

    #[test]
    fn clear_empties_recent() {
        let mut s = LearnStore::memory();
        s.note_learned("kadto");
        s.clear();
        assert!(s.recent_words().is_empty());
    }

    #[test]
    fn first_sighting_not_taken() {
        let mut s = LearnStore::memory();
        s.record_word("gstack");
        assert!(s.take_dirty().vocab.is_empty());
        s.record_word("gstack");
        let b = s.take_dirty();
        assert_eq!(b.vocab.len(), 1);
        assert_eq!(b.vocab[0].freq, 2);
    }

    #[test]
    fn slips_threshold_and_layout() {
        let mut s = LearnStore::memory();
        s.record_slip(LayoutId::Colemak, 'N', 'e');
        assert!(s.take_dirty().slips.is_empty());
        s.record_slip(LayoutId::Colemak, 'n', 'e');
        assert_eq!(s.take_dirty().slips.len(), 1);
        assert_eq!(s.slip_count(LayoutId::Colemak, 'n', 'e'), 2);
        assert_eq!(s.slip_count(LayoutId::Qwerty, 'n', 'e'), 0);
    }

    #[test]
    fn blacklist_and_bigram_thresholds() {
        let mut s = LearnStore::memory();
        s.record_blacklist("Teh", "The");
        s.bump_bigram("a", "b");
        let b = s.take_dirty();
        assert_eq!(b.blacklist.len(), 1);
        assert!(b.bigrams.is_empty());
        assert!(s.is_blacklisted("teh", "the"));
        s.bump_bigram("A", "b");
        assert_eq!(s.take_dirty().bigrams.len(), 1);
        assert_eq!(s.bigram_count("a", "b"), 2);
    }

    #[test]
    fn failed_write_restores_dirty() {
        let mut s = LearnStore::memory();
        s.record_word("x");
        s.record_word("x");
        s.record_blacklist("a", "b");
        let b = s.take_dirty();
        assert!(s.take_dirty().vocab.is_empty());
        s.restore_dirty(b);
        let again = s.take_dirty();
        assert_eq!(again.vocab.len(), 1);
        assert_eq!(again.blacklist.len(), 1);
    }

    #[test]
    fn merge_snapshot_sums() {
        let mut s = LearnStore::memory();
        s.record_word("x");
        s.record_word("x");
        s.take_dirty();
        let snap = Snapshot {
            vocab: vec![
                VocabRow {
                    word: "x".into(),
                    freq: 3,
                    last_seen: 1,
                },
                VocabRow {
                    word: "only".into(),
                    freq: 9,
                    last_seen: 1,
                },
            ],
            ..Snapshot::default()
        };
        s.merge_snapshot(snap);
        assert_eq!(s.personal_freq("x"), 5);
        assert_eq!(s.personal_freq("only"), 9);
        let b = s.take_dirty();
        assert_eq!(b.vocab.len(), 1);
        assert_eq!(b.vocab[0].word, "x");
        assert_eq!(s.learned_word_count(), 2);
        s.clear();
        assert_eq!(s.learned_word_count(), 0);
    }

    #[test]
    fn add_imported_lowercases_sums_and_marks_dirty() {
        use crate::vocab::VocabBatch;
        let mut s = LearnStore::memory();
        s.record_word("ako");
        s.add_imported(&VocabBatch {
            words: vec![("Ako".into(), 4), ("once".into(), 1)],
            bigrams: vec![("Ako".into(), "Naman".into(), 3)],
        });
        assert_eq!(s.personal_freq("ako"), 5);
        assert_eq!(s.bigram_count("ako", "naman"), 3);
        let b = s.take_dirty();
        // Count >= 2 reaches the next flush; a count of 1 waits for a
        // second sighting, as typed words do.
        let words: Vec<_> = b.vocab.iter().map(|r| (r.word.as_str(), r.freq)).collect();
        assert_eq!(words, [("ako", 5)]);
        assert!(b.vocab[0].last_seen > 0);
        assert_eq!(b.bigrams.len(), 1);
        assert_eq!(
            (b.bigrams[0].w1.as_str(), b.bigrams[0].w2.as_str()),
            ("ako", "naman")
        );
        s.record_word("once");
        assert_eq!(s.take_dirty().vocab.len(), 1);
    }

    #[test]
    fn import_totals_apply_second_sighting_without_changing_memory() {
        let mut s = LearnStore::memory();
        s.record_word("ako");
        s.bump_bigram("ako", "naman");
        s.take_dirty();
        let words = vec![("Ako".into(), 1), ("once".into(), 1), ("mabuti".into(), 3)];
        let bigrams = vec![
            ("ako".into(), "naman".into(), 1),
            ("ako".into(), "mabuti".into(), 1),
        ];
        let b = s.import_totals(&words, &bigrams);
        let w: Vec<_> = b.vocab.iter().map(|r| (r.word.as_str(), r.freq)).collect();
        // memory + imported; "once" (total 1) stays out.
        assert_eq!(w, [("ako", 2), ("mabuti", 3)]);
        assert!(b.vocab.iter().all(|r| r.last_seen > 0));
        let g: Vec<_> = b
            .bigrams
            .iter()
            .map(|r| (r.w1.as_str(), r.w2.as_str(), r.count))
            .collect();
        assert_eq!(g, [("ako", "naman", 2)]);
        // Read only: nothing added, nothing marked dirty.
        assert_eq!(s.personal_freq("ako"), 1);
        assert_eq!(s.personal_freq("mabuti"), 0);
        let d = s.take_dirty();
        assert!(d.vocab.is_empty() && d.bigrams.is_empty());
    }

    #[test]
    fn persistent_flag() {
        let mut s = LearnStore::memory();
        assert!(!s.is_persistent());
        s.set_persistent(true);
        assert!(s.is_persistent());
    }
}
