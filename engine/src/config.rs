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

/// Apps that get autocorrect without being added by hand. Mirrored in
/// `extension/config.js` `DEFAULT_ALLOWLIST`.
pub const DEFAULT_ALLOWLIST: [&str; 5] = [
    "firefox",
    "firefox_firefox",
    "firefox-esr",
    "org.mozilla.firefox",
    "org.gnome.TextEditor",
];

impl Default for Config {
    /// English only. Use `default_for` to keep existing tl/war lexicons.
    fn default() -> Config {
        Config {
            languages: vec![LangSpec {
                code: "en".into(),
                lexicon: "builtin:en".into(),
            }],
            allowlist: DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect(),
            broken: Vec::new(),
            auto_switch_osk: true,
            osk_source: None,
            allow_any_caller: false,
        }
    }
}

impl Config {
    /// English, plus `tl` and `war` when their lexicon files already exist
    /// under `home` (installs from before English-only defaults keep them).
    /// Lexicon paths stay in `~/` form.
    pub fn default_for(home: &Path) -> Config {
        let mut c = Config::default();
        for code in ["tl", "war"] {
            let rel = format!(".local/share/correctiond/lexicons/{code}.txt");
            if home.join(&rel).is_file() {
                c.languages.push(LangSpec {
                    code: code.into(),
                    lexicon: format!("~/{rel}"),
                });
            }
        }
        c
    }

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
                Config::default_for(home)
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default_for(home),
            Err(_) => {
                eprintln!("correctiond: config.toml is unreadable, using defaults");
                Config::default_for(home)
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
        assert_eq!(c.languages, vec![lang("en", "builtin:en")]);
        assert_eq!(c.allowlist, DEFAULT_ALLOWLIST);
        assert!(c.broken.is_empty());
        assert!(c.auto_switch_osk);
        assert!(!c.allow_any_caller);
    }

    #[test]
    fn default_allowlist_is_pinned() {
        assert_eq!(
            Config::default().allowlist,
            [
                "firefox",
                "firefox_firefox",
                "firefox-esr",
                "org.mozilla.firefox",
                "org.gnome.TextEditor"
            ]
        );
    }

    #[test]
    fn default_for_follows_vectors() {
        let tsv = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/default-langs-vectors.tsv"),
        )
        .unwrap();
        let mut rows = 0;
        for line in tsv.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
            let f: Vec<&str> = line.split('\t').collect();
            let home = tempfile::tempdir().unwrap();
            let lex = home.path().join(".local/share/correctiond/lexicons");
            std::fs::create_dir_all(&lex).unwrap();
            if f[0] == "1" {
                std::fs::write(lex.join("tl.txt"), "a\t1\n").unwrap();
            }
            if f[1] == "1" {
                std::fs::write(lex.join("war.txt"), "a\t1\n").unwrap();
            }
            let c = Config::default_for(home.path());
            let codes: Vec<&str> = c.languages.iter().map(|l| l.code.as_str()).collect();
            assert_eq!(codes.join(","), f[2], "row {line}");
            if let Some(tl) = c.languages.iter().find(|l| l.code == "tl") {
                assert_eq!(tl.lexicon, "~/.local/share/correctiond/lexicons/tl.txt");
            }
            rows += 1;
        }
        assert_eq!(rows, 4);
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
