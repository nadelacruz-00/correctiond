//! Replay evaluator (docs/designs/messenger-import.md step 2).
//!
//! Types held-out messages through the real `EngineState::on_key` path and
//! reports, per layout and per mode, how often the engine changes words it
//! should leave alone and how often it fixes planted typos. Counts only:
//! nothing in the output or on stderr is ever a word or a message.
//!
//! cargo run --release -p engine --example replay -- \
//!     --lexicon en=builtin:en --lexicon tl=PATH --lexicon war=PATH \
//!     --label-lexicon en=builtin:en --label-lexicon tl=PATH --label-lexicon war=PATH \
//!     --labels labels.tsv --heldout heldout.tokens --personal personal.vocab.tsv \
//!     --out replay.json

use corelib::dictionary::{Dictionary, LangId, LangSet};
use corelib::layout::{sub_cost, LayoutId};
use corelib::learning::{LearnStore, SharedStore};
use corelib::policy::{effective_layout, is_learnable, Mode};
use corelib::tokenizer::KeyClass;
use corelib::vocab::{parse_vocab, VocabBatch};
use engine::{build_dictionary, Action, Counters, EngineState};
use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

const USAGE: &str = "usage: replay --lexicon CODE=PATH... --label-lexicon CODE=PATH... \
    --heldout FILE --out FILE [--labels FILE] [--personal FILE]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match real_main(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(f) => {
            eprintln!("replay: {}", f.msg);
            ExitCode::from(f.code)
        }
    }
}

/// A fixed error line (never file content) and the exit code it gets.
#[derive(Debug)]
struct Fail {
    code: u8,
    msg: String,
}

impl From<String> for Fail {
    fn from(msg: String) -> Fail {
        Fail { code: 1, msg }
    }
}

impl From<&str> for Fail {
    fn from(msg: &str) -> Fail {
        Fail::from(msg.to_string())
    }
}

/// Exit 1 for usage and input errors, 2 for an unreadable lexicon.
fn real_main(args: &[String]) -> Result<(), Fail> {
    let a = Args::parse(args)?;
    // The loader skips a missing list with a warning; a replay against a
    // silently missing list would measure the wrong thing.
    check_readable("--lexicon", &a.lexicons)?;
    check_readable("--label-lexicon", &a.label_lexicons)?;
    let heldout = read(&a.heldout, "--heldout")?;
    let hand = match &a.labels {
        Some(p) => parse_labels(&read(p, "--labels")?)?,
        None => HashMap::new(),
    };
    let personal = match &a.personal {
        Some(p) => {
            parse_vocab(&read(p, "--personal")?)
                .map_err(|_| "--personal is not a valid vocab v1 file")?
                .0
        }
        None => VocabBatch::default(),
    };
    // Each dictionary is built once and shared by every run.
    let labeler = Labeler::new(&pairs(&a.label_lexicons));
    let dict = Arc::new(build_dictionary(&pairs(&a.lexicons)));
    let corpus = Corpus::new(&heldout, &labeler, &hand);
    let json = evaluate(&dict, &personal, &corpus);
    write_private(&a.out, &json).map_err(|_| "cannot write --out".into())
}

/// Every non-builtin lexicon is a file that opens for reading. Exit 2.
fn check_readable(flag: &str, langs: &[(String, String)]) -> Result<(), Fail> {
    for (code, path) in langs {
        if path.starts_with("builtin:") {
            continue;
        }
        let ok = std::fs::File::open(path)
            .and_then(|f| f.metadata())
            .is_ok_and(|m| m.is_file());
        if !ok {
            return Err(Fail {
                code: 2,
                msg: format!("{flag} {code}: file not readable"),
            });
        }
    }
    Ok(())
}

/// The file's text; errors name the flag, never the content.
fn read(path: &str, flag: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|_| format!("cannot read {flag}"))
}

fn pairs(v: &[(String, String)]) -> Vec<(&str, &str)> {
    v.iter().map(|(c, p)| (c.as_str(), p.as_str())).collect()
}

/// The output sits in the pipeline's 0600 work directory; keep it 0600.
fn write_private(path: &str, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(text.as_bytes())
}

#[derive(Debug, Default)]
struct Args {
    lexicons: Vec<(String, String)>,
    label_lexicons: Vec<(String, String)>,
    labels: Option<String>,
    heldout: String,
    personal: Option<String>,
    out: String,
}

