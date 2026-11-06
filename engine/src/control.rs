//! Control plane: `org.nadelacruz.Correctiond1` on the session bus.
//!
//! The GNOME Shell extension (Task 11) tells the engine which app has focus
//! and whether the on-screen keyboard is up, and toggles correction. The
//! engine instances read that state on every key through `ControlState`.
//!
//! Every method except `GetStats` and `ImportVocabulary` is accepted only
//! from the connection that owns `org.gnome.Shell` (cached, refreshed on
//! `NameOwnerChanged`), unless `allow_any_caller` is set in config.toml.
//! `ListWords` and `ForgetWords` are Shell-only even with `allow_any_caller`
//! (R9): the Shell passes them through only for a Settings window the user
//! allowed. `ImportVocabulary` is open to any caller on the session bus
//! (which only the same user can reach), so `engine import FILE` works from
//! a terminal. Replies and errors carry counts, booleans, fixed codes and
//! fixed texts only: never file content, paths or the app name, and words
//! only in the Shell-only `RecentWords` and `ListWords`.

use crate::config::Config;
use crate::ibus::Shared;
use crate::persist::{
    PersistStats, FORGET_FAILED, FORGET_LOCKED, IMPORT_DUPLICATE, IMPORT_STOPPED,
    IMPORT_UNAVAILABLE, IMPORT_WRITE_FAILED,
};
use corelib::learning::SharedStore;
use corelib::policy::{resolve_mode, resolve_mode_reason, Ctx, Mode, Reason};
use corelib::vocab::{read_vocab, VocabBatch, VocabError, MAX_BYTES};
use engine::Counters;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use zbus::message::Header;
use zbus::names::ErrorName;
use zbus::{fdo, interface, DBusError, Message};

pub const CONTROL_NAME: &str = "org.nadelacruz.Correctiond1";
pub const CONTROL_PATH: &str = "/org/nadelacruz/Correctiond";
const SHELL_NAME: &str = "org.gnome.Shell";

/// A longer class is not a real WM_CLASS; it is treated as unidentified.
const MAX_WM_CLASS: usize = 256;

const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";
const PURGE_PARTIAL: &str = "org.nadelacruz.Correctiond1.Error.PurgePartial";
const IMPORT_FAILED: &str = "org.nadelacruz.Correctiond1.Error.ImportFailed";
const FORGET_ERROR: &str = "org.nadelacruz.Correctiond1.Error.ForgetFailed";
const NOT_SHELL: &str = "only GNOME Shell may call this method";

pub const IMPORT_NOT_VOCAB: &str = "not a correctiond vocab v1 file";
pub const IMPORT_TOO_LARGE: &str = "file too large (over 16 MiB or 200,000 records)";
pub const IMPORT_UNREADABLE: &str = "file could not be read";
const IMPORT_OTHER: &str = "import failed";

/// Every description an `ImportFailed` error can carry. The client prints
/// only these (anything else becomes "import failed").
pub const IMPORT_ERRORS: &[&str] = &[
    IMPORT_UNAVAILABLE,
    IMPORT_DUPLICATE,
    IMPORT_NOT_VOCAB,
    IMPORT_TOO_LARGE,
    IMPORT_UNREADABLE,
    IMPORT_WRITE_FAILED,
    IMPORT_STOPPED,
    IMPORT_OTHER,
];

/// A known fixed text, or the generic one.
pub fn import_error_text(e: &str) -> &'static str {
    IMPORT_ERRORS
        .iter()
        .find(|t| **t == e)
        .copied()
        .unwrap_or(IMPORT_OTHER)
}

/// A known `ForgetWord` failure text, or the generic one (also for the
/// actor having stopped).
pub fn forget_error_text(e: &str) -> &'static str {
    if e == FORGET_LOCKED {
        FORGET_LOCKED
    } else {
        FORGET_FAILED
    }
}

/// Longest word `ForgetWord` looks up; anything longer was never learned.
const MAX_FORGET_BYTES: usize = 64;

/// Most words one `ForgetWords` call takes.
const MAX_FORGET_WORDS: usize = 10_000;

/// A `ListWords` row on the wire, `(sutb)`: (word, count, last_seen,
/// personal). The store's u64 count saturates and a negative last_seen
/// (never written) reads as 0.
fn wire_row((w, count, seen, personal): (String, u64, i64, bool)) -> (String, u32, u64, bool) {
    (
        w,
        u32::try_from(count).unwrap_or(u32::MAX),
        u64::try_from(seen).unwrap_or(0),
        personal,
    )
}

/// The effective mode on a client that cannot apply a replacement: never
/// corrected. Silent becomes Observe (same learning, no Replace), and
/// ProseAware becomes Off, because terminals never learn and Observe would.
pub fn without_replace(mode: Mode) -> Mode {
    match mode {
        Mode::Silent => Mode::Observe,
        Mode::ProseAware => Mode::Off,
        m => m,
    }
}

/// The `app_mode` code of `GetStatus`.
fn mode_code(mode: Mode) -> &'static str {
    match mode {
        Mode::Silent | Mode::ProseAware => "correct",
        Mode::Observe => "learn_only",
        Mode::Off => "off",
    }
}

/// What the control plane has told us, plus the config it is judged by.
pub struct ControlState {
    allowlist: Vec<String>,
    broken: Vec<String>,
    allow_any_caller: bool,
    /// The focused app's WM_CLASS; `None` = unidentified (observe only).
    focused: Mutex<Option<String>>,
    osk_visible: AtomicBool,
    enabled: AtomicBool,
    /// SetLearning: memory only, on at start.
    learning: AtomicBool,
    /// The last IBus field: (purpose, hints, can_replace). Kept on focus
    /// out (Quick Settings takes focus); `None` until a field focuses.
    field: Mutex<Option<(u32, u32, bool)>>,
}

impl ControlState {
    /// Starts unidentified, OSK hidden, enabled.
    pub fn new(config: &Config) -> ControlState {
        ControlState {
            allowlist: config.allowlist.clone(),
            broken: config.broken.clone(),
            allow_any_caller: config.allow_any_caller,
            focused: Mutex::new(None),
            osk_visible: AtomicBool::new(false),
            enabled: AtomicBool::new(true),
            learning: AtomicBool::new(true),
            field: Mutex::new(None),
        }
    }

    /// Empty (no focus window) or oversized ⇒ unidentified.
    pub fn set_focused_app(&self, wm_class: &str) {
        let app =
            (!wm_class.is_empty() && wm_class.len() <= MAX_WM_CLASS).then(|| wm_class.to_owned());
        *self.focused.lock().unwrap_or_else(|e| e.into_inner()) = app;
    }

