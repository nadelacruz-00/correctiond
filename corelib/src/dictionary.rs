//! Multi-language dictionary with a SymSpell delete-index (prefix length 7).

use std::collections::HashMap;

const PREFIX_LEN: usize = 7;
const MAX_DEPTH: usize = 2;
const MAX_INPUT_CHARS: usize = 24;
const HASH_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Index into the configured language list (max 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LangId(pub u8);

/// Bitmask of languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LangSet(pub u16);

impl LangSet {
    pub const EMPTY: LangSet = LangSet(0);

    /// Set containing languages `0..n` (n capped at 16).
    pub fn all(n: usize) -> LangSet {
        if n >= 16 {
            LangSet(u16::MAX)
        } else {
            LangSet(((1u32 << n) - 1) as u16)
        }
    }

    pub fn contains(self, l: LangId) -> bool {
        l.0 < 16 && self.0 & (1 << l.0) != 0
    }

    pub fn insert(&mut self, l: LangId) {
        if l.0 < 16 {
            self.0 |= 1 << l.0;
        }
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn intersects(self, other: LangSet) -> bool {
        self.0 & other.0 != 0
    }
}

/// Parse `word<TAB>count` lines. Lowercases; skips comments and malformed lines.
pub fn parse_lexicon(text: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((w, c)) = line.split_once('\t') else {
            continue;
        };
        let Ok(count) = c.trim().parse::<u64>() else {
            continue;
        };
        let w = w.trim();
        if w.is_empty() {
            continue;
        }
        // gstack-shortcut(dec-b420d3ff-d38a-4665-b51d-d7dc094a731c): ASCII-only lexicons, upgrade when per-app byte-vs-char offset detection exists
        if !w.is_ascii() {
            continue;
        }
        out.push((w.to_ascii_lowercase(), count));
    }
    out
}

struct Entry {
    word: String,
    langs: LangSet,
    /// Range into `Dictionary::freqs`.
    fstart: u32,
    flen: u8,
}

pub struct Dictionary {
    /// Sorted by word.
    entries: Vec<Entry>,
    /// Shared side table: (language, relative frequency per million).
    freqs: Vec<(LangId, f32)>,
    /// Sorted by key.
    keys: Vec<(u64, u32, u32)>,
    postings: Vec<u32>,
}

fn hash_chars(cs: &[char]) -> u64 {
    // FNV-1a with a fixed seed and a splitmix64 finalizer: deterministic.
    let mut h = HASH_SEED ^ 0xCBF2_9CE4_8422_2325;
    for &c in cs {
        h ^= c as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h ^= cs.len() as u64;
    h = (h ^ (h >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^ (h >> 31)
}

/// Hashes of the prefix and all its deletes up to `depth`, sorted and unique.
fn delete_hashes(prefix: &[char], depth: usize) -> Vec<u64> {
    let mut level: Vec<Vec<char>> = vec![prefix.to_vec()];
    let mut all: Vec<Vec<char>> = level.clone();
    for _ in 0..depth {
        let mut next: Vec<Vec<char>> = Vec::new();
        for s in &level {
            for i in 0..s.len() {
                let mut d = s.clone();
                d.remove(i);
                next.push(d);
            }
        }
        next.sort();
        next.dedup();
        all.extend(next.iter().cloned());
        level = next;
    }
    let mut hs: Vec<u64> = all.iter().map(|s| hash_chars(s)).collect();
    hs.sort_unstable();
    hs.dedup();
    hs
}

fn prefix_of(w: &str) -> Vec<char> {
    w.chars().take(PREFIX_LEN).collect()
}

impl Dictionary {
    pub fn from_lexicons(lexicons: Vec<(LangId, Vec<(String, u64)>)>) -> Dictionary {
        // Temporary: word -> per-language counts.
        let mut tmp: HashMap<String, Vec<(LangId, u64)>> = HashMap::new();
        let mut totals: HashMap<u8, u64> = HashMap::new();
        for (lang, lex) in lexicons {
            for (w, c) in lex {
                let w = w.to_lowercase();
                if w.is_empty() {
                    continue;
                }
                let t = totals.entry(lang.0).or_insert(0);
                *t = t.saturating_add(c);
                let slot = tmp.entry(w).or_default();
                match slot.iter_mut().find(|(l, _)| *l == lang) {
                    Some((_, n)) => *n = n.saturating_add(c),
                    None => slot.push((lang, c)),
                }
            }
        }
        let mut sorted: Vec<(String, Vec<(LangId, u64)>)> = tmp.into_iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));

        let mut entries: Vec<Entry> = Vec::with_capacity(sorted.len());
        let mut freqs: Vec<(LangId, f32)> = Vec::new();
        for (word, mut cs) in sorted {
            cs.sort_by_key(|(l, _)| l.0);
            let mut langs = LangSet::EMPTY;
            let fstart = freqs.len() as u32;
            for (l, n) in &cs {
                langs.insert(*l);
                let total = totals[&l.0].max(1) as f64;
                freqs.push((*l, (*n as f64 / total * 1e6) as f32));
            }
            let mut word = word;
            word.shrink_to_fit();
            entries.push(Entry {
                word,
                langs,
                fstart,
                flen: cs.len() as u8,
            });
        }
        freqs.shrink_to_fit();

        let mut pairs: Vec<(u64, u32)> = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            for h in delete_hashes(&prefix_of(&e.word), MAX_DEPTH) {
                pairs.push((h, i as u32));
            }
        }
        pairs.sort_unstable();
        pairs.dedup();
        let mut keys: Vec<(u64, u32, u32)> = Vec::new();
        let mut postings: Vec<u32> = Vec::with_capacity(pairs.len());
        for (h, i) in pairs {
            match keys.last_mut() {
                Some(k) if k.0 == h => k.2 += 1,
                _ => keys.push((h, postings.len() as u32, 1)),
            }
            postings.push(i);
        }
        keys.shrink_to_fit();
        postings.shrink_to_fit();
        Dictionary {
            entries,
            freqs,
            keys,
            postings,
        }
    }

    fn find(&self, w: &str) -> Option<&Entry> {
        let w = w.to_lowercase();
        let i = self
            .entries
            .binary_search_by(|e| e.word.as_str().cmp(w.as_str()))
            .ok()?;
        Some(&self.entries[i])
    }

    fn freqs_of(&self, e: &Entry) -> &[(LangId, f32)] {
        &self.freqs[e.fstart as usize..e.fstart as usize + e.flen as usize]
    }
    pub fn contains(&self, w: &str) -> bool {
        self.find(w).is_some()
    }

    pub fn langs_of(&self, w: &str) -> LangSet {
        self.find(w).map_or(LangSet::EMPTY, |e| e.langs)
    }

    /// Language with the highest relative frequency (ties: lowest LangId).
    pub fn primary_lang(&self, w: &str) -> Option<LangId> {
        let e = self.find(w)?;
        let mut best: Option<(LangId, f32)> = None;
        for &(l, f) in self.freqs_of(e) {
            if best.is_none_or(|(_, bf)| f > bf) {
                best = Some((l, f));
            }
        }
        best.map(|(l, _)| l)
    }

    /// Count per million words of that language's lexicon; 0.0 when absent.
    pub fn rel_freq(&self, w: &str, lang: LangId) -> f32 {
        self.find(w)
            .and_then(|e| self.freqs_of(e).iter().find(|(l, _)| *l == lang))
            .map_or(0.0, |(_, f)| *f)
    }

    /// Candidate words (superset of those within `max_edits`; caller verifies).
    pub fn candidates(&self, w: &str, max_edits: u8, allowed: LangSet) -> Vec<&str> {
        let lower = w.to_lowercase();
        if lower.chars().count() > MAX_INPUT_CHARS {
            return Vec::new();
        }
        let depth = (max_edits as usize).min(MAX_DEPTH);
        let mut ids: Vec<u32> = Vec::new();
        for h in delete_hashes(&prefix_of(&lower), depth) {
            if let Ok(pos) = self.keys.binary_search_by_key(&h, |k| k.0) {
                let (_, start, len) = self.keys[pos];
                ids.extend_from_slice(&self.postings[start as usize..(start + len) as usize]);
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
            .map(|i| &self.entries[i as usize])
            .filter(|e| e.langs.intersects(allowed))
            .map(|e| e.word.as_str())
            .collect()
    }

    /// Heap size of the key table, postings and word table (capacities).
    pub fn index_bytes(&self) -> usize {
        use std::mem::size_of;
        let words: usize = self.entries.iter().map(|e| e.word.capacity()).sum();
        self.entries.capacity() * size_of::<Entry>()
            + words
            + self.freqs.capacity() * size_of::<(LangId, f32)>()
            + self.keys.capacity() * size_of::<(u64, u32, u32)>()
            + self.postings.capacity() * size_of::<u32>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EN: LangId = LangId(0);
    const TL: LangId = LangId(1);
    const WAR: LangId = LangId(2);

    fn lex(items: &[(&str, u64)]) -> Vec<(String, u64)> {
        items.iter().map(|(w, c)| (w.to_string(), *c)).collect()
    }

    fn fixture(tl_extra: &[(&str, u64)]) -> Dictionary {
        let mut tl = vec![("ako", 500), ("kami", 40), ("may", 300)];
        tl.extend_from_slice(tl_extra);
        Dictionary::from_lexicons(vec![
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
            (TL, lex(&tl)),
            (WAR, lex(&[("maupay", 60), ("ako", 20)])),
        ])
    }

    fn set(ls: &[LangId]) -> LangSet {
        let mut s = LangSet::EMPTY;
        for l in ls {
            s.insert(*l);
        }
        s
    }

    #[test]
    fn contains_and_langs() {
        let d = fixture(&[]);
        assert!(d.contains("ako"));
        assert!(d.contains("AKO"));
        assert_eq!(d.langs_of("ako"), set(&[TL, WAR]));
        assert_eq!(d.langs_of("may"), set(&[EN, TL]));
        assert!(!d.contains("teh"));
        assert!(d.langs_of("teh").is_empty());
    }

    #[test]
    fn rel_freq_per_language() {
        let d = fixture(&[]);
        let expect = 500.0_f32 / 840.0 * 1e6;
        assert!((d.rel_freq("ako", TL) - expect).abs() < 1.0);
        assert_eq!(d.rel_freq("zzz", EN), 0.0);
        assert_eq!(d.rel_freq("ako", EN), 0.0);
    }

    #[test]
    fn primary_lang_by_relative_frequency() {
        let d = fixture(&[("the", 5)]);
        assert_eq!(d.primary_lang("the"), Some(EN));
        assert_eq!(d.primary_lang("ako"), Some(TL));
        assert_eq!(d.primary_lang("zzz"), None);
    }

    #[test]
    fn candidates_within_two_edits() {
        let d = fixture(&[]);
        let all = LangSet::all(3);
        assert!(d.candidates("bulid", 2, all).contains(&"build"));
        assert!(d.candidates("teh", 2, all).contains(&"the"));
    }

    #[test]
    fn candidates_respect_language() {
        let d = fixture(&[]);
        assert!(d.candidates("maupy", 2, set(&[WAR])).contains(&"maupay"));
        assert!(!d.candidates("maupy", 2, set(&[EN])).contains(&"maupay"));
    }

    #[test]
    fn long_input_rejected() {
        let d = Dictionary::from_lexicons(vec![(EN, lex(&[("understanding", 10)]))]);
        let all = LangSet::all(1);
        assert!(d.candidates(&"a".repeat(500), 2, all).is_empty());
        let long = format!("understanding{}", "x".repeat(20));
        assert_eq!(long.chars().count(), 33);
        assert!(d.candidates(&long, 2, all).is_empty());
        let ok = format!("understanding{}", "x".repeat(11));
        assert_eq!(ok.chars().count(), 24);
        assert!(d.candidates(&ok, 2, all).contains(&"understanding"));
    }

    #[test]
    fn kelvin_sign_dropped() {
        assert!(parse_lexicon("\u{212A}ami\t5\n").is_empty());
    }

    #[test]
    fn huge_counts_do_not_panic() {
        let d = Dictionary::from_lexicons(vec![(
            EN,
            lex(&[("the", u64::MAX), ("the", u64::MAX), ("a", u64::MAX)]),
        )]);
        assert!(d.contains("the"));
        assert!(d.rel_freq("the", EN) > 0.0);
    }

    #[test]
    fn nonascii_entries_filtered() {
        assert!(parse_lexicon("café\t10\nniño\t5\n").is_empty());
        assert_eq!(
            parse_lexicon("# c\nAko\t5\nbad\nx\tq\n"),
            lex(&[("ako", 5)])
        );
    }

    #[test]
    fn long_word_edits_found() {
        let d = Dictionary::from_lexicons(vec![(EN, lex(&[("understanding", 10)]))]);
        let all = LangSet::all(1);
        assert!(d
            .candidates("understnading", 2, all)
            .contains(&"understanding"));
        assert!(d
            .candidates("undrestanding", 2, all)
            .contains(&"understanding"));
    }

    #[test]
    fn index_size_budget() {
        let text = include_str!("../../data/en_words.txt");
        let d = Dictionary::from_lexicons(vec![(EN, parse_lexicon(text))]);
        eprintln!("index_bytes = {}", d.index_bytes());
        assert!(d.index_bytes() <= 32 * 1024 * 1024);
    }

    #[test]
    fn primary_lang_shared_vectors() {
        // The same file is shared with the importers in https://github.com/nadelacruz-00/correctiond-import-cookbook.
        let text = include_str!("../../data/primary-lang-vectors.tsv");
        let names = ["en", "tl", "war"];
        let mut sections: Vec<String> = vec![String::new(); names.len()];
        let mut expect: Vec<(String, String)> = Vec::new();
        let mut cur: Option<&str> = None;
        for line in text.lines() {
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                cur = Some(name);
                continue;
            }
            match cur {
                Some("expect") => {
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    let (w, l) = line.split_once('\t').expect("word<TAB>lang");
                    expect.push((w.to_string(), l.to_string()));
                }
                Some(name) => {
                    let i = names
                        .iter()
                        .position(|n| *n == name)
                        .expect("known section");
                    sections[i].push_str(line);
                    sections[i].push('\n');
                }
                None => {}
            }
        }
        let d = Dictionary::from_lexicons(
            sections
                .iter()
                .enumerate()
                .map(|(i, s)| (LangId(i as u8), parse_lexicon(s)))
                .collect(),
        );
        assert!(expect.len() >= 8);
        for (w, l) in &expect {
            let want = names.iter().position(|n| n == l).map(|i| LangId(i as u8));
            assert_eq!(d.primary_lang(w), want, "{w}");
        }
    }
}