impl Args {
    fn parse(args: &[String]) -> Result<Args, String> {
        let mut a = Args::default();
        let (mut heldout, mut out) = (None, None);
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let value = it.next().ok_or(USAGE)?.clone();
            match flag.as_str() {
                "--lexicon" => a.lexicons.push(code_path(&value)?),
                "--label-lexicon" => a.label_lexicons.push(code_path(&value)?),
                "--labels" => a.labels = Some(value),
                "--heldout" => heldout = Some(value),
                "--personal" => a.personal = Some(value),
                "--out" => out = Some(value),
                _ => return Err(USAGE.into()),
            }
        }
        a.heldout = heldout.ok_or(USAGE)?;
        a.out = out.ok_or(USAGE)?;
        if a.lexicons.is_empty() || a.label_lexicons.is_empty() {
            return Err(USAGE.into());
        }
        Ok(a)
    }
}

fn code_path(v: &str) -> Result<(String, String), String> {
    match v.split_once('=') {
        Some((c, p)) if !c.is_empty() && !p.is_empty() => Ok((c.into(), p.into())),
        _ => Err("expected CODE=PATH".into()),
    }
}

/// The engine's own learnable rule (3-24 ASCII letters or apostrophes, a
/// letter first, lowercase or Capitalized): the shapes it can learn or
/// correct. Everything else is typed but not scored.
fn scored_shape(tok: &str) -> bool {
    is_learnable(tok, Mode::Silent)
}

/// `word<TAB>label` lines; blank lines and `#` comments are skipped.
fn parse_labels(text: &str) -> Result<HashMap<String, Label>, String> {
    let mut out = HashMap::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = || {
            format!(
                "--labels line {}: expected word<TAB>war|tl|bisaya|slang|name|typo",
                i + 1
            )
        };
        let (w, l) = line.split_once('\t').ok_or_else(bad)?;
        let label = match l {
            "war" | "tl" | "bisaya" => Label::Chat,
            "slang" => Label::Slang,
            "name" | "typo" => Label::Excluded,
            _ => return Err(bad()),
        };
        if w.is_empty() {
            return Err(bad());
        }
        out.insert(w.to_lowercase(), label);
    }
    Ok(out)
}

/// The frozen pre-rebuild lists, used only to label tokens.
struct Labeler {
    dict: Dictionary,
    en: LangSet,
    fil: LangSet,
}

impl Labeler {
    fn new(langs: &[(&str, &str)]) -> Labeler {
        let (mut en, mut fil) = (LangSet::EMPTY, LangSet::EMPTY);
        for (i, (code, _)) in langs.iter().enumerate().take(16) {
            let id = LangId(i as u8);
            match *code {
                "en" => en.insert(id),
                "tl" | "war" => fil.insert(id),
                _ => {}
            }
        }
        if en.is_empty() {
            eprintln!("replay: no 'en' label lexicon; no typos will be planted");
        }
        Labeler {
            dict: build_dictionary(langs),
            en,
            fil,
        }
    }

    fn label(&self, tok: &str, hand: &HashMap<String, Label>) -> Label {
        if !scored_shape(tok) {
            return Label::Unscored;
        }
        if let Some(&l) = hand.get(&tok.to_lowercase()) {
            return l;
        }
        let langs = self.dict.langs_of(tok);
        if langs.0 & self.fil.0 != 0 {
            Label::Fil
        } else if langs.is_empty() {
            Label::Oov
        } else if langs.0 & !self.en.0 == 0 {
            Label::En
        } else {
            Label::Other
        }
    }
}

