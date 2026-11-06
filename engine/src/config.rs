//! `~/.config/correctiond/config.toml`. A missing or invalid file means
//! defaults; nothing in here is ever fatal.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `LangId` is a `u8` index into at most 16 languages (`LangSet` is a u16).
pub const MAX_LANGUAGES: usize = 16;

/// One language: a code and where its `word<TAB>count` lexicon lives.
/// `builtin:en` is the English list compiled into the binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LangSpec {
    pub code: String,
    pub lexicon: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub languages: Vec<LangSpec>,
    pub allowlist: Vec<String>,
    pub broken: Vec<String>,
    /// Older form of `osk_source`: true = switch to `correctiond:us`.
    pub auto_switch_osk: bool,
    /// IBus engine the companion extension switches to while GJS OSK is
    /// open ("" = none). Read by the extension, not the engine.
    pub osk_source: Option<String>,
    pub allow_any_caller: bool,
}

impl Default for Config {
    fn default() -> Config {
        let lang = |code: &str, lexicon: &str| LangSpec {
            code: code.into(),
            lexicon: lexicon.into(),
        };
        Config {
            languages: vec![
                lang("en", "builtin:en"),
                lang("tl", "~/.local/share/correctiond/lexicons/tl.txt"),
                lang("war", "~/.local/share/correctiond/lexicons/war.txt"),
            ],
            allowlist: vec!["firefox".into()],
            broken: Vec::new(),
            auto_switch_osk: true,
            osk_source: None,
            allow_any_caller: false,
        }
    }
}

impl Config {
    pub fn path(home: &Path) -> PathBuf {
        home.join(".config/correctiond/config.toml")
    }

    pub fn load(home: &Path) -> Config {
        Config::load_from(&Config::path(home), home)
    }

    /// Never fails: a missing file is silent, an unreadable or invalid one
    /// logs a one-line warning (without the file's contents), and both give
    /// defaults. Lexicon paths come back with `~` expanded.
    pub fn load_from(path: &Path, home: &Path) -> Config {
        let mut c = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|_| {
                eprintln!("correctiond: config.toml is invalid, using defaults");
                Config::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(_) => {
                eprintln!("correctiond: config.toml is unreadable, using defaults");
                Config::default()
            }
        };
        if c.languages.len() > MAX_LANGUAGES {
            eprintln!(
                "correctiond: {} languages configured, ignoring all after the first {MAX_LANGUAGES}",
                c.languages.len()
            );
            c.languages.truncate(MAX_LANGUAGES);
        }
        for l in &mut c.languages {
            l.lexicon = expand_tilde(&l.lexicon, home);
        }
        c
    }
}

/// Expands a leading `~` or `~/` to `home`; anything else is unchanged.
pub fn expand_tilde(s: &str, home: &Path) -> String {
    if s == "~" {
        return home.to_string_lossy().into_owned();
    }
    match s.strip_prefix("~/") {
        Some(rest) => home.join(rest).to_string_lossy().into_owned(),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lang(code: &str, lexicon: &str) -> LangSpec {
        LangSpec {
            code: code.into(),
            lexicon: lexicon.into(),
        }
    }

    #[test]
    fn defaults_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let home = Path::new("/home/u");
        let c = Config::load_from(&dir.path().join("missing.toml"), home);
        assert_eq!(
            c.languages,
            vec![
                lang("en", "builtin:en"),
                lang("tl", "/home/u/.local/share/correctiond/lexicons/tl.txt"),
                lang("war", "/home/u/.local/share/correctiond/lexicons/war.txt"),
            ]
        );
        assert_eq!(c.allowlist, vec!["firefox".to_string()]);
        assert!(c.broken.is_empty());
        assert!(c.auto_switch_osk);
        assert!(!c.allow_any_caller);
    }

    #[test]
    fn reads_what_the_preferences_window_writes() {
        // extension/test/config.test.js asserts that config.js writes exactly
        // this file, so the window and the engine agree on the format.
        let p =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../extension/test/fixtures/config.toml");
        let c = Config::load_from(&p, Path::new("/home/u"));
        assert_eq!(
            c.languages,
            vec![
                lang("en", "builtin:en"),
                lang("tl", "/home/u/.local/share/correctiond/lexicons/tl.txt"),
                lang("war", "/opt/lex/wa\"r\\x.txt"),
            ]
        );
        assert_eq!(c.allowlist, vec!["firefox", "zen", "org.gnome.TextEditor"]);
        assert_eq!(c.broken, vec!["org.example.Broken"]);
        assert_eq!(c.osk_source.as_deref(), Some("correctiond:colemak"));
        assert!(!c.allow_any_caller);
    }

    #[test]
    fn invalid_file_means_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "languages = 3\n[[[").unwrap();
        let home = Path::new("/home/u");
        assert_eq!(
            Config::load_from(&p, home),
            Config::load_from(&dir.path().join("none"), home)
        );
    }

    #[test]
    fn custom_languages_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let text = r#"
allowlist = ["firefox", "org.gnome.TextEditor"]
auto_switch_osk = false

[[languages]]
code = "en"
lexicon = "builtin:en"

[[languages]]
code = "ceb"
lexicon = "/opt/lex/ceb.txt"
"#;
        std::fs::write(&p, text).unwrap();
        let c = Config::load_from(&p, Path::new("/home/u"));
        assert_eq!(
            c.languages,
            vec![lang("en", "builtin:en"), lang("ceb", "/opt/lex/ceb.txt")]
        );
        assert_eq!(c.allowlist, vec!["firefox", "org.gnome.TextEditor"]);
        assert!(!c.auto_switch_osk);
        // Unset keys keep their defaults.
        assert!(c.broken.is_empty());
        assert!(!c.allow_any_caller);
        // Serializing and reloading gives the same config back.
        std::fs::write(&p, toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(Config::load_from(&p, Path::new("/home/u")), c);
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = Path::new("/home/u");
        assert_eq!(expand_tilde("~/lex/tl.txt", home), "/home/u/lex/tl.txt");
        assert_eq!(expand_tilde("~", home), "/home/u");
        assert_eq!(expand_tilde("/abs/~/x", home), "/abs/~/x");
        assert_eq!(expand_tilde("~other/x", home), "~other/x");
        assert_eq!(expand_tilde("builtin:en", home), "builtin:en");

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "[[languages]]\ncode = \"tl\"\nlexicon = \"~/tl.txt\"\n").unwrap();
        let c = Config::load_from(&p, home);
        assert_eq!(c.languages, vec![lang("tl", "/home/u/tl.txt")]);
    }

    #[test]
    fn extra_languages_beyond_sixteen_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let mut text = String::new();
        for i in 0..20 {
            text.push_str(&format!(
                "[[languages]]\ncode = \"l{i}\"\nlexicon = \"/x/{i}.txt\"\n"
            ));
        }
        std::fs::write(&p, text).unwrap();
        let c = Config::load_from(&p, Path::new("/home/u"));
        assert_eq!(c.languages.len(), MAX_LANGUAGES);
        assert_eq!(c.languages[15].code, "l15");
    }
}
