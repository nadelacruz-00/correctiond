//! Weighted edit distance and the correction decision rule.

use crate::dictionary::{Dictionary, LangId, LangSet};
use crate::layout::{sub_cost, LayoutId};
use crate::policy::case_shape_ok;

const INDEL_COST: f32 = 1.0;
const TRANSPOSE_COST: f32 = 0.7;
const MIN_SUB_COST: f32 = 0.1;
const SLIP_DISCOUNT: f32 = 0.2;
const SLIP_SATURATION: f32 = 5.0;
const MAX_ACCEPT_DL: f32 = 1.5;
const BIGRAM_BONUS: f32 = 0.3;
const MIN_LEN: usize = 3;
const MAX_LEN: usize = 24;

/// Learned user data consulted while scoring (implemented by the store).
pub trait PersonalLookup {
    fn personal_freq(&self, w: &str) -> u64;
    fn is_blacklisted(&self, orig: &str, cand: &str) -> bool;
    fn bigram_count(&self, w1: &str, w2: &str) -> u64;
    fn slip_count(&self, layout: LayoutId, typed: char, intended: char) -> u64;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub replacement: String,
}

#[derive(Debug, Clone, Copy)]
pub struct LangCtx {
    pub allowed: LangSet,
    pub margin: f32,
}

fn substitution(layout: LayoutId, x: char, y: char, personal: &dyn PersonalLookup) -> f32 {
    if x == y {
        return 0.0;
    }
    let slips = personal.slip_count(layout, x, y) as f32;
    let discount = SLIP_DISCOUNT * (slips / SLIP_SATURATION).min(1.0);
    (sub_cost(layout, x, y) - discount).max(MIN_SUB_COST)
}

/// Damerau-Levenshtein (optimal string alignment) from typed `a` to candidate `b`.
pub fn weighted_dl(a: &str, b: &str, layout: LayoutId, personal: &dyn PersonalLookup) -> f32 {
    let a: Vec<char> = a.to_lowercase().chars().collect();
    let b: Vec<char> = b.to_lowercase().chars().collect();
    let (n, m) = (a.len(), b.len());
    let w = m + 1;
    let mut d = vec![0.0f32; (n + 1) * w];
    for i in 1..=n {
        d[i * w] = i as f32 * INDEL_COST;
    }
    for (j, cell) in d.iter_mut().enumerate().take(m + 1).skip(1) {
        *cell = j as f32 * INDEL_COST;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub = d[(i - 1) * w + j - 1] + substitution(layout, a[i - 1], b[j - 1], personal);
            let del = d[(i - 1) * w + j] + INDEL_COST;
            let ins = d[i * w + j - 1] + INDEL_COST;
            let mut best = sub.min(del).min(ins);
            if i > 1
                && j > 1
                && a[i - 1] == b[j - 2]
                && a[i - 2] == b[j - 1]
                && a[i - 1] != a[i - 2]
            {
                best = best.min(d[(i - 2) * w + j - 2] + TRANSPOSE_COST);
            }
            d[i * w + j] = best;
        }
    }
    d[n * w + m]
}

/// Lowercase or Capitalized ASCII words only: ALL-CAPS and mixed case
/// (acronyms, identifiers) are left alone, as the learn rule treats them.
fn eligible(word: &str) -> bool {
    let len = word.chars().count();
    (MIN_LEN..=MAX_LEN).contains(&len)
        && word.chars().all(|c| c.is_ascii_alphabetic() || c == '\'')
        && case_shape_ok(word)
}

fn best_rel_freq(dict: &Dictionary, cand: &str, allowed: LangSet) -> f32 {
    let langs = dict.langs_of(cand);
    (0..16u8)
        .map(LangId)
        .filter(|&l| langs.contains(l) && allowed.contains(l))
        .map(|l| dict.rel_freq(cand, l))
        .fold(0.0, f32::max)
}

fn match_case(word: &str, replacement: &str) -> String {
    if !word.chars().next().is_some_and(|c| c.is_uppercase()) {
        return replacement.to_string();
    }
    let mut cs = replacement.chars();
    match cs.next() {
        Some(f) => f.to_uppercase().chain(cs).collect(),
        None => String::new(),
    }
}