impl Corpus {
    /// One message per non-empty line, tokens split on single spaces.
    fn new(text: &str, labeler: &Labeler, hand: &HashMap<String, Label>) -> Corpus {
        let messages: Vec<Vec<String>> = text
            .lines()
            .map(|l| {
                l.trim_end_matches('\r')
                    .split(' ')
                    .filter(|t| !t.is_empty())
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .filter(|m| !m.is_empty())
            .collect();
        let labels = messages
            .iter()
            .map(|m| m.iter().map(|t| labeler.label(t, hand)).collect())
            .collect();
        Corpus { messages, labels }
    }

    fn scored(&self) -> usize {
        self.labels
            .iter()
            .flatten()
            .filter(|l| !matches!(l, Label::Unscored))
            .count()
    }
}

/// xorshift64: tiny, deterministic, and the same on every machine.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const SEED: u64 = 42;
/// Every 10th English token of at least 4 letters gets a typo.
const PLANT_EVERY: usize = 10;
const PLANT_MIN_LETTERS: usize = 4;

/// The keys next to `c` on `layout`, in alphabetical order.
fn neighbors(layout: LayoutId, c: char) -> Vec<char> {
    let c = c.to_ascii_lowercase();
    ('a'..='z')
        .filter(|&k| k != c && sub_cost(layout, c, k) < 1.0)
        .collect()
}

/// One neighbor-key substitution in every 10th English token, from a fresh
/// seed-42 generator, so a layout's set depends only on the labels.
fn plant(corpus: &Corpus, layout: LayoutId) -> Planted {
    let mut rng = XorShift(SEED);
    let mut seen = 0usize;
    let mut out = Vec::with_capacity(corpus.messages.len());
    for (msg, labels) in corpus.messages.iter().zip(&corpus.labels) {
        let mut row = vec![None; msg.len()];
        for (i, tok) in msg.iter().enumerate() {
            let letters = tok.chars().filter(|c| c.is_alphabetic()).count();
            if labels[i] != Label::En || letters < PLANT_MIN_LETTERS {
                continue;
            }
            seen += 1;
            if !seen.is_multiple_of(PLANT_EVERY) {
                continue;
            }
            let chars: Vec<char> = tok.chars().collect();
            let slots: Vec<usize> = (0..chars.len())
                .filter(|&j| !neighbors(layout, chars[j]).is_empty())
                .collect();
            if slots.is_empty() {
                continue;
            }
            let j = slots[rng.below(slots.len())];
            let options = neighbors(layout, chars[j]);
            let k = options[rng.below(options.len())];
            let mut typo = chars;
            typo[j] = if typo[j].is_uppercase() {
                k.to_ascii_uppercase()
            } else {
                k
            };
            row[i] = Some(typo.into_iter().collect());
        }
        out.push(row);
    }
    out
}

/// (numerator, denominator) per rate.
#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    chat: (u64, u64),
    fil: (u64, u64),
    slang: (u64, u64),
    typo: (u64, u64),
    oov: (u64, u64),
}

/// One replay of the whole corpus with learning off.
fn replay(
    dict: &Arc<Dictionary>,
    store: SharedStore,
    variant: LayoutId,
    osk: bool,
    corpus: &Corpus,
    planted: &Planted,
) -> Tally {
    let mut e = EngineState::new(dict.clone(), store, Arc::new(Counters::default()), variant);
    e.set_learning(false);
    let mut t = Tally::default();
    for (m, msg) in corpus.messages.iter().enumerate() {
        // A new text field: no language context, no previous word.
        e.on_invalidate();
        for (i, tok) in msg.iter().enumerate() {
            let typed = planted[m][i].as_deref().unwrap_or(tok);
            for c in typed.chars() {
                e.on_key(KeyClass::Printable(c), Mode::Silent, osk, None);
            }
            let a = e.on_key(KeyClass::Boundary(Some(' ')), Mode::Silent, osk, None);
            let committed = match &a {
                Action::Replace { commit, .. } => commit.strip_suffix(' ').unwrap_or(commit),
                Action::Pass => typed,
            };
            let changed = committed != typed;
            let bump = |r: &mut (u64, u64), hit: bool| {
                r.0 += u64::from(hit);
                r.1 += 1;
            };
            if planted[m][i].is_some() {
                bump(&mut t.typo, committed == tok);
                continue;
            }
            match corpus.labels[m][i] {
                Label::Chat => bump(&mut t.chat, changed),
                Label::Fil => bump(&mut t.fil, changed),
                Label::Slang => bump(&mut t.slang, changed),
                Label::Oov => bump(&mut t.oov, changed),
                Label::En | Label::Other | Label::Excluded | Label::Unscored => {}
            }
        }
    }
    t
}

/// Name, engine variant and OSK visibility. "osk" is the Qwerty variant
/// with the on-screen keyboard up, so taps use the OSK geometry.
const LAYOUTS: [(&str, LayoutId, bool); 3] = [
    ("qwerty", LayoutId::Qwerty, false),
    ("colemak", LayoutId::Colemak, false),
    ("osk", LayoutId::Qwerty, true),
];

/// Every layout x {lists_only, full}, as the output JSON.
fn evaluate(dict: &Arc<Dictionary>, personal: &VocabBatch, corpus: &Corpus) -> String {
    let mut layouts = Vec::new();
    for (name, variant, osk) in LAYOUTS {
        let planted = plant(corpus, effective_layout(variant, osk));
        let mut modes = Vec::new();
        for full in [false, true] {
            let mut store = LearnStore::memory();
            if full {
                store.add_imported(personal);
            }
            let store = Arc::new(Mutex::new(store));
            let t = replay(dict, store, variant, osk, corpus, &planted);
            let key = if full { "full" } else { "lists_only" };
            modes.push(format!("\"{key}\": {}", mode_json(&t, full)));
        }
        layouts.push(format!("\"{name}\": {{{}}}", modes.join(", ")));
    }
    format!(
        "{{\n  \"messages\": {},\n  \"scored_tokens\": {},\n  \"personal_words\": {},\n  \"layouts\": {{\n    {}\n  }}\n}}\n",
        corpus.messages.len(),
        corpus.scored(),
        personal.words.len(),
        layouts.join(",\n    ")
    )
}

/// A rate as a JSON number, or null with a zero denominator.
fn rate((n, d): (u64, u64)) -> String {
    if d == 0 {
        "null".into()
    } else {
        format!("{}", n as f64 / d as f64)
    }
}

fn mode_json(t: &Tally, full: bool) -> String {
    let mut f = vec![
        format!("\"chat_changed\": {}", t.chat.0),
        format!("\"chat_total\": {}", t.chat.1),
        format!("\"chat_changed_rate\": {}", rate(t.chat)),
        format!("\"fil_changed\": {}", t.fil.0),
        format!("\"fil_total\": {}", t.fil.1),
        format!("\"fil_changed_rate\": {}", rate(t.fil)),
        format!("\"typo_fixed\": {}", t.typo.0),
        format!("\"typo_planted\": {}", t.typo.1),
        format!("\"typo_fix_rate\": {}", rate(t.typo)),
        format!("\"oov_changed_count\": {}", t.oov.0),
        format!("\"oov_total\": {}", t.oov.1),
    ];
    // SLANG is protected only by personal vocabulary, so full mode only.
    if full {
        f.push(format!("\"slang_changed\": {}", t.slang.0));
        f.push(format!("\"slang_total\": {}", t.slang.1));
        f.push(format!("\"slang_changed_rate\": {}", rate(t.slang)));
    }
    format!("{{{}}}", f.join(", "))
}

/// Planted typo for each token (`None`: typed as written), per message.
type Planted = Vec<Vec<Option<String>>>;

/// What a held-out token is scored as; fixed once, before any replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Label {
    En,
    Fil,
    Chat,
    Slang,
    Oov,
    /// In a label list, but neither English-only nor Tagalog/Waray.
    Other,
    /// Hand-labeled name or typo.
    Excluded,
    /// Not learnable-shaped: typed, never scored.
    Unscored,
}

