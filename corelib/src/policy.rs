use crate::dictionary::{LangId, LangSet};
use crate::layout::LayoutId;
use crate::scoring::LangCtx;

// IBus InputPurpose raw values (mirrors GtkInputPurpose); verified via python-gi.
const PURPOSE_DIGITS: u32 = 2;
const PURPOSE_NUMBER: u32 = 3;
const PURPOSE_PHONE: u32 = 4;
const PURPOSE_URL: u32 = 5;
const PURPOSE_EMAIL: u32 = 6;
const PURPOSE_PASSWORD: u32 = 8;
const PURPOSE_PIN: u32 = 9;
const PURPOSE_TERMINAL: u32 = 10;

/// Purposes where nothing typed is ever corrected or learned.
pub const OFF_PURPOSES: [u32; 7] = [
    PURPOSE_DIGITS,
    PURPOSE_NUMBER,
    PURPOSE_PHONE,
    PURPOSE_URL,
    PURPOSE_EMAIL,
    PURPOSE_PASSWORD,
    PURPOSE_PIN,
];

/// IBus InputHints bits.
pub const HINT_NO_SPELLCHECK: u32 = 1 << 1;
pub const HINT_PRIVATE: u32 = 1 << 11;

/// Lowercased wm_class values treated as terminals.
pub const TERMINAL_WM_CLASSES: [&str; 15] = [
    "org.gnome.ptyxis",
    "org.gnome.console",
    "kgx",
    "gnome-terminal-server",
    "org.gnome.terminal",
    // Third-party terminals that may not report purpose TERMINAL.
    "alacritty",
    "kitty",
    "foot",
    "footclient",
    "org.wezfurlong.wezterm",
    "xterm", // also XTerm: matching is on the lowercased wm_class
    "com.mitchellh.ghostty",
    "terminator",
    "com.gexperts.tilix",
    "org.kde.konsole",
];

const LANG_WINDOW: usize = 4;
const MARGIN_FOCUSED: f32 = 0.3;
const MARGIN_OPEN: f32 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// No correction and no learning.
    Off,
    /// Learn only, never correct.
    Observe,
    Silent,
    ProseAware,
}

#[derive(Debug, Clone)]
pub struct Ctx {
    pub purpose: u32,
    pub hints: u32,
    pub wm_class: Option<String>,
    pub osk_visible: bool,
}

/// Why `resolve_mode_reason` chose a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Allowlisted,
    NotListed,
    Broken,
    PrivateField,
    Terminal,
    Unidentified,
    /// Never returned by `resolve_mode_reason`; the engine sets it.
    NoReplace,
}

impl Reason {
    pub fn code(self) -> &'static str {
        match self {
            Reason::Allowlisted => "allowlisted",
            Reason::NotListed => "not_listed",
            Reason::Broken => "broken",
            Reason::PrivateField => "private_field",
            Reason::Terminal => "terminal",
            Reason::Unidentified => "unidentified",
            Reason::NoReplace => "no_replace",
        }
    }
}

pub fn resolve_mode(ctx: &Ctx, allowlist: &[String], broken: &[String]) -> Mode {
    resolve_mode_reason(ctx, allowlist, broken).0
}

pub fn resolve_mode_reason(ctx: &Ctx, allowlist: &[String], broken: &[String]) -> (Mode, Reason) {
    if OFF_PURPOSES.contains(&ctx.purpose) || ctx.hints & (HINT_NO_SPELLCHECK | HINT_PRIVATE) != 0 {
        return (Mode::Off, Reason::PrivateField);
    }
    let wm = ctx.wm_class.as_ref().map(|w| w.to_lowercase());
    let listed = |list: &[String], w: &str| list.iter().any(|a| a.to_lowercase() == w);
    let is_terminal = ctx.purpose == PURPOSE_TERMINAL
        || wm
            .as_deref()
            .is_some_and(|w| TERMINAL_WM_CLASSES.contains(&w));
    if is_terminal {
        return match wm.as_deref() {
            Some(w) if listed(allowlist, w) && !listed(broken, w) => {
                (Mode::ProseAware, Reason::Allowlisted)
            }
            _ => (Mode::Off, Reason::Terminal),
        };
    }
    match wm.as_deref() {
        Some(w) if !listed(broken, w) && listed(allowlist, w) => {
            (Mode::Silent, Reason::Allowlisted)
        }
        Some(w) if listed(broken, w) && listed(allowlist, w) => (Mode::Observe, Reason::Broken),
        Some(_) => (Mode::Observe, Reason::NotListed),
        None => (Mode::Observe, Reason::Unidentified),
    }
}

/// Recent language evidence for one field.
#[derive(Debug, Clone)]
pub struct LangWindow {
    n_langs: usize,
    recent: Vec<LangId>,
}

