//! The engine's decision code as a library, so dev tools (the replay
//! evaluator in `examples/replay.rs`) run exactly what the IBus engine runs.
//!
//! The surface is deliberately narrow: `EngineState`, `Action`, `Counters`
//! and the lexicon loader. IBus, persistence, the control plane, the keyring
//! and the trace stay private to the binary (`main.rs`).

mod state;

pub use state::{Action, Counters, EngineState};

use corelib::dictionary::{parse_lexicon, Dictionary, LangId};

/// The English lexicon is compiled in, so a package needs no data file.
const BUILTIN_EN: &str = include_str!("../../data/en_words.txt");

/// One dictionary from `(code, lexicon)` pairs, languages in the given
/// order (`LangId` = position). `lexicon` is a file path or `builtin:en`.
/// A lexicon that cannot be read is skipped with a warning on stderr and
/// never stops startup.
pub fn build_dictionary(langs: &[(&str, &str)]) -> Dictionary {
    let mut warnings = Vec::new();
    let dict = build_dictionary_logged(langs, &mut warnings);
    for w in &warnings {
        eprintln!("{w}");
    }
    dict
}

/// The startup line for a configured lexicon file that does not exist.
/// The path is config, never typed content, so it may be printed.
fn missing_lexicon_line(code: &str, path: &str) -> String {
    format!(
        "correctiond: WARNING: no lexicon for language '{code}' at {path}; \
         words in that language may be corrected to English until it is built \
         (see data/ATTRIBUTION.md)"
    )
}

/// `build_dictionary`, collecting its warning lines instead of printing them.
fn build_dictionary_logged(langs: &[(&str, &str)], warnings: &mut Vec<String>) -> Dictionary {
    let mut lexicons = Vec::with_capacity(langs.len());
    for (i, &(code, lexicon)) in langs.iter().enumerate() {
        let Ok(id) = u8::try_from(i) else { break };
        let words = match lexicon {
            "builtin:en" => parse_lexicon(BUILTIN_EN),
            s if s.starts_with("builtin:") => {
                warnings.push(format!(
                    "correctiond: no builtin lexicon for language '{code}', skipped"
                ));
                continue;
            }
            path => match std::fs::read_to_string(path) {
                Ok(text) => parse_lexicon(&text),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    warnings.push(missing_lexicon_line(code, path));
                    continue;
                }
                Err(_) => {
                    warnings.push(format!(
                        "correctiond: WARNING: lexicon for language '{code}' at {path} cannot be read, skipped"
                    ));
                    continue;
                }
            },
        };
        lexicons.push((LangId(id), words));
    }
    Dictionary::from_lexicons(lexicons)
}

#[cfg(test)]
mod tests {
    use super::*;
    use corelib::layout::LayoutId;
    use corelib::learning::LearnStore;
    use corelib::policy::Mode;
    use corelib::tokenizer::KeyClass;
    use std::sync::{Arc, Mutex};

    fn type_word(e: &mut EngineState, w: &str) -> Action {
        for c in w.chars() {
            e.on_key(KeyClass::Printable(c), Mode::Silent, false, None);
        }
        e.on_key(KeyClass::Boundary(Some(' ')), Mode::Silent, false, None)
    }

    /// The shipped English list and the plan's pinned margins: "bulid"
    /// (build 0.34 vs bulls 0.68) is fixed once the field's language is
    /// known (margin 0.3), but not as a field's first word (open margin
    /// 0.6). scripts/e2e.sh therefore uses a fixture lexicon.
    #[test]
    fn builtin_english_fixes_bulid_after_an_english_word() {
        let dict = Arc::new(build_dictionary(&[("en", "builtin:en")]));
        let mut e = EngineState::new(
            dict,
            Arc::new(Mutex::new(LearnStore::memory())),
            Arc::new(Counters::default()),
            LayoutId::Qwerty,
        );
        assert_eq!(type_word(&mut e, "bulid"), Action::Pass);
        e.on_invalidate();
        assert_eq!(type_word(&mut e, "the"), Action::Pass);
        assert_eq!(
            type_word(&mut e, "bulid"),
            Action::Replace {
                delete_chars: 5,
                commit: "build ".into(),
                consume: true
            }
        );
    }

    /// The reviewer's measured rewrites (OIDC->Kids, NVME->Name,
    /// BULID->Build) with the shipped list: only lowercase or Capitalized
    /// input is ever corrected.
    #[test]
    fn builtin_english_leaves_all_caps_alone() {
        let dict = Arc::new(build_dictionary(&[("en", "builtin:en")]));
        let mut e = EngineState::new(
            dict,
            Arc::new(Mutex::new(LearnStore::memory())),
            Arc::new(Counters::default()),
            LayoutId::Qwerty,
        );
        assert_eq!(type_word(&mut e, "the"), Action::Pass);
        for w in ["OIDC", "NVME", "BULID"] {
            assert_eq!(type_word(&mut e, w), Action::Pass, "{w}");
        }
        assert_eq!(
            type_word(&mut e, "Bulid"),
            Action::Replace {
                delete_chars: 5,
                commit: "Build ".into(),
                consume: true
            }
        );
    }

    #[test]
    fn missing_lexicon_is_one_warning_line_with_code_and_path() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lexicons/tl.txt");
        let path = path.to_str().unwrap();
        let langs = [("en", "builtin:en"), ("tl", path)];
        let mut warnings = Vec::new();
        let dict = build_dictionary_logged(&langs, &mut warnings);
        assert!(dict.contains("the"));
        assert_eq!(warnings, [missing_lexicon_line("tl", path)]);
        let w = &warnings[0];
        assert!(!w.contains('\n'));
        assert!(w.starts_with("correctiond: WARNING: "), "{w}");
        assert!(w.contains("'tl'") && w.contains(path), "{w}");
        assert!(w.contains("corrected to English"), "{w}");

        // Present: no warning, and the word is known.
        std::fs::create_dir_all(tmp.path().join("lexicons")).unwrap();
        std::fs::write(path, "naman\t100\n").unwrap();
        warnings.clear();
        let dict = build_dictionary_logged(&langs, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(dict.contains("naman"));
    }
}