/// Decide whether `word` should be replaced; never panics.
pub fn decide(
    word: &str,
    prev_word: Option<&str>,
    layout: LayoutId,
    lang: &LangCtx,
    dict: &Dictionary,
    personal: &dyn PersonalLookup,
) -> Option<Verdict> {
    if !eligible(word) || dict.contains(word) || personal.personal_freq(&word.to_lowercase()) >= 2 {
        return None;
    }
    let lower = word.to_lowercase();
    let prev_lower = prev_word.map(str::to_lowercase);
    // (score, dl, candidate) for the best and the runner-up.
    let mut best: Option<(f32, f32, &str)> = None;
    let mut runner_up = f32::INFINITY;
    for cand in dict.candidates(word, 2, lang.allowed) {
        if cand == lower {
            continue;
        }
        let dl = weighted_dl(word, cand, layout, personal);
        if !dl.is_finite() {
            continue;
        }
        let freq =
            best_rel_freq(dict, cand, lang.allowed) + 10.0 * personal.personal_freq(cand) as f32;
        let mut score = dl - 0.2 * freq.max(1.0).log10();
        if prev_lower
            .as_deref()
            .is_some_and(|p| personal.bigram_count(p, cand) > 0)
        {
            score -= BIGRAM_BONUS;
        }
        match best {
            Some((bs, _, _)) if score >= bs => runner_up = runner_up.min(score),
            _ => {
                if let Some((bs, _, _)) = best {
                    runner_up = runner_up.min(bs);
                }
                best = Some((score, dl, cand));
            }
        }
    }
    let (score, dl, cand) = best?;
    if dl > MAX_ACCEPT_DL
        || !cand.is_ascii()
        || runner_up - score < lang.margin
        || personal.is_blacklisted(&lower, cand)
    {
        return None;
    }
    Some(Verdict {
        replacement: match_case(word, cand),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary::{Dictionary, LangId, LangSet};
    use crate::layout::LayoutId::{Colemak, Qwerty};

    const EN: LangId = LangId(0);
    const TL: LangId = LangId(1);
    const WAR: LangId = LangId(2);

    struct Stub {
        personal: Vec<(&'static str, u64)>,
        blacklist: Vec<(&'static str, &'static str)>,
        slips: u64,
        bigrams: Vec<(&'static str, &'static str)>,
    }
    impl Stub {
        fn none() -> Stub {
            Stub {
                personal: vec![],
                blacklist: vec![],
                slips: 0,
                bigrams: vec![],
            }
        }
    }
    impl PersonalLookup for Stub {
        fn personal_freq(&self, w: &str) -> u64 {
            self.personal
                .iter()
                .find(|(p, _)| *p == w)
                .map_or(0, |(_, c)| *c)
        }
        fn is_blacklisted(&self, o: &str, c: &str) -> bool {
            self.blacklist.iter().any(|(a, b)| *a == o && *b == c)
        }
        fn bigram_count(&self, a: &str, b: &str) -> u64 {
            u64::from(self.bigrams.iter().any(|(x, y)| *x == a && *y == b))
        }
        fn slip_count(&self, layout: LayoutId, t: char, i: char) -> u64 {
            if layout == Colemak && t == 'n' && i == 'e' {
                self.slips
            } else {
                0
            }
        }
    }

    fn lex(items: &[(&str, u64)]) -> Vec<(String, u64)> {
        items.iter().map(|(w, c)| (w.to_string(), *c)).collect()
    }
    fn dict_with(en_extra: &[(&str, u64)]) -> Dictionary {
        let mut en = vec![("the", 1000), ("build", 50), ("hello", 80), ("may", 40)];
        en.extend_from_slice(en_extra);
        Dictionary::from_lexicons(vec![
            (EN, lex(&en)),
            (TL, lex(&[("ako", 500), ("kami", 40), ("may", 300)])),
            (WAR, lex(&[("maupay", 60), ("ako", 20)])),
        ])
    }
    fn all() -> LangCtx {
        LangCtx {
            allowed: LangSet::all(3),
            margin: 0.3,
        }
    }
    fn only(l: LangId) -> LangCtx {
        let mut s = LangSet::EMPTY;
        s.insert(l);
        LangCtx {
            allowed: s,
            margin: 0.3,
        }
    }
    fn run(w: &str, ctx: &LangCtx, d: &Dictionary, p: &Stub) -> Option<String> {
        decide(w, None, Qwerty, ctx, d, p).map(|v| v.replacement)
    }

    #[test]
    fn fixes_transposition() {
        assert_eq!(
            run("bulid", &all(), &dict_with(&[]), &Stub::none()).as_deref(),
            Some("build")
        );
    }
    #[test]
    fn known_word_untouched() {
        assert_eq!(run("build", &all(), &dict_with(&[]), &Stub::none()), None);
    }
    #[test]
    fn filipino_word_never_corrected() {
        let d = dict_with(&[("ago", 1000)]);
        let ctx = only(EN);
        assert_eq!(run("ako", &ctx, &d, &Stub::none()), None);
        assert_eq!(run("kami", &ctx, &d, &Stub::none()), None);
    }
    #[test]
    fn waray_typo_fixed_toward_waray() {
        assert_eq!(
            run("maupy", &only(WAR), &dict_with(&[]), &Stub::none()).as_deref(),
            Some("maupay")
        );
    }
    #[test]
    fn english_blocked_in_filipino_context() {
        assert_eq!(
            run("bulid", &only(TL), &dict_with(&[]), &Stub::none()),
            None
        );
    }
    #[test]
    fn personal_word_untouched() {
        let mut p = Stub::none();
        p.personal.push(("gstack", 5));
        assert_eq!(
            run("gstack", &all(), &dict_with(&[("stack", 50)]), &p),
            None
        );
    }
    #[test]
    fn blacklisted_pair_skipped() {
        let mut p = Stub::none();
        p.blacklist.push(("bulid", "build"));
        assert_eq!(run("bulid", &all(), &dict_with(&[]), &p), None);
    }
    #[test]
    fn ambiguous_margin_rejected() {
        let p = Stub::none();
        // "cit" -> "cut"/"cot": both one adjacent-key substitution.
        let tie = dict_with(&[("cut", 100), ("cot", 100)]);
        assert_eq!(run("cit", &all(), &tie, &p), None);
        // 100x frequency ratio => 0.4 score gap.
        let gap = dict_with(&[("cut", 1000), ("cot", 10)]);
        let wide = LangCtx {
            margin: 0.6,
            ..all()
        };
        assert_eq!(run("cit", &wide, &gap, &p), None);
        assert_eq!(run("cit", &all(), &gap, &p).as_deref(), Some("cut"));
    }
    #[test]
    fn personal_slip_lowers_cost() {
        let none = Stub::none();
        let mut slip = Stub::none();
        slip.slips = 5;
        let base = weighted_dl("bnd", "bed", Colemak, &none);
        let low = weighted_dl("bnd", "bed", Colemak, &slip);
        assert!((base - low - 0.2).abs() < 1e-5, "{base} {low}");
        slip.slips = 1000;
        let capped = weighted_dl("bnd", "bed", Colemak, &slip);
        assert!((capped - low).abs() < 1e-5);
        assert!(capped >= 0.1);
    }
    #[test]
    fn weighted_dl_basics() {
        let p = Stub::none();
        assert_eq!(weighted_dl("abc", "ABC", Qwerty, &p), 0.0);
        assert!((weighted_dl("bulid", "build", Qwerty, &p) - 0.7).abs() < 1e-5);
        assert!((weighted_dl("maupy", "maupay", Qwerty, &p) - 1.0).abs() < 1e-5);
    }
    #[test]
    fn capitalization_preserved() {
        assert_eq!(
            run("Bulid", &all(), &dict_with(&[]), &Stub::none()).as_deref(),
            Some("Build")
        );
    }
    #[test]
    fn all_caps_and_mixed_case_untouched() {
        let d = dict_with(&[("kids", 500), ("name", 500), ("node", 500)]);
        let p = Stub::none();
        // Each would be corrected if lowercase.
        assert_eq!(run("bulid", &all(), &d, &p).as_deref(), Some("build"));
        assert_eq!(run("kidc", &all(), &d, &p).as_deref(), Some("kids"));
        assert_eq!(run("nvme", &all(), &d, &p).as_deref(), Some("name"));
        for w in ["BULID", "OIDC", "NVME", "KIDC", "bULID", "BuLid", "buliD"] {
            assert_eq!(run(w, &all(), &d, &p), None, "{w}");
        }
        assert_eq!(run("Bulid", &all(), &d, &p).as_deref(), Some("Build"));
    }
    #[test]
    fn short_and_nonascii_skipped() {
        let d = dict_with(&[
            ("go", 1000),
            ("don't", 500),
            ("abcdefghijklmnopqrstuvwx", 100),
        ]);
        let p = Stub::none();
        // Would be corrected if the guards were absent.
        assert_eq!(run("gx", &all(), &d, &p), None);
        assert_eq!(run("büild", &all(), &d, &p), None);
        assert_eq!(run("abcdefghijklmnopqrstuvwxy", &all(), &d, &p), None);
        assert_eq!(run("", &all(), &d, &p), None);
        // Apostrophes are allowed.
        assert_eq!(run("dno't", &all(), &d, &p).as_deref(), Some("don't"));
    }
    #[test]
    fn far_frequent_candidate_blocks_by_margin() {
        let p = Stub::none();
        let d = dict_with(&[("cut", 1), ("cxxt", 1_000_000)]);
        let wide = LangCtx {
            margin: 0.8,
            ..all()
        };
        assert_eq!(run("cit", &wide, &d, &p), None);
        assert_eq!(run("cit", &all(), &d, &p).as_deref(), Some("cut"));
    }
    #[test]
    fn far_best_candidate_is_rejected() {
        // The far word wins on score but exceeds the 1.5 distance cap.
        let d = dict_with(&[("cxxt", 1_000_000)]);
        assert_eq!(run("cit", &all(), &d, &Stub::none()), None);
    }
    #[test]
    fn prev_word_lowercased_for_bigram() {
        // "cut"/"cot" tie on score; the bigram bonus (keyed lowercase) decides.
        let d = dict_with(&[("cut", 100), ("cot", 100)]);
        let mut p = Stub::none();
        p.bigrams.push(("the", "cut"));
        let r = decide("cit", Some("The"), Qwerty, &all(), &d, &p);
        assert_eq!(r.map(|v| v.replacement).as_deref(), Some("cut"));
    }
}