    pub fn set_osk_visible(&self, visible: bool) {
        self.osk_visible.store(visible, Ordering::Relaxed);
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn set_learning(&self, on: bool) {
        self.learning.store(on, Ordering::Relaxed);
    }

    pub fn learning(&self) -> bool {
        self.learning.load(Ordering::Relaxed)
    }

    /// Recorded by the IBus engine at FocusIn, SetContentType and
    /// SetCapabilities.
    pub fn set_field(&self, purpose: u32, hints: u32, can_replace: bool) {
        *self.field.lock().unwrap_or_else(|e| e.into_inner()) = Some((purpose, hints, can_replace));
    }

    pub fn osk_visible(&self) -> bool {
        self.osk_visible.load(Ordering::Relaxed)
    }

    /// The policy context for a field with this content type.
    pub fn ctx(&self, purpose: u32, hints: u32) -> Ctx {
        Ctx {
            purpose,
            hints,
            wm_class: self
                .focused
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            osk_visible: self.osk_visible(),
        }
    }

    /// Disabled from the indicator ⇒ Off (no correction, no learning).
    pub fn mode(&self, purpose: u32, hints: u32) -> Mode {
        if !self.enabled.load(Ordering::Relaxed) {
            return Mode::Off;
        }
        resolve_mode(&self.ctx(purpose, hints), &self.allowlist, &self.broken)
    }

    /// The last field's mode and why, judged now (with the app focused
    /// now), with the IBus engine's can't-replace downgrade. `enabled` is
    /// not applied: `status_json` reports it on its own. `None` until a
    /// field has focused.
    pub fn app_state(&self) -> Option<(Mode, Reason)> {
        let (purpose, hints, can_replace) =
            (*self.field.lock().unwrap_or_else(|e| e.into_inner()))?;
        let (mode, reason) =
            resolve_mode_reason(&self.ctx(purpose, hints), &self.allowlist, &self.broken);
        let effective = if can_replace {
            mode
        } else {
            without_replace(mode)
        };
        Some(if effective == mode {
            (mode, reason)
        } else {
            (effective, Reason::NoReplace)
        })
    }

    /// `GetStatus` JSON: fixed codes and booleans only, never the app.
    pub fn status_json(&self) -> String {
        let (mode, reason) = self
            .app_state()
            .map_or(("none", ""), |(m, r)| (mode_code(m), r.code()));
        format!(
            "{{\"app_mode\":\"{mode}\",\"app_reason\":\"{reason}\",\
             \"enabled\":{},\"learning\":{}}}",
            self.enabled.load(Ordering::Relaxed),
            self.learning(),
        )
    }
}

/// The control-plane methods, for the caller check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    SetFocusedApp,
    SetOskVisible,
    SetEnabled,
    Purge,
    GetStats,
    ImportVocabulary,
    GetStatus,
    SetLearning,
    RecentWords,
    ForgetWord,
    ListWords,
    ForgetWords,
}

impl Method {
    /// Methods only GNOME Shell may call.
    fn shell_only(self) -> bool {
        !matches!(self, Method::GetStats | Method::ImportVocabulary)
    }

    /// Methods that list or bulk-forget learned words: Shell-only even
    /// with `allow_any_caller` (R9).
    fn word_reading(self) -> bool {
        matches!(self, Method::ListWords | Method::ForgetWords)
    }
}

/// Errors of `org.nadelacruz.Correctiond1`, with fixed descriptions.
#[derive(Debug)]
pub enum ControlError {
    /// `org.freedesktop.DBus.Error.AccessDenied`.
    AccessDenied,
    /// The forget finished only in part; the text says what is pending.
    PurgePartial(String),
    /// `ImportVocabulary` failed; always one of `IMPORT_ERRORS`.
    ImportFailed(&'static str),
    /// `ForgetWord` or `ForgetWords` failed: `FORGET_LOCKED` or
    /// `FORGET_FAILED`.
    ForgetFailed(&'static str),
}

impl DBusError for ControlError {
    fn create_reply(&self, call: &Header<'_>) -> zbus::Result<Message> {
        Message::error(call, self.name())?.build(&(self.description().unwrap_or_default(),))
    }

    fn name(&self) -> ErrorName<'_> {
        ErrorName::from_static_str_unchecked(match self {
            ControlError::AccessDenied => ACCESS_DENIED,
            ControlError::PurgePartial(_) => PURGE_PARTIAL,
            ControlError::ImportFailed(_) => IMPORT_FAILED,
            ControlError::ForgetFailed(_) => FORGET_ERROR,
        })
    }

    fn description(&self) -> Option<&str> {
        Some(match self {
            ControlError::AccessDenied => NOT_SHELL,
            ControlError::PurgePartial(m) => m,
            ControlError::ImportFailed(m) => m,
            ControlError::ForgetFailed(m) => m,
        })
    }
}

/// The caller check, without D-Bus: `sender` is the call's unique name,
/// `shell_owner` the current owner of `org.gnome.Shell`.
pub fn authorize(
    method: Method,
    sender: Option<&str>,
    shell_owner: Option<&str>,
    allow_any_caller: bool,
) -> Result<(), ControlError> {
    if !method.shell_only() || (allow_any_caller && !method.word_reading()) {
        return Ok(());
    }
    match (sender, shell_owner) {
        (Some(s), Some(o)) if s == o => Ok(()),
        _ => Err(ControlError::AccessDenied),
    }
}

/// `GetStats` JSON: counts and booleans only, so no word can appear.
pub fn stats_json(counters: &Counters, store: &SharedStore, persist: PersistStats) -> String {
    let words = store
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .learned_word_count();
    format!(
        "{{\"corrections\":{},\"reverts\":{},\"words_learned\":{},\
         \"persistent\":{},\"quarantined_files\":{},\"key_delete_pending\":{}}}",
        counters.corrections.load(Ordering::Relaxed),
        counters.reverts.load(Ordering::Relaxed),
        words,
        persist.persistent,
        persist.quarantined_files,
        persist.key_delete_pending,
    )
}

/// Read a passed vocabulary file: a regular file only (a pipe could block
/// the reading thread forever), from offset 0 whatever the sender's file
/// position, at most `MAX_BYTES`. Blocking: call it in `spawn_blocking`.
fn read_import_fd(fd: zbus::zvariant::OwnedFd) -> Result<(VocabBatch, [u8; 32]), ControlError> {
    let unreadable = || ControlError::ImportFailed(IMPORT_UNREADABLE);
    let too_large = || ControlError::ImportFailed(IMPORT_TOO_LARGE);
    let file = std::fs::File::from(std::os::fd::OwnedFd::from(fd));
    let meta = file.metadata().map_err(|_| unreadable())?;
    if !meta.is_file() {
        return Err(unreadable());
    }
    if meta.len() > MAX_BYTES as u64 {
        return Err(too_large());
    }
    // One byte past the limit tells a file that grew since the stat.
    let limit = MAX_BYTES + 1;
    let mut buf = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0).min(limit));
    let mut chunk = vec![0u8; 64 * 1024];
    while buf.len() < limit {
        let want = chunk.len().min(limit - buf.len());
        match file.read_at(&mut chunk[..want], buf.len() as u64) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(unreadable()),
        }
    }
    match read_vocab(&buf) {
        Ok((batch, _stats, sha)) => Ok((batch, sha)),
        Err(VocabError::BadHeader) => Err(ControlError::ImportFailed(IMPORT_NOT_VOCAB)),
        Err(VocabError::TooLarge) => Err(too_large()),
    }
}