struct Corpus {
    messages: Vec<Vec<String>>,
    labels: Vec<Vec<Label>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const EN_LIST: &str = "hello\t100\nworld\t50\nthing\t40\n";
    const TL_LIST: &str = "kami\t40\nsalamat\t30\n";
    const WAR_LIST: &str = "maupay\t60\n";
    // Frozen label lists: house and zebra are English the lists under test
    // lack; kamo is Tagalog they lack.
    const EN_LABEL: &str = "hello\t100\nworld\t50\nthing\t40\nhouse\t30\nzebra\t5\n";
    const TL_LABEL: &str = "kami\t40\nsalamat\t30\nkamo\t20\n";
    const WAR_LABEL: &str = "maupay\t60\n";
    // zebra is in the English label list, but the hand label wins.
    const LABELS: &str =
        "bulig\twar\nthang\tbisaya\nlol\tslang\nhellz\tslang\nwrld\tslang\nzebra\tname\n";
    const PERSONAL: &str = "#correctiond-vocab v1\nw\thellz\t5\nw\tthang\t3\n";
    /// Five messages. EN tokens of 4+ letters, in order: 9 in message 1,
    /// "hello" opens message 2 (the 10th, planted), 10 in message 5 ending
    /// with "house" (the 20th, planted, but absent from the lists under
    /// test, so it can never be fixed back).
    const HELDOUT: &str = "\
hello world thing hello world thing hello world thing
hello thang wurld gstack
kami kamo salamat maupay bulig
zebra lol hellz wrld 123 ok
world thing hello world thing hello world thing hello house
";