impl LangWindow {
    pub fn new(n_langs: usize) -> LangWindow {
        LangWindow {
            n_langs,
            recent: Vec::with_capacity(LANG_WINDOW),
        }
    }

    pub fn push(&mut self, lang: LangId) {
        if self.recent.len() == LANG_WINDOW {
            self.recent.remove(0);
        }
        self.recent.push(lang);
    }

    pub fn reset(&mut self) {
        self.recent.clear();
    }

    /// Language with the most votes; a tie for first (or empty) gives None.
    pub fn dominant(&self) -> Option<LangId> {
        let mut counts: Vec<(LangId, usize)> = Vec::new();
        for &l in &self.recent {
            match counts.iter_mut().find(|(k, _)| *k == l) {
                Some(e) => e.1 += 1,
                None => counts.push((l, 1)),
            }
        }
        let max = counts.iter().map(|e| e.1).max()?;
        let mut top = counts.iter().filter(|e| e.1 == max);
        let first = top.next()?;
        if top.next().is_some() {
            None
        } else {
            Some(first.0)
        }
    }

    pub fn ctx(&self) -> LangCtx {
        match self.dominant() {
            Some(l) => {
                let mut allowed = LangSet::EMPTY;
                allowed.insert(l);
                LangCtx {
                    allowed,
                    margin: MARGIN_FOCUSED,
                }
            }
            None => LangCtx {
                allowed: LangSet::all(self.n_langs),
                margin: MARGIN_OPEN,
            },
        }
    }
}

pub fn effective_layout(variant: LayoutId, osk_visible: bool) -> LayoutId {
    if osk_visible {
        LayoutId::OskTap
    } else {
        variant
    }
}

/// Lowercase or Capitalized: the tail after the first char has no uppercase.
pub(crate) fn case_shape_ok(token: &str) -> bool {
    token.bytes().skip(1).all(|b| !b.is_ascii_uppercase())
}

/// Clean prose word as seen from a terminal: ASCII letters only, >= 4 chars.
pub fn terminal_token_ok(token: &str) -> bool {
    token.len() >= 4 && token.bytes().all(|b| b.is_ascii_alphabetic()) && case_shape_ok(token)
}

/// Counts consecutive clean prose words completed before the candidate.
#[derive(Debug, Clone, Default)]
pub struct ProseRun {
    clean: u32,
}

impl ProseRun {
    pub fn on_word_completed(&mut self, token: &str) {
        if terminal_token_ok(token) {
            self.clean = self.clean.saturating_add(1);
        } else {
            self.clean = 0;
        }
    }

    pub fn reset(&mut self) {
        self.clean = 0;
    }

    pub fn ready(&self) -> bool {
        self.clean >= 2
    }
}

/// Data-minimization gate for every write to the store.
pub fn is_learnable(token: &str, mode: Mode) -> bool {
    match mode {
        Mode::Off | Mode::ProseAware => false,
        Mode::Silent | Mode::Observe => {
            (3..=24).contains(&token.len())
                && token.bytes().all(|b| b.is_ascii_alphabetic() || b == b'\'')
                && token
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphabetic())
                && case_shape_ok(token)
        }
    }
}