/// The current owner of `org.gnome.Shell`, as the bus last told us.
type ShellOwner = Arc<Mutex<Option<String>>>;

fn set_owner(owner: &ShellOwner, value: Option<String>) {
    *owner.lock().unwrap_or_else(|e| e.into_inner()) = value;
}

/// The object at `/org/nadelacruz/Correctiond`.
pub struct Control {
    shared: Arc<Shared>,
    shell: ShellOwner,
    /// One `ImportVocabulary` at a time: a second concurrent call waits, so
    /// the engine never holds more than one file and batch in memory.
    import_gate: tokio::sync::Semaphore,
}

impl Control {
    fn check(&self, method: Method, hdr: &Header<'_>) -> Result<(), ControlError> {
        let owner = self.shell.lock().unwrap_or_else(|e| e.into_inner()).clone();
        authorize(
            method,
            hdr.sender().map(|s| s.as_str()),
            owner.as_deref(),
            self.shared.control.allow_any_caller,
        )
    }
}

#[interface(name = "org.nadelacruz.Correctiond1")]
impl Control {
    /// The focused window's WM_CLASS; empty ⇒ unidentified.
    fn set_focused_app(
        &self,
        wm_class: &str,
        #[zbus(header)] hdr: Header<'_>,
    ) -> Result<(), ControlError> {
        self.check(Method::SetFocusedApp, &hdr)?;
        self.shared.control.set_focused_app(wm_class);
        Ok(())
    }

    fn set_osk_visible(
        &self,
        visible: bool,
        #[zbus(header)] hdr: Header<'_>,
    ) -> Result<(), ControlError> {
        self.check(Method::SetOskVisible, &hdr)?;
        self.shared.control.set_osk_visible(visible);
        Ok(())
    }

    fn set_enabled(
        &self,
        enabled: bool,
        #[zbus(header)] hdr: Header<'_>,
    ) -> Result<(), ControlError> {
        self.check(Method::SetEnabled, &hdr)?;
        self.shared.control.set_enabled(enabled);
        Ok(())
    }

    /// Forget everything learned. All of it runs inside the persistence
    /// actor, serialized with attach and flush.
    async fn purge(&self, #[zbus(header)] hdr: Header<'_>) -> Result<(), ControlError> {
        self.check(Method::Purge, &hdr)?;
        match &self.shared.persist {
            Some(p) => p.purge().await.map_err(ControlError::PurgePartial),
            None => {
                // No actor (tests): the store is memory-only anyway.
                self.shared
                    .store
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
                Ok(())
            }
        }
    }

    /// Import a "correctiond vocab v1" file (docs/vocab-format.md) passed
    /// as an fd. Open to any caller. Reading, hashing and parsing run in
    /// `spawn_blocking`; the counts land in the store through the
    /// persistence actor. One call at a time (`import_gate`). Replies
    /// (words, word pairs) imported.
    #[zbus(out_args("words", "bigrams"))]
    async fn import_vocabulary(
        &self,
        fd: zbus::zvariant::OwnedFd,
        #[zbus(header)] hdr: Header<'_>,
    ) -> Result<(u32, u32), ControlError> {
        self.check(Method::ImportVocabulary, &hdr)?;
        // Never closed, so acquiring cannot fail.
        let _permit = self
            .import_gate
            .acquire()
            .await
            .map_err(|_| ControlError::ImportFailed(IMPORT_UNREADABLE))?;
        let (batch, sha) = tokio::task::spawn_blocking(move || read_import_fd(fd))
            .await
            .map_err(|_| ControlError::ImportFailed(IMPORT_UNREADABLE))??;
        match &self.shared.persist {
            Some(p) => p
                .import(batch, sha)
                .await
                .map_err(|e| ControlError::ImportFailed(import_error_text(&e))),
            // No actor (tests): never persistent.
            None => Err(ControlError::ImportFailed(IMPORT_UNAVAILABLE)),
        }
    }

    /// Open to any caller: counts and booleans only.
    fn get_stats(&self, #[zbus(header)] hdr: Header<'_>) -> Result<String, ControlError> {
        self.check(Method::GetStats, &hdr)?;
        let persist = self
            .shared
            .persist
            .as_ref()
            .map(|p| p.stats())
            .unwrap_or_default();
        Ok(stats_json(
            &self.shared.counters,
            &self.shared.store,
            persist,
        ))
    }

    /// The focused field's app state as JSON (`status_json`): codes and
    /// booleans, never the app's name.
    fn get_status(&self, #[zbus(header)] hdr: Header<'_>) -> Result<String, ControlError> {
        self.check(Method::GetStatus, &hdr)?;
        Ok(self.shared.control.status_json())
    }

    /// Pause (false) or resume learning from typing. Memory only.
    fn set_learning(&self, on: bool, #[zbus(header)] hdr: Header<'_>) -> Result<(), ControlError> {
        self.check(Method::SetLearning, &hdr)?;
        self.shared.control.set_learning(on);
        Ok(())
    }

    /// Words recently learned from typing, newest first (at most 5).
    fn recent_words(&self, #[zbus(header)] hdr: Header<'_>) -> Result<Vec<String>, ControlError> {
        self.check(Method::RecentWords, &hdr)?;
        Ok(self
            .shared
            .store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recent_words())
    }

    /// Forget one word everywhere (memory and disk). True if it was
    /// learned; false if not (so a second click is not an error), or if it
    /// is empty or over 64 bytes. Runs in the persistence actor.
    async fn forget_word(
        &self,
        word: String,
        #[zbus(header)] hdr: Header<'_>,
    ) -> Result<bool, ControlError> {
        self.check(Method::ForgetWord, &hdr)?;
        if word.is_empty() || word.len() > MAX_FORGET_BYTES {
            return Ok(false);
        }
        match &self.shared.persist {
            Some(p) => p
                .forget_word(word)
                .await
                .map_err(|e| ControlError::ForgetFailed(forget_error_text(&e))),
            // No actor (tests): the store is memory-only anyway.
            None => Ok(self
                .shared
                .store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .forget_word(&word)),
        }
    }