    fn write(dir: &Path, name: &str, text: &str) -> String {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p.to_str().unwrap().to_string()
    }

    fn fixture_args(dir: &Path) -> Vec<String> {
        let mut a = Vec::new();
        let mut push = |k: &str, v: String| {
            a.push(k.to_string());
            a.push(v);
        };
        push("--lexicon", format!("en={}", write(dir, "en.txt", EN_LIST)));
        push("--lexicon", format!("tl={}", write(dir, "tl.txt", TL_LIST)));
        push(
            "--lexicon",
            format!("war={}", write(dir, "war.txt", WAR_LIST)),
        );
        push(
            "--label-lexicon",
            format!("en={}", write(dir, "en.label.txt", EN_LABEL)),
        );
        push(
            "--label-lexicon",
            format!("tl={}", write(dir, "tl.label.txt", TL_LABEL)),
        );
        push(
            "--label-lexicon",
            format!("war={}", write(dir, "war.label.txt", WAR_LABEL)),
        );
        push("--labels", write(dir, "labels.tsv", LABELS));
        push("--heldout", write(dir, "heldout.tokens", HELDOUT));
        push("--personal", write(dir, "personal.vocab.tsv", PERSONAL));
        push(
            "--out",
            dir.join("replay.json").to_str().unwrap().to_string(),
        );
        a
    }

    fn run_fixture() -> (String, serde_json::Value) {
        let tmp = tempfile::tempdir().unwrap();
        real_main(&fixture_args(tmp.path())).unwrap();
        let text = std::fs::read_to_string(tmp.path().join("replay.json")).unwrap();
        let v = serde_json::from_str(&text).unwrap();
        (text, v)
    }

    /// Hand-computed for every layout (each changed word has exactly one
    /// candidate within reach, so key geometry does not matter):
    /// - CHAT: thang (-> thing) and bulig (Tagalog context, nothing near).
    ///   Full mode: thang is personal vocabulary, so left alone.
    /// - FIL: kami, kamo (-> kami), salamat, maupay.
    /// - SLANG (full only): lol, hellz (personal), wrld (-> world).
    /// - OOV: wurld (-> world), gstack.
    /// - Typos: the planted hello is fixed; the planted house is not.
    #[test]
    fn fixture_rates_match_hand_computed_values() {
        let (_, v) = run_fixture();
        assert_eq!(v["messages"], 5);
        for layout in ["qwerty", "colemak", "osk"] {
            for mode in ["lists_only", "full"] {
                let m = &v["layouts"][layout][mode];
                let ctx = format!("{layout}/{mode}");
                let full = mode == "full";
                assert_eq!(m["chat_total"], 2, "{ctx}");
                assert_eq!(m["chat_changed"], if full { 0 } else { 1 }, "{ctx}");
                assert_eq!(
                    m["chat_changed_rate"],
                    if full { 0.0 } else { 0.5 },
                    "{ctx}"
                );
                assert_eq!(m["fil_total"], 4, "{ctx}");
                assert_eq!(m["fil_changed"], 1, "{ctx}");
                assert_eq!(m["fil_changed_rate"], 0.25, "{ctx}");
                assert_eq!(m["typo_planted"], 2, "{ctx}");
                assert_eq!(m["typo_fixed"], 1, "{ctx}");
                assert_eq!(m["typo_fix_rate"], 0.5, "{ctx}");
                assert_eq!(m["oov_total"], 2, "{ctx}");
                assert_eq!(m["oov_changed_count"], 1, "{ctx}");
                if full {
                    assert_eq!(m["slang_total"], 3, "{ctx}");
                    assert_eq!(m["slang_changed"], 1, "{ctx}");
                    assert_eq!(m["slang_changed_rate"], 1.0 / 3.0, "{ctx}");
                } else {
                    assert!(m.get("slang_changed_rate").is_none(), "{ctx}");
                }
            }
        }
    }

