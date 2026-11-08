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
    pub modules: Modules,
}

/// Optional engine modules. `None` means the key is absent: the caller
/// decides what that means (see the spec).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modules {
    pub touch: Option<bool>,
}

/// Where a config file stopped parsing. Both numbers start at 1. `key` stays
/// `None`: toml 0.9 errors carry a span and a message but no key path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub line: usize,
    pub column: usize,
    pub key: Option<String>,
}

/// One-line, content-free description of a `ConfigError`.
pub fn invalid_line(e: &ConfigError) -> String {
    let key = match &e.key {
        Some(k) => format!(" (key {k})"),
        None => String::new(),
    };
    format!(
        "correctiond: config.toml is invalid at line {}, column {}{key}; using defaults. Fix or delete ~/.config/correctiond/config.toml",
        e.line, e.column
    )
}

const TOUCH_WARNING: &str = "correctiond: config.toml modules.touch must be true or false; ignored";

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawModules {
    touch: Option<toml::Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawConfig {
    languages: Option<Vec<LangSpec>>,
    allowlist: Option<Vec<String>>,
    broken: Option<Vec<String>>,
    auto_switch_osk: Option<bool>,
    osk_source: Option<String>,
    allow_any_caller: Option<bool>,
    modules: RawModules,
}

/// Parses config text without logging. The `Vec` holds warning texts. On
/// `Err` the caller uses `Config::default_for(home)`. Lexicon paths keep any
/// `~`.
pub fn parse_report(text: &str, home: &Path) -> (Result<Config, ConfigError>, Vec<&'static str>) {
    let raw: RawConfig = match toml::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            let start = e.span().map_or(0, |s| s.start.min(text.len()));
            let before = text.get(..start).unwrap_or("");
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            return (
                Err(ConfigError {
                    line,
                    column,
                    key: None,
                }),
                Vec::new(),
            );
        }
    };
    let mut warnings = Vec::new();
    let touch = match raw.modules.touch {
        None => None,
        Some(toml::Value::Boolean(b)) => Some(b),
        Some(_) => {
            warnings.push(TOUCH_WARNING);
            None
        }
    };
    let d = Config::default_for(home);
    let c = Config {
        languages: raw.languages.unwrap_or(d.languages),
        allowlist: raw.allowlist.unwrap_or(d.allowlist),
        broken: raw.broken.unwrap_or(d.broken),
        auto_switch_osk: raw.auto_switch_osk.unwrap_or(d.auto_switch_osk),
        osk_source: raw.osk_source.or(d.osk_source),
        allow_any_caller: raw.allow_any_caller.unwrap_or(d.allow_any_caller),
        modules: Modules { touch },
    };
    (Ok(c), warnings)
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
            modules: Modules::default(),
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
            Ok(text) => {
                let (parsed, warnings) = parse_report(&text, home);
                for w in warnings {
                    eprintln!("{w}");
                }
                parsed.unwrap_or_else(|e| {
                    eprintln!("{}", invalid_line(&e));
                    Config::default_for(home)
                })
            }
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

    fn home_with_tl() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let lex = home.path().join(".local/share/correctiond/lexicons");
        std::fs::create_dir_all(&lex).unwrap();
        std::fs::write(lex.join("tl.txt"), "a\t1\n").unwrap();
        home
    }

    fn codes(c: &Config) -> Vec<&str> {
        c.languages.iter().map(|l| l.code.as_str()).collect()
    }

    #[test]
    fn missing_languages_uses_legacy_rule() {
        let home = home_with_tl();
        let p = home.path().join("config.toml");
        std::fs::write(&p, "allowlist = [\"x\"]\n").unwrap();
        let c = Config::load_from(&p, home.path());
        assert_eq!(codes(&c), ["en", "tl"]);
        assert_eq!(c.allowlist, ["x"]);
    }

    #[test]
    fn invalid_file_uses_default_for() {
        let home = home_with_tl();
        let p = home.path().join("config.toml");
        std::fs::write(&p, "languages = 3\n[[[").unwrap();
        assert_eq!(codes(&Config::load_from(&p, home.path())), ["en", "tl"]);
    }

    #[test]
    fn unreadable_file_uses_default_for() {
        let home = home_with_tl();
        let dir = home.path().join("adir");
        std::fs::create_dir(&dir).unwrap();
        assert_eq!(codes(&Config::load_from(&dir, home.path())), ["en", "tl"]);
    }

    #[test]
    fn touch_true_false_and_absent() {
        let home = Path::new("/home/u");
        let t = |s: &str| parse_report(s, home).0.unwrap().modules.touch;
        assert_eq!(t("[modules]\ntouch = true"), Some(true));
        assert_eq!(t("[modules]\ntouch = false"), Some(false));
        assert_eq!(t("allowlist = []"), None);
    }

    #[test]
    fn touch_wrong_type_is_ignored() {
        let (r, w) = parse_report(
            "allowlist = [\"x\"]\n[modules]\ntouch = \"yes\"",
            Path::new("/home/u"),
        );
        let c = r.unwrap();
        assert_eq!(c.modules.touch, None);
        assert_eq!(c.allowlist, ["x"]);
        assert_eq!(
            w,
            ["correctiond: config.toml modules.touch must be true or false; ignored"]
        );
    }

    const BAD: &str = "allowlist = [\"a\"]\nbroken = 3\n";

    #[test]
    fn error_position_comes_from_span() {
        let e = parse_report(BAD, Path::new("/home/u")).0.unwrap_err();
        assert_eq!((e.line, e.column), (2, 10));
    }

    #[test]
    fn error_text_has_no_snippet() {
        let e = parse_report(BAD, Path::new("/home/u")).0.unwrap_err();
        let s = invalid_line(&e);
        assert!(s.starts_with("correctiond: config.toml is invalid at line 2, column "));
        assert!(s.ends_with("; using defaults. Fix or delete ~/.config/correctiond/config.toml"));
        assert!(!s.contains("broken = 3"));
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