    /// Every stored word (count >= 2) as (word, count, last_seen,
    /// personal), unsorted; `personal` = in no loaded dictionary. One
    /// store lock. Shell-only even with `allow_any_caller`.
    fn list_words(
        &self,
        #[zbus(header)] hdr: Header<'_>,
    ) -> Result<Vec<(String, u32, u64, bool)>, ControlError> {
        self.check(Method::ListWords, &hdr)?;
        let dict = &self.shared.dict;
        let rows = self
            .shared
            .store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .list_words(&|w| dict.contains(w));
        Ok(rows.into_iter().map(wire_row).collect())
    }

    /// Forget 1 to 10 000 words (each 1 to 64 bytes) everywhere, in one
    /// persistence request and one transaction. Replies how many existed.
    /// A bad batch is refused whole (`FORGET_FAILED`). Shell-only even
    /// with `allow_any_caller`.
    async fn forget_words(
        &self,
        words: Vec<String>,
        #[zbus(header)] hdr: Header<'_>,
    ) -> Result<u32, ControlError> {
        self.check(Method::ForgetWords, &hdr)?;
        let bad = |w: &String| w.is_empty() || w.len() > MAX_FORGET_BYTES;
        if words.is_empty() || words.len() > MAX_FORGET_WORDS || words.iter().any(bad) {
            return Err(ControlError::ForgetFailed(FORGET_FAILED));
        }
        match &self.shared.persist {
            Some(p) => p
                .forget_words(words)
                .await
                .map_err(|e| ControlError::ForgetFailed(forget_error_text(&e))),
            // No actor (tests): the store is memory-only anyway.
            None => Ok(self
                .shared
                .store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .forget_words(&words)),
        }
    }
}

/// Serves the control plane until the session bus goes away. Failures are
/// logged (fixed text) and never affect typing: without it the engine
/// stays unidentified, which means observe-only.
pub async fn serve(shared: Arc<Shared>) {
    if let Err(e) = run(shared).await {
        eprintln!("correctiond: control plane unavailable: {}", error_kind(&e));
    }
}

/// A fixed description: zbus error texts can quote message contents.
fn error_kind(e: &zbus::Error) -> &'static str {
    match e {
        zbus::Error::Address(_) => "no session bus address",
        zbus::Error::InputOutput(_) => "session bus connection failed",
        zbus::Error::NameTaken => "name already owned",
        zbus::Error::MethodError(..) | zbus::Error::FDO(_) => "bus call failed",
        _ => "session bus error",
    }
}