    #[test]
    fn zero_denominator_rate_is_null() {
        let tmp = tempfile::tempdir().unwrap();
        let mut args = fixture_args(tmp.path());
        // No hand labels: no CHAT or SLANG tokens at all.
        let i = args.iter().position(|a| a == "--labels").unwrap();
        args.drain(i..i + 2);
        real_main(&args).unwrap();
        let text = std::fs::read_to_string(tmp.path().join("replay.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let m = &v["layouts"]["qwerty"]["full"];
        assert_eq!(m["chat_total"], 0);
        assert!(m["chat_changed_rate"].is_null());
        assert!(m["slang_changed_rate"].is_null());
    }

    #[test]
    fn output_carries_no_words() {
        let (text, v) = run_fixture();
        // Every key is a fixed field name and no value is a string.
        fn walk(v: &serde_json::Value, keys: &mut Vec<String>) {
            match v {
                serde_json::Value::Object(m) => {
                    for (k, x) in m {
                        keys.push(k.clone());
                        walk(x, keys);
                    }
                }
                serde_json::Value::String(_) => panic!("string value in output"),
                serde_json::Value::Array(_) => panic!("array in output"),
                _ => {}
            }
        }
        let mut keys = Vec::new();
        walk(&v, &mut keys);
        const FIELDS: &[&str] = &[
            "messages",
            "scored_tokens",
            "personal_words",
            "layouts",
            "qwerty",
            "colemak",
            "osk",
            "lists_only",
            "full",
            "chat_changed",
            "chat_total",
            "chat_changed_rate",
            "fil_changed",
            "fil_total",
            "fil_changed_rate",
            "typo_fixed",
            "typo_planted",
            "typo_fix_rate",
            "oov_changed_count",
            "oov_total",
            "slang_changed",
            "slang_total",
            "slang_changed_rate",
        ];
        for k in &keys {
            assert!(FIELDS.contains(&k.as_str()), "unexpected key {k}");
        }
        for w in HELDOUT.split_whitespace().filter(|w| w.len() >= 4) {
            assert!(!text.contains(w), "{w}");
        }
    }

    fn fixture_corpus(dir: &Path) -> Corpus {
        let langs = [
            ("en", write(dir, "en.label.txt", EN_LABEL)),
            ("tl", write(dir, "tl.label.txt", TL_LABEL)),
            ("war", write(dir, "war.label.txt", WAR_LABEL)),
        ];
        let pairs: Vec<(&str, &str)> = langs.iter().map(|(c, p)| (*c, p.as_str())).collect();
        let labeler = Labeler::new(&pairs);
        let hand = parse_labels(LABELS).unwrap();
        Corpus::new(HELDOUT, &labeler, &hand)
    }

    #[test]
    fn same_seed_gives_the_same_planted_set() {
        let tmp = tempfile::tempdir().unwrap();
        let corpus = fixture_corpus(tmp.path());
        for layout in [LayoutId::Qwerty, LayoutId::Colemak, LayoutId::OskTap] {
            let a = plant(&corpus, layout);
            assert_eq!(a, plant(&corpus, layout), "{layout:?}");
            // The 10th and 20th English tokens, each one neighbor key off.
            let mut n = 0;
            for (m, msg) in corpus.messages.iter().enumerate() {
                for (i, tok) in msg.iter().enumerate() {
                    let Some(p) = &a[m][i] else { continue };
                    n += 1;
                    assert_eq!(corpus.labels[m][i], Label::En);
                    let diff: Vec<(char, char)> =
                        tok.chars().zip(p.chars()).filter(|(x, y)| x != y).collect();
                    assert_eq!(diff.len(), 1, "{layout:?}");
                    assert_eq!(tok.chars().count(), p.chars().count());
                    let (x, y) = diff[0];
                    assert!(sub_cost(layout, x, y) < 1.0, "{layout:?}");
                }
            }
            assert_eq!(n, 2, "{layout:?}");
            assert!(a[1][0].is_some() && a[4][9].is_some(), "{layout:?}");
        }
    }

    /// Seed 42, xorshift64 and alphabetical neighbor order, pinned. The
    /// values were cross-checked with an independent re-implementation.
    /// OSK taps use QWERTY geometry, so osk plants what qwerty plants.
    #[test]
    fn seed_42_planted_typos_are_pinned() {
        let tmp = tempfile::tempdir().unwrap();
        let corpus = fixture_corpus(tmp.path());
        for (layout, hello, house) in [
            (LayoutId::Qwerty, "hellp", "housf"),
            (LayoutId::Colemak, "helly", "housu"),
            (LayoutId::OskTap, "hellp", "housf"),
        ] {
            let a = plant(&corpus, layout);
            assert_eq!(a[1][0].as_deref(), Some(hello), "{layout:?}");
            assert_eq!(a[4][9].as_deref(), Some(house), "{layout:?}");
        }
    }

    #[test]
    fn unreadable_lexicon_fails_with_code_2() {
        for (flag, file) in [("--lexicon", "tl.txt"), ("--label-lexicon", "tl.label.txt")] {
            for broken in ["missing", "directory"] {
                let tmp = tempfile::tempdir().unwrap();
                let mut args = fixture_args(tmp.path());
                let path = tmp.path().join(file);
                std::fs::remove_file(&path).unwrap();
                if broken == "directory" {
                    std::fs::create_dir(&path).unwrap();
                }
                let f = real_main(&args).unwrap_err();
                assert_eq!(f.code, 2, "{flag} {broken}");
                assert_eq!(f.msg, format!("{flag} tl: file not readable"));
                assert!(!tmp.path().join("replay.json").exists());
                // builtin: entries are never checked as files (the loader
                // warns about an unknown one and goes on).
                for a in args.iter_mut().filter(|a| a.ends_with(file)) {
                    *a = "tl=builtin:tl".into();
                }
                real_main(&args).unwrap();
            }
        }
    }

    /// Lists and label lists in one: en {the, bad, bed, world}, tl {kami}.
    fn invariant_corpus(dir: &Path, heldout: &str) -> (Arc<Dictionary>, Corpus) {
        let en = write(dir, "en.txt", "the\t1000\nbad\t1778\nbed\t10\nworld\t50\n");
        let tl = write(dir, "tl.txt", "kami\t40\n");
        let pairs = [("en", en.as_str()), ("tl", tl.as_str())];
        let dict = Arc::new(build_dictionary(&pairs));
        let corpus = Corpus::new(heldout, &Labeler::new(&pairs), &HashMap::new());
        (dict, corpus)
    }

    fn replay_qwerty(dict: &Arc<Dictionary>, corpus: &Corpus) -> Tally {
        let planted = plant(corpus, LayoutId::Qwerty);
        let store = Arc::new(Mutex::new(LearnStore::memory()));
        replay(dict, store, LayoutId::Qwerty, false, corpus, &planted)
    }

    /// "bod" is out of every list, one substitution from both "bad" and
    /// "bed" (1.0 each on QWERTY). "bad" is 178x more frequent, so it wins
    /// by 0.45 > the 0.3 focused margin and every "bod" is changed. With
    /// learning on, "the bed" teaches the bigram (the, bed); its 0.3 bonus
    /// leaves "bed" 0.15 behind, inside the margin, so "bod" would stop
    /// being changed (and, left alone twice, become personal vocabulary).
    #[test]
    fn frozen_learning_changes_every_repeat() {
        let tmp = tempfile::tempdir().unwrap();
        let (dict, corpus) = invariant_corpus(tmp.path(), "the bed\nthe bod\nthe bod\nthe bod\n");
        let t = replay_qwerty(&dict, &corpus);
        assert_eq!(t.oov, (3, 3));
    }

    /// "wurld" opens its own message: no language context, so "world" is
    /// in reach. Carried over from the Tagalog message before, the context
    /// would allow Tagalog only and leave it alone.
    #[test]
    fn every_message_starts_a_new_field() {
        let tmp = tempfile::tempdir().unwrap();
        let (dict, corpus) = invariant_corpus(tmp.path(), "kami kami\nwurld\n");
        let t = replay_qwerty(&dict, &corpus);
        assert_eq!(t.oov, (1, 1));
    }

    #[test]
    fn labels_follow_lists_and_hand_labels() {
        let tmp = tempfile::tempdir().unwrap();
        let c = fixture_corpus(tmp.path());
        use Label::*;
        assert_eq!(c.labels[1], [En, Chat, Oov, Oov]);
        assert_eq!(c.labels[2], [Fil, Fil, Fil, Fil, Chat]);
        assert_eq!(
            c.labels[3],
            [Excluded, Slang, Slang, Slang, Unscored, Unscored]
        );
    }

    #[test]
    fn bad_label_is_rejected_without_echoing_the_word() {
        let e = parse_labels("sikreto\tpassword\n").unwrap_err();
        assert!(!e.contains("sikreto") && !e.contains("password"), "{e}");
        assert!(e.contains("line 1"), "{e}");
    }
}