pub fn bigram_learnable(mode: Mode) -> bool {
    matches!(mode, Mode::Silent | Mode::Observe)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EN: LangId = LangId(0);
    const TL: LangId = LangId(1);

    fn ctx(purpose: u32, hints: u32, wm: Option<&str>) -> Ctx {
        Ctx {
            purpose,
            hints,
            wm_class: wm.map(String::from),
            osk_visible: false,
        }
    }
    fn al(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn password_hard_off() {
        let c = ctx(8, 0, Some("firefox"));
        assert_eq!(resolve_mode(&c, &al(&["firefox"]), &[]), Mode::Off);
    }
    #[test]
    fn sensitive_purposes_off() {
        for p in [2, 3, 4, 5, 6, 9] {
            let c = ctx(p, 0, Some("firefox"));
            assert_eq!(resolve_mode(&c, &al(&["firefox"]), &[]), Mode::Off, "{p}");
        }
    }
    #[test]
    fn private_hint_off() {
        let c = ctx(0, HINT_PRIVATE, Some("firefox"));
        assert_eq!(resolve_mode(&c, &al(&["firefox"]), &[]), Mode::Off);
        let c = ctx(0, HINT_NO_SPELLCHECK, Some("firefox"));
        assert_eq!(resolve_mode(&c, &al(&["firefox"]), &[]), Mode::Off);
    }
    #[test]
    fn reason_per_rule() {
        let pty = "org.gnome.ptyxis";
        let r = |c: Ctx, a: &[&str], b: &[&str]| resolve_mode_reason(&c, &al(a), &al(b));
        assert_eq!(
            r(ctx(8, 0, Some("firefox")), &["firefox"], &[]),
            (Mode::Off, Reason::PrivateField)
        );
        assert_eq!(
            r(ctx(0, HINT_PRIVATE, Some("firefox")), &["firefox"], &[]),
            (Mode::Off, Reason::PrivateField)
        );
        assert_eq!(
            r(ctx(0, 0, Some(pty)), &["firefox"], &[]),
            (Mode::Off, Reason::Terminal)
        );
        assert_eq!(
            r(ctx(0, 0, Some(pty)), &[pty], &[pty]),
            (Mode::Off, Reason::Terminal)
        );
        assert_eq!(
            r(ctx(0, 0, Some(pty)), &[pty], &[]),
            (Mode::ProseAware, Reason::Allowlisted)
        );
        assert_eq!(
            r(ctx(0, 0, Some("firefox")), &["firefox"], &[]),
            (Mode::Silent, Reason::Allowlisted)
        );
        assert_eq!(
            r(ctx(0, 0, Some("firefox")), &["firefox"], &["firefox"]),
            (Mode::Observe, Reason::Broken)
        );
        assert_eq!(
            r(ctx(0, 0, Some("files")), &["firefox"], &[]),
            (Mode::Observe, Reason::NotListed)
        );
        assert_eq!(
            r(ctx(0, 0, None), &["firefox"], &[]),
            (Mode::Observe, Reason::Unidentified)
        );
    }
    #[test]
    fn reason_wrapper_matches_resolve_mode() {
        let pty = "org.gnome.ptyxis";
        let a = al(&[pty, "firefox", "foot"]);
        let b = al(&["foot"]);
        for purpose in [0, 8, 10] {
            for hints in [0, HINT_PRIVATE, HINT_NO_SPELLCHECK] {
                for wm in [
                    None,
                    Some("firefox"),
                    Some(pty),
                    Some("foot"),
                    Some("files"),
                ] {
                    let c = ctx(purpose, hints, wm);
                    assert_eq!(resolve_mode_reason(&c, &a, &b).0, resolve_mode(&c, &a, &b));
                }
            }
        }
    }
    #[test]
    fn reason_codes() {
        for (r, s) in [
            (Reason::Allowlisted, "allowlisted"),
            (Reason::NotListed, "not_listed"),
            (Reason::Broken, "broken"),
            (Reason::PrivateField, "private_field"),
            (Reason::Terminal, "terminal"),
            (Reason::Unidentified, "unidentified"),
            (Reason::NoReplace, "no_replace"),
        ] {
            assert_eq!(r.code(), s);
        }
    }
    #[test]
    fn learnable_rejects_secret_shapes() {
        for t in ["hunter2", "sk-abc123", "KEY=x", "GitHub", "ab"] {
            assert!(!is_learnable(t, Mode::Silent), "{t}");
        }
        assert!(is_learnable("gstack", Mode::Silent));
        assert!(is_learnable("Gstack", Mode::Silent));
        assert!(!is_learnable("gstack", Mode::Off));
    }
    #[test]
    fn learnable_vectors_file() {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../data/learnable-vectors.tsv");
        let s = std::fs::read_to_string(p).unwrap();
        let mut n = 0;
        for line in s.lines() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let (tok, flag) = line.split_once('\t').unwrap();
            assert_eq!(is_learnable(tok, Mode::Silent), flag == "1", "{tok}");
            n += 1;
        }
        assert!(n >= 12);
    }
    #[test]
    fn terminal_never_learns() {
        assert!(!is_learnable("building", Mode::ProseAware));
    }
    #[test]
    fn terminal_unidentified_is_off() {
        assert_eq!(resolve_mode(&ctx(10, 0, None), &al(&["x"]), &[]), Mode::Off);
    }
    #[test]
    fn terminal_not_allowlisted_is_off() {
        let c = ctx(10, 0, Some("org.gnome.Ptyxis"));
        assert_eq!(resolve_mode(&c, &al(&["firefox"]), &[]), Mode::Off);
    }
    #[test]
    fn terminal_by_wm_class() {
        let c = ctx(0, 0, Some("org.gnome.Ptyxis"));
        assert_eq!(resolve_mode(&c, &al(&["firefox"]), &[]), Mode::Off);
        assert_eq!(
            resolve_mode(&c, &al(&["org.gnome.ptyxis"]), &[]),
            Mode::ProseAware
        );
    }
    #[test]
    fn third_party_terminals_by_wm_class() {
        // As the apps report them; matching ignores case.
        for wm in [
            "Alacritty",
            "kitty",
            "foot",
            "footclient",
            "org.wezfurlong.wezterm",
            "xterm",
            "XTerm",
            "com.mitchellh.ghostty",
            "terminator",
            "Terminator",
            "com.gexperts.Tilix",
            "org.kde.konsole",
        ] {
            let c = ctx(0, 0, Some(wm));
            assert_eq!(resolve_mode(&c, &al(&["firefox"]), &[]), Mode::Off, "{wm}");
            assert_eq!(resolve_mode(&c, &al(&[wm]), &[]), Mode::ProseAware, "{wm}");
        }
    }
    #[test]
    fn no_terminal_bigrams() {
        assert!(!bigram_learnable(Mode::ProseAware));
        assert!(bigram_learnable(Mode::Silent));
        assert!(bigram_learnable(Mode::Observe));
        assert!(!bigram_learnable(Mode::Off));
    }
    #[test]
    fn unidentified_app_observes() {
        assert_eq!(
            resolve_mode(&ctx(0, 0, None), &al(&["firefox"]), &[]),
            Mode::Observe
        );
    }
    #[test]
    fn terminal_purpose_prose_aware() {
        let c = ctx(10, 0, Some("foot"));
        assert_eq!(resolve_mode(&c, &al(&["foot"]), &[]), Mode::ProseAware);
    }
    #[test]
    fn broken_app_observes() {
        let c = ctx(0, 0, Some("firefox"));
        assert_eq!(
            resolve_mode(&c, &al(&["firefox"]), &al(&["firefox"])),
            Mode::Observe
        );
        assert_eq!(resolve_mode(&c, &[], &[]), Mode::Observe);
        assert_eq!(resolve_mode(&c, &al(&["firefox"]), &[]), Mode::Silent);
    }
    #[test]
    fn lang_window_majority() {
        let mut w = LangWindow::new(2);
        assert_eq!(w.dominant(), None);
        w.push(TL);
        w.push(TL);
        w.push(EN);
        assert_eq!(w.dominant(), Some(TL));
        let c = w.ctx();
        assert_eq!(c.allowed, LangSet(0b10));
        assert_eq!(c.margin, 0.3);
        w.reset();
        w.push(EN);
        w.push(TL);
        assert_eq!(w.dominant(), None);
        let c = w.ctx();
        assert_eq!(c.allowed, LangSet::all(2));
        assert_eq!(c.margin, 0.6);
        // Eviction: the 5th push drops the oldest, so the window is TL,TL,EN,EN
        // (tie => None). Without eviction EN would have 3 votes and win.
        w.reset();
        for l in [EN, TL, TL, EN, EN] {
            w.push(l);
        }
        assert_eq!(w.dominant(), None);
        let c = w.ctx();
        assert_eq!(c.allowed, LangSet::all(2));
        assert_eq!(c.margin, 0.6);
    }
    #[test]
    fn mode_order_and_hint_bits() {
        assert_eq!(HINT_NO_SPELLCHECK, 2);
        assert_eq!(HINT_PRIVATE, 2048);
        let pty = "org.gnome.ptyxis";
        let a = al(&[pty, "firefox"]);
        assert_eq!(resolve_mode(&ctx(8, 0, Some(pty)), &a, &[]), Mode::Off);
        assert_eq!(
            resolve_mode(&ctx(10, HINT_PRIVATE | 1, Some(pty)), &a, &[]),
            Mode::Off
        );
        assert_eq!(
            resolve_mode(&ctx(0, HINT_PRIVATE | 1, Some("firefox")), &a, &[]),
            Mode::Off
        );
    }
    #[test]
    fn osk_overrides_layout() {
        assert_eq!(effective_layout(LayoutId::Colemak, true), LayoutId::OskTap);
        assert_eq!(
            effective_layout(LayoutId::Colemak, false),
            LayoutId::Colemak
        );
    }
    #[test]
    fn prose_run_excludes_candidate() {
        let mut p = ProseRun::default();
        assert!(!p.ready());
        p.on_word_completed("building");
        assert!(!p.ready());
        p.on_word_completed("Another");
        assert!(p.ready());
        p.reset();
        assert!(!p.ready());
    }
    #[test]
    fn prose_run_rejects_command_priming() {
        let mut p = ProseRun::default();
        p.on_word_completed("commit");
        p.on_word_completed("them");
        assert!(p.ready());
        p.on_word_completed("git");
        p.on_word_completed("-m");
        assert!(!p.ready());
        p.on_word_completed("them");
        assert!(!p.ready());
    }
    #[test]
    fn command_tokens_rejected() {
        for t in [
            "git",
            "rm",
            "--force",
            "a/b",
            "myVar",
            "ab_cd",
            "HTTP",
            "caf\u{e9}s",
        ] {
            assert!(!terminal_token_ok(t), "{t}");
        }
        assert!(terminal_token_ok("building"));
        assert!(terminal_token_ok("Building"));
    }
}