async fn run(shared: Arc<Shared>) -> zbus::Result<()> {
    let conn = zbus::connection::Builder::session()?.build().await?;
    let dbus = fdo::DBusProxy::new(&conn).await?;
    // Subscribe before asking: a change in between is then not lost.
    let changes = dbus
        .receive_name_owner_changed_with_args(&[(0, SHELL_NAME)])
        .await?;
    let shell: ShellOwner = Arc::new(Mutex::new(None));
    let shell_name = zbus::names::BusName::try_from(SHELL_NAME)?;
    // NameHasNoOwner (no shell, e.g. headless) leaves it unset.
    if let Ok(o) = dbus.get_name_owner(shell_name).await {
        set_owner(&shell, Some(o.to_string()));
    }
    let control = Control {
        shared,
        shell: shell.clone(),
        import_gate: tokio::sync::Semaphore::new(1),
    };
    conn.object_server().at(CONTROL_PATH, control).await?;
    conn.request_name(CONTROL_NAME).await?;

    let mut changes = std::pin::pin!(changes);
    while let Some(sig) = std::future::poll_fn(|cx| {
        zbus::export::futures_core::Stream::poll_next(changes.as_mut(), cx)
    })
    .await
    {
        if let Ok(args) = sig.args() {
            let owner = args.new_owner().as_ref().map(|o| o.to_string());
            set_owner(&shell, owner);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::persist::PersistStats;
    use corelib::learning::LearnStore;
    use corelib::policy::Mode;
    use corelib::scoring::PersonalLookup;
    use engine::Counters;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use zbus::DBusError;

    const FREE_FORM: u32 = 0;
    const PASSWORD: u32 = 8;

    fn state() -> ControlState {
        ControlState::new(&Config::default())
    }

    #[test]
    fn firefox_allowlisted_silent() {
        let c = state();
        c.set_focused_app("firefox");
        assert_eq!(c.mode(FREE_FORM, 0), Mode::Silent);
        // The shell reports the class as-is; the allowlist match ignores case.
        c.set_focused_app("Firefox");
        assert_eq!(c.mode(FREE_FORM, 0), Mode::Silent);
        assert_eq!(c.ctx(FREE_FORM, 0).wm_class.as_deref(), Some("Firefox"));
    }

    #[test]
    fn password_in_firefox_off() {
        let c = state();
        c.set_focused_app("firefox");
        assert_eq!(c.mode(PASSWORD, 0), Mode::Off);
        // The hard-off hints too.
        assert_eq!(c.mode(FREE_FORM, 1 << 1), Mode::Off); // NO_SPELLCHECK
        assert_eq!(c.mode(FREE_FORM, 1 << 11), Mode::Off); // PRIVATE
    }

    #[test]
    fn no_wmclass_observes() {
        let c = state();
        assert_eq!(c.ctx(FREE_FORM, 0).wm_class, None);
        assert_eq!(c.mode(FREE_FORM, 0), Mode::Observe);
        // An empty class (no focus window) is unidentified, not an app.
        c.set_focused_app("firefox");
        c.set_focused_app("");
        assert_eq!(c.ctx(FREE_FORM, 0).wm_class, None);
        assert_eq!(c.mode(FREE_FORM, 0), Mode::Observe);
        // An app that is not allowlisted only observes as well.
        c.set_focused_app("org.gnome.TextEditor");
        assert_eq!(c.mode(FREE_FORM, 0), Mode::Observe);
    }

    #[test]
    fn disabled_is_off_and_osk_reaches_ctx() {
        let c = state();
        c.set_focused_app("firefox");
        c.set_enabled(false);
        assert_eq!(c.mode(FREE_FORM, 0), Mode::Off);
        c.set_enabled(true);
        assert_eq!(c.mode(FREE_FORM, 0), Mode::Silent);
        assert!(!c.osk_visible());
        c.set_osk_visible(true);
        assert!(c.osk_visible());
        assert!(c.ctx(FREE_FORM, 0).osk_visible);
    }

    #[test]
    fn non_shell_caller_denied() {
        let shell = Some(":1.12");
        for m in [
            Method::SetFocusedApp,
            Method::SetOskVisible,
            Method::SetEnabled,
            Method::Purge,
            Method::GetStatus,
            Method::SetLearning,
            Method::RecentWords,
            Method::ForgetWord,
        ] {
            let e = authorize(m, Some(":1.99"), shell, false).unwrap_err();
            assert_eq!(e.name().as_str(), "org.freedesktop.DBus.Error.AccessDenied");
            assert!(authorize(m, Some(":1.12"), shell, false).is_ok(), "{m:?}");
            assert!(authorize(m, Some(":1.99"), shell, true).is_ok(), "{m:?}");
            // No shell on the bus, or no sender: nobody qualifies.
            assert!(authorize(m, Some(":1.12"), None, false).is_err(), "{m:?}");
            assert!(authorize(m, None, shell, false).is_err(), "{m:?}");
        }
        // GetStats is open to any caller.
        assert!(authorize(Method::GetStats, Some(":1.99"), shell, false).is_ok());
        assert!(authorize(Method::GetStats, None, None, false).is_ok());
    }

    /// R9: the methods that read or forget learned words are Shell-only
    /// even when `allow_any_caller` is set; the other Shell-only methods
    /// still honor it.
    #[test]
    fn word_methods_ignore_allow_any_caller() {
        let shell = Some(":1.12");
        for m in [Method::ListWords, Method::ForgetWords] {
            let e = authorize(m, Some(":1.99"), shell, false).unwrap_err();
            assert_eq!(e.name().as_str(), "org.freedesktop.DBus.Error.AccessDenied");
            assert!(authorize(m, Some(":1.12"), shell, false).is_ok(), "{m:?}");
            assert!(authorize(m, Some(":1.12"), shell, true).is_ok(), "{m:?}");
            assert!(authorize(m, Some(":1.99"), shell, true).is_err(), "{m:?}");
            assert!(authorize(m, None, shell, true).is_err(), "{m:?}");
            assert!(authorize(m, Some(":1.12"), None, true).is_err(), "{m:?}");
        }
        for m in [
            Method::SetFocusedApp,
            Method::SetOskVisible,
            Method::SetEnabled,
            Method::Purge,
            Method::GetStatus,
            Method::SetLearning,
            Method::RecentWords,
            Method::ForgetWord,
        ] {
            assert!(authorize(m, Some(":1.99"), shell, true).is_ok(), "{m:?}");
            assert!(authorize(m, None, None, true).is_ok(), "{m:?}");
        }
    }

    #[test]
    fn import_allowed_from_any_caller() {
        let shell = Some(":1.12");
        let m = Method::ImportVocabulary;
        assert!(authorize(m, Some(":1.99"), shell, false).is_ok());
        assert!(authorize(m, Some(":1.12"), shell, false).is_ok());
        assert!(authorize(m, Some(":1.99"), None, false).is_ok());
    }

    #[test]
    fn import_error_name() {
        let e = ControlError::ImportFailed("x");
        assert_eq!(
            e.name().as_str(),
            "org.nadelacruz.Correctiond1.Error.ImportFailed"
        );
        assert_eq!(e.description(), Some("x"));
    }

    #[test]
    fn purge_partial_error_name() {
        let e = ControlError::PurgePartial("x".into());
        assert_eq!(
            e.name().as_str(),
            "org.nadelacruz.Correctiond1.Error.PurgePartial"
        );
        assert_eq!(e.description(), Some("x"));
    }

    #[test]
    fn forget_error_name_and_texts() {
        let e = ControlError::ForgetFailed(FORGET_LOCKED);
        assert_eq!(
            e.name().as_str(),
            "org.nadelacruz.Correctiond1.Error.ForgetFailed"
        );
        assert_eq!(e.description(), Some("unlock your keyring first"));
        assert_eq!(
            forget_error_text(FORGET_LOCKED),
            "unlock your keyring first"
        );
        assert_eq!(forget_error_text(FORGET_FAILED), "could not forget");
        // The actor stopping, or anything unexpected, is the generic text.
        assert_eq!(forget_error_text("persistence stopped"), "could not forget");
        assert_eq!(forget_error_text("kadto"), "could not forget");
    }

    #[test]
    fn learning_flag_starts_on() {
        let c = state();
        assert!(c.learning());
        c.set_learning(false);
        assert!(!c.learning());
        c.set_learning(true);
        assert!(c.learning());
    }

    /// R2: the app state is computed when read, from the last field and
    /// the app focused now.
    #[test]
    fn app_state_is_computed_on_read() {
        let c = state();
        c.set_focused_app("firefox");
        c.set_field(FREE_FORM, 0, true);
        assert_eq!(c.app_state(), Some((Mode::Silent, Reason::Allowlisted)));
        // Switching windows without typing gives the new app's state.
        c.set_focused_app("org.gnome.Nautilus");
        assert_eq!(c.app_state(), Some((Mode::Observe, Reason::NotListed)));
        // Disabled does not change it: `enabled` is reported on its own.
        c.set_enabled(false);
        assert_eq!(c.app_state(), Some((Mode::Observe, Reason::NotListed)));
    }

    /// R1: the same downgrade as the IBus engine's `mode()`.
    #[test]
    fn no_replace_downgrade() {
        let c = ControlState::new(&Config {
            allowlist: vec!["firefox".into(), "kitty".into()],
            ..Config::default()
        });
        c.set_focused_app("firefox");
        c.set_field(FREE_FORM, 0, false);
        assert_eq!(c.app_state(), Some((Mode::Observe, Reason::NoReplace)));
        c.set_focused_app("kitty");
        assert_eq!(c.app_state(), Some((Mode::Off, Reason::NoReplace)));
        c.set_field(FREE_FORM, 0, true);
        assert_eq!(c.app_state(), Some((Mode::ProseAware, Reason::Allowlisted)));
        // Only a correcting mode is downgraded.
        c.set_focused_app("org.gnome.Nautilus");
        c.set_field(PASSWORD, 0, false);
        assert_eq!(c.app_state(), Some((Mode::Off, Reason::PrivateField)));
        c.set_field(FREE_FORM, 0, false);
        assert_eq!(c.app_state(), Some((Mode::Observe, Reason::NotListed)));
    }

    #[test]
    fn none_before_any_field() {
        let c = state();
        c.set_focused_app("firefox");
        assert_eq!(c.app_state(), None);
        let json = c.status_json();
        assert!(
            json.contains("\"app_mode\":\"none\",\"app_reason\":\"\""),
            "{json}"
        );
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["enabled"], true);
        assert_eq!(v["learning"], true);
        assert_eq!(v.as_object().unwrap().len(), 4);
    }

    #[test]
    fn status_json_reports_flags_and_codes() {
        let c = state();
        c.set_focused_app("firefox");
        c.set_field(FREE_FORM, 0, true);
        c.set_enabled(false);
        c.set_learning(false);
        let v: serde_json::Value = serde_json::from_str(&c.status_json()).unwrap();
        assert_eq!(v["app_mode"], "correct");
        assert_eq!(v["app_reason"], "allowlisted");
        assert_eq!(v["enabled"], false);
        assert_eq!(v["learning"], false);
        c.set_field(PASSWORD, 0, true);
        let v: serde_json::Value = serde_json::from_str(&c.status_json()).unwrap();
        assert_eq!(v["app_mode"], "off");
        assert_eq!(v["app_reason"], "private_field");
    }

    /// (app_mode, app_reason) pairs of `data/status-vectors.tsv`, which the
    /// extension's statusText test reads too (R5).
    fn status_vector_pairs() -> std::collections::BTreeSet<(String, String)> {
        let text = include_str!("../../data/status-vectors.tsv");
        let mut pairs = std::collections::BTreeSet::new();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let cols: Vec<&str> = line.split('\t').collect();
            assert_eq!(cols.len(), 4, "{line:?}");
            assert!(
                ["correct", "learn_only", "off", "none"].contains(&cols[0]),
                "{line:?}"
            );
            assert!(!cols[2].is_empty() && !cols[3].is_empty(), "{line:?}");
            assert!(
                pairs.insert((cols[0].to_owned(), cols[1].to_owned())),
                "duplicate {line:?}"
            );
        }
        pairs
    }

    /// Every pair the engine can report has a row, and every row is a
    /// pair the engine can report.
    #[test]
    fn status_vectors_cover_every_emitted_pair() {
        const TERMINAL: u32 = 10;
        let c = ControlState::new(&Config {
            allowlist: vec!["firefox".into(), "kitty".into(), "gedit".into()],
            broken: vec!["gedit".into()],
            ..Config::default()
        });
        let status = |c: &ControlState| {
            let v: serde_json::Value = serde_json::from_str(&c.status_json()).unwrap();
            (
                v["app_mode"].as_str().unwrap().to_owned(),
                v["app_reason"].as_str().unwrap().to_owned(),
            )
        };
        let mut emitted = std::collections::BTreeSet::new();
        let mut reasons = Vec::new();
        // none: no field yet.
        emitted.insert(status(&c));
        // (app, purpose, hints): one context per reason the policy gives; the
        // loop below runs each with and without replace support.
        let contexts: [(&str, u32, u32); 8] = [
            ("firefox", FREE_FORM, 0),            // allowlisted, Silent
            ("kitty", FREE_FORM, 0),              // allowlisted, ProseAware
            ("org.gnome.Nautilus", FREE_FORM, 0), // not listed
            ("gedit", FREE_FORM, 0),              // broken
            ("", FREE_FORM, 0),                   // unidentified
            ("firefox", PASSWORD, 0),             // private field
            ("firefox", FREE_FORM, 1 << 11),      // private hint
            ("", TERMINAL, 0),                    // terminal
        ];
        for (app, purpose, hints) in contexts {
            for can_replace in [true, false] {
                c.set_focused_app(app);
                c.set_field(purpose, hints, can_replace);
                reasons.push(c.app_state().unwrap().1);
                emitted.insert(status(&c));
            }
        }
        // Every reason is reached (a new variant must join the table).
        for r in [
            Reason::Allowlisted,
            Reason::NotListed,
            Reason::Broken,
            Reason::PrivateField,
            Reason::Terminal,
            Reason::Unidentified,
            Reason::NoReplace,
        ] {
            match r {
                Reason::Allowlisted
                | Reason::NotListed
                | Reason::Broken
                | Reason::PrivateField
                | Reason::Terminal
                | Reason::Unidentified
                | Reason::NoReplace => assert!(reasons.contains(&r), "{r:?}"),
            }
        }
        assert_eq!(emitted, status_vector_pairs());
    }

    #[test]
    fn stats_contain_no_words() {
        let store = Arc::new(Mutex::new(LearnStore::memory()));
        store.lock().unwrap().record_word("gstack");
        store.lock().unwrap().record_word("gstack");
        let counters = Counters::default();
        counters.corrections.store(3, Ordering::Relaxed);
        counters.reverts.store(1, Ordering::Relaxed);
        let persist = PersistStats {
            persistent: true,
            quarantined_files: 2,
            key_delete_pending: false,
        };
        let json = stats_json(&counters, &store, persist);
        assert!(!json.contains("gstack"), "{json}");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["words_learned"], 1);
        assert_eq!(v["corrections"], 3);
        assert_eq!(v["reverts"], 1);
        assert_eq!(v["persistent"], true);
        assert_eq!(v["quarantined_files"], 2);
        assert_eq!(v["key_delete_pending"], false);
        // Counts and booleans only.
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), 6);
        assert!(obj.values().all(|x| x.is_u64() || x.is_boolean()), "{json}");
    }

    /// Serves `Control` over an in-process p2p connection, whose calls
    /// carry no sender: only `allow_any_caller` lets them through.
    async fn p2p_control(
        allow_any_caller: bool,
    ) -> (zbus::Connection, zbus::Connection, Arc<Shared>) {
        use corelib::dictionary::{Dictionary, LangId};
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let server = zbus::connection::Builder::unix_stream(a)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p()
            .build();
        let client = zbus::connection::Builder::unix_stream(b).p2p().build();
        let (server, client) = tokio::try_join!(server, client).unwrap();
        let config = Config {
            allow_any_caller,
            ..Config::default()
        };
        let shared = Arc::new(Shared {
            // One dictionary word, so `personal` can be false.
            dict: Arc::new(Dictionary::from_lexicons(vec![(
                LangId(0),
                vec![("bahay".to_owned(), 100)],
            )])),
            store: Arc::new(Mutex::new(LearnStore::memory())),
            counters: Arc::new(Counters::default()),
            trace: crate::trace::Trace::disabled(),
            persist: None,
            control: ControlState::new(&config),
        });
        let control = Control {
            shared: shared.clone(),
            shell: Arc::new(Mutex::new(Some(":1.12".into()))),
            import_gate: tokio::sync::Semaphore::new(1),
        };
        server
            .object_server()
            .at(CONTROL_PATH, control)
            .await
            .unwrap();
        (server, client, shared)
    }

    async fn call<B>(c: &zbus::Connection, method: &str, body: &B) -> zbus::Result<zbus::Message>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        c.call_method(None::<&str>, CONTROL_PATH, Some(CONTROL_NAME), method, body)
            .await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wire_errors_and_open_get_stats() {
        let (_server, client, shared) = p2p_control(false).await;
        for (m, r) in [
            (
                "SetFocusedApp",
                call(&client, "SetFocusedApp", &("firefox",)).await,
            ),
            (
                "SetOskVisible",
                call(&client, "SetOskVisible", &(true,)).await,
            ),
            ("SetEnabled", call(&client, "SetEnabled", &(false,)).await),
            ("Purge", call(&client, "Purge", &()).await),
            ("GetStatus", call(&client, "GetStatus", &()).await),
            ("SetLearning", call(&client, "SetLearning", &(false,)).await),
            ("RecentWords", call(&client, "RecentWords", &()).await),
            ("ForgetWord", call(&client, "ForgetWord", &("kadto",)).await),
            ("ListWords", call(&client, "ListWords", &()).await),
            (
                "ForgetWords",
                call(&client, "ForgetWords", &(vec!["kadto"],)).await,
            ),
        ] {
            match r {
                Err(zbus::Error::MethodError(name, desc, _)) => {
                    assert_eq!(
                        name.as_str(),
                        "org.freedesktop.DBus.Error.AccessDenied",
                        "{m}"
                    );
                    assert_eq!(desc.as_deref(), Some(NOT_SHELL), "{m}");
                }
                other => panic!("{m}: {other:?}"),
            }
        }
        // Nothing changed.
        assert_eq!(shared.control.ctx(0, 0).wm_class, None);
        assert!(!shared.control.osk_visible());
        assert_eq!(shared.control.mode(0, 0), Mode::Observe);
        assert!(shared.control.learning());
        let r = call(&client, "GetStats", &()).await.unwrap();
        let json: String = r.body().deserialize().unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["corrections"], 0);
    }

    fn vocab_file(text: &[u8]) -> std::fs::File {
        use std::io::{Seek, Write};
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(text).unwrap();
        // The engine reads from offset 0 whatever the caller's offset is.
        f.seek(std::io::SeekFrom::Start(3)).unwrap();
        f
    }

    async fn import_err(c: &zbus::Connection, f: &std::fs::File) -> (String, String) {
        let fd = zbus::zvariant::Fd::from(f);
        match call(c, "ImportVocabulary", &(fd,)).await {
            Err(zbus::Error::MethodError(name, desc, _)) => {
                (name.as_str().to_owned(), desc.unwrap_or_default())
            }
            other => panic!("{other:?}"),
        }
    }

    /// Over the wire, from a caller that is not the shell (p2p: no sender)
    /// and with `allow_any_caller` off. Without a persistence actor the
    /// store is never persistent, so a valid file is refused; a bad one
    /// fails parsing first. Error texts are fixed and quote nothing.
    #[tokio::test(flavor = "current_thread")]
    async fn import_over_the_wire() {
        let (_server, client, shared) = p2p_control(false).await;
        let ok = vocab_file(b"#correctiond-vocab v1\nw\tsikreto\t5\n");
        let (name, desc) = import_err(&client, &ok).await;
        assert_eq!(name, "org.nadelacruz.Correctiond1.Error.ImportFailed");
        assert_eq!(
            desc,
            "learning store not available; unlock your keyring and retry"
        );
        assert_eq!(shared.store.lock().unwrap().personal_freq("sikreto"), 0);

        let bad = vocab_file(b"sikreto\n");
        let (name, desc) = import_err(&client, &bad).await;
        assert_eq!(name, "org.nadelacruz.Correctiond1.Error.ImportFailed");
        assert_eq!(desc, "not a correctiond vocab v1 file");

        let mut huge = b"#correctiond-vocab v1\n# ".to_vec();
        huge.resize(16 * 1024 * 1024 + 1, b'a');
        let (_, desc) = import_err(&client, &vocab_file(&huge)).await;
        assert_eq!(desc, "file too large (over 16 MiB or 200,000 records)");

        // A directory fd cannot be read.
        let dir = tempfile::tempdir().unwrap();
        let d = std::fs::File::open(dir.path()).unwrap();
        let (_, desc) = import_err(&client, &d).await;
        assert_eq!(desc, "file could not be read");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn allow_any_caller_applies_calls() {
        let (_server, client, shared) = p2p_control(true).await;
        call(&client, "SetFocusedApp", &("firefox",)).await.unwrap();
        assert_eq!(shared.control.mode(0, 0), Mode::Silent);
        call(&client, "SetOskVisible", &(true,)).await.unwrap();
        assert!(shared.control.osk_visible());
        call(&client, "SetEnabled", &(false,)).await.unwrap();
        assert_eq!(shared.control.mode(0, 0), Mode::Off);
        call(&client, "SetFocusedApp", &("",)).await.unwrap();
        assert_eq!(shared.control.ctx(0, 0).wm_class, None);
        shared.store.lock().unwrap().record_word("gstack");
        shared.store.lock().unwrap().record_word("gstack");
        call(&client, "Purge", &()).await.unwrap();
        assert_eq!(shared.store.lock().unwrap().learned_word_count(), 0);
    }

    async fn reply<R>(c: &zbus::Connection, method: &str) -> R
    where
        R: for<'d> serde::Deserialize<'d> + zbus::zvariant::Type,
    {
        call(c, method, &())
            .await
            .unwrap()
            .body()
            .deserialize()
            .unwrap()
    }

    async fn forget(c: &zbus::Connection, word: &str) -> bool {
        let r = call(c, "ForgetWord", &(word,)).await.unwrap();
        r.body().deserialize().unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn set_learning_and_get_status_over_the_wire() {
        let (_server, client, shared) = p2p_control(true).await;
        let json: String = reply(&client, "GetStatus").await;
        assert!(json.contains("\"learning\":true"), "{json}");
        call(&client, "SetLearning", &(false,)).await.unwrap();
        assert!(!shared.control.learning());
        let json: String = reply(&client, "GetStatus").await;
        assert!(json.contains("\"learning\":false"), "{json}");
        call(&client, "SetLearning", &(true,)).await.unwrap();
        assert!(shared.control.learning());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recent_words_and_forget_word_over_the_wire() {
        let (_server, client, shared) = p2p_control(true).await;
        {
            let mut s = shared.store.lock().unwrap();
            for w in ["kadto", "gstack"] {
                s.record_word(w);
                s.record_word(w);
                s.note_learned(w);
            }
        }
        let words: Vec<String> = reply(&client, "RecentWords").await;
        assert_eq!(words, ["gstack", "kadto"]);
        // A second click on the same word finds nothing: false, no error.
        assert!(forget(&client, "kadto").await);
        assert!(!forget(&client, "kadto").await);
        assert_eq!(shared.store.lock().unwrap().personal_freq("kadto"), 0);
        let words: Vec<String> = reply(&client, "RecentWords").await;
        assert_eq!(words, ["gstack"]);
        // Empty or oversized words are refused without touching the store.
        assert!(!forget(&client, "").await);
        let long = "a".repeat(65);
        shared.store.lock().unwrap().record_word(&long);
        assert!(!forget(&client, &long).await);
        assert!(shared.store.lock().unwrap().personal_freq(&long) > 0);
        // 64 bytes is still a word.
        let max = "b".repeat(64);
        shared.store.lock().unwrap().record_word(&max);
        assert!(forget(&client, &max).await);
    }

    /// Neither status reply names the app.
    #[tokio::test(flavor = "current_thread")]
    async fn status_replies_carry_no_app_identity() {
        const APP: &str = "org.example.SecretWriter";
        let (_server, client, shared) = p2p_control(true).await;
        call(&client, "SetFocusedApp", &(APP,)).await.unwrap();
        shared.control.set_field(0, 0, true);
        let status: String = reply(&client, "GetStatus").await;
        let stats: String = reply(&client, "GetStats").await;
        for json in [&status, &stats] {
            let lower = json.to_lowercase();
            assert!(!lower.contains("secretwriter"), "{json}");
            assert!(!lower.contains("org.example"), "{json}");
        }
        let v: serde_json::Value = serde_json::from_str(&status).unwrap();
        assert_eq!(v["app_mode"], "learn_only");
        assert_eq!(v["app_reason"], "not_listed");
    }

    /// `call` as the Shell: a p2p connection has no unique name, so the
    /// message is built with the Shell owner's name (":1.12") as sender and
    /// the reply is picked off the stream by serial.
    async fn shell_call<B>(c: &zbus::Connection, method: &str, body: &B) -> zbus::Result<Message>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        use zbus::export::futures_core::Stream;
        use zbus::message::Type;
        let msg = Message::method_call(CONTROL_PATH, method)?
            .sender(":1.12")?
            .destination(CONTROL_NAME)?
            .interface(CONTROL_NAME)?
            .build(body)?;
        let serial = msg.primary_header().serial_num();
        let stream = zbus::MessageStream::from(c);
        c.send(&msg).await?;
        let mut stream = std::pin::pin!(stream);
        loop {
            let next = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await;
            let m = next.expect("stream ended")?;
            if m.header().reply_serial() != Some(serial) {
                continue;
            }
            return match m.message_type() {
                Type::Error => Err(m.into()),
                _ => Ok(m),
            };
        }
    }

    type Row = (String, u32, u64, bool);

    async fn list_words(c: &zbus::Connection) -> Vec<Row> {
        let r = shell_call(c, "ListWords", &()).await.unwrap();
        assert_eq!(r.body().signature().to_string(), "a(sutb)");
        let mut rows: Vec<Row> = r.body().deserialize().unwrap();
        rows.sort();
        rows
    }

    async fn forget_words(c: &zbus::Connection, words: Vec<String>) -> zbus::Result<u32> {
        let r = shell_call(c, "ForgetWords", &(words,)).await?;
        Ok(r.body().deserialize().unwrap())
    }

    fn assert_forget_failed(r: zbus::Result<u32>) {
        match r {
            Err(zbus::Error::MethodError(name, desc, _)) => {
                assert_eq!(
                    name.as_str(),
                    "org.nadelacruz.Correctiond1.Error.ForgetFailed"
                );
                assert_eq!(desc.as_deref(), Some("could not forget"));
            }
            other => panic!("{other:?}"),
        }
    }

    /// From the Shell: every stored word with count >= 2, `personal` false
    /// for a dictionary word; ForgetWords counts what existed.
    #[tokio::test(flavor = "current_thread")]
    async fn list_and_forget_words_over_the_wire() {
        let (_server, client, shared) = p2p_control(false).await;
        {
            let mut s = shared.store.lock().unwrap();
            for w in ["kadto", "bahay"] {
                s.record_word(w);
                s.record_word(w);
            }
            // Count 1: not stored, not listed.
            s.record_word("isa");
        }
        let rows = list_words(&client).await;
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(
            (rows[0].0.as_str(), rows[0].1, rows[0].3),
            ("bahay", 2, false)
        );
        assert_eq!(
            (rows[1].0.as_str(), rows[1].1, rows[1].3),
            ("kadto", 2, true)
        );
        assert!(rows[1].2 > 0);

        let n = forget_words(&client, vec!["kadto".into(), "nope".into()]).await;
        assert_eq!(n.unwrap(), 1);
        let rows = list_words(&client).await;
        assert!(rows.iter().all(|r| r.0 != "kadto"), "{rows:?}");
        assert_eq!(rows.len(), 1);
        assert_eq!(shared.store.lock().unwrap().personal_freq("kadto"), 0);
    }

    /// Bad batches are refused with the fixed text, without touching the
    /// store.
    #[tokio::test(flavor = "current_thread")]
    async fn forget_words_limits_over_the_wire() {
        let (_server, client, shared) = p2p_control(false).await;
        let long = "a".repeat(65);
        {
            let mut s = shared.store.lock().unwrap();
            for w in ["kadto", long.as_str()] {
                s.record_word(w);
                s.record_word(w);
            }
        }
        assert_forget_failed(forget_words(&client, Vec::new()).await);
        assert_forget_failed(forget_words(&client, vec!["kadto".into(), long.clone()]).await);
        assert_forget_failed(forget_words(&client, vec!["kadto".into(), String::new()]).await);
        let mut many: Vec<String> = (0..10_000).map(|i| format!("w{i}")).collect();
        many.push("kadto".into());
        assert_forget_failed(forget_words(&client, many.clone()).await);
        assert_eq!(shared.store.lock().unwrap().personal_freq("kadto"), 2);
        assert_eq!(shared.store.lock().unwrap().personal_freq(&long), 2);
        // 10 000 words of up to 64 bytes are accepted.
        many.remove(0);
        many[0] = "b".repeat(64);
        assert_eq!(forget_words(&client, many).await.unwrap(), 1);
        assert_eq!(shared.store.lock().unwrap().personal_freq("kadto"), 0);
    }

    /// R9: with `allow_any_caller`, a caller that is not the Shell (p2p: no
    /// sender) is still refused, and nothing is forgotten.
    #[tokio::test(flavor = "current_thread")]
    async fn word_methods_denied_with_allow_any_caller() {
        let (_server, client, shared) = p2p_control(true).await;
        {
            let mut s = shared.store.lock().unwrap();
            s.record_word("kadto");
            s.record_word("kadto");
        }
        for r in [
            call(&client, "ListWords", &()).await,
            call(&client, "ForgetWords", &(vec!["kadto"],)).await,
        ] {
            match r {
                Err(zbus::Error::MethodError(name, desc, _)) => {
                    assert_eq!(name.as_str(), "org.freedesktop.DBus.Error.AccessDenied");
                    assert_eq!(desc.as_deref(), Some(NOT_SHELL));
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(shared.store.lock().unwrap().personal_freq("kadto"), 2);
        // The other methods still honor the flag.
        let words: Vec<String> = reply(&client, "RecentWords").await;
        assert!(words.is_empty());
    }

    /// GetStats and GetStatus never carry a word, even with words stored.
    #[tokio::test(flavor = "current_thread")]
    async fn stats_and_status_carry_no_word() {
        let (_server, client, shared) = p2p_control(true).await;
        {
            let mut s = shared.store.lock().unwrap();
            for w in ["kadto", "bahay"] {
                s.record_word(w);
                s.record_word(w);
                s.note_learned(w);
            }
        }
        shared.control.set_field(0, 0, true);
        let status: String = reply(&client, "GetStatus").await;
        let stats: String = reply(&client, "GetStats").await;
        for json in [&status, &stats] {
            assert!(!json.contains("kadto"), "{json}");
            assert!(!json.contains("bahay"), "{json}");
        }
    }
}
