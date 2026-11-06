//! IBus D-Bus binding: Factory, Engine and Service objects, the IBusText /
//! IBusComponent wire forms, and finding the ibus-daemon address.
//!
//! Interface shapes follow libibus 1.5.34's own introspection XML
//! (`src/ibusengine.c`, `src/ibusfactory.c`, `src/ibusservice.c`) and the
//! daemon side in `bus/engineproxy.c`. Notable details:
//! - `ContentType` is a write-only `(uu)` *property* set through
//!   `org.freedesktop.DBus.Properties.Set`, not a method.
//! - `FocusId` / `ActiveSurroundingText` are advertised as `(b)` but libibus
//!   returns a plain `b` and the daemon reads it with
//!   `g_variant_get_boolean`, so ours are plain `bool`.
//! - `RequireSurroundingText` `()` and `DeleteSurroundingText` `(iu)` are
//!   signals the daemon handles although they are absent from the XML.
//!
//! `ProcessKeyEvent` corrects live: an `Action::Replace` from the state
//! machine becomes `DeleteSurroundingText` then `CommitText`, and the call
//! returns the action's `consume` flag. Everything else returns false
//! (unhandled), and so does any internal failure: typing never breaks.

use crate::control::{without_replace, ControlState};
use crate::persist::PersistHandle;
use crate::trace::Trace;
use corelib::dictionary::Dictionary;
use corelib::layout::LayoutId;
use corelib::learning::SharedStore;
use corelib::policy::Mode;
use corelib::tokenizer::{classify_key, KeyClass};
use engine::{Action, Counters, EngineState};
use std::collections::HashMap;
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::Arc;
use std::task::Poll;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Structure, StructureBuilder, Value};
use zbus::{fdo, interface, Connection, ObjectServer};

pub const BUS_NAME: &str = "org.freedesktop.IBus.Correctiond";
pub const FACTORY_PATH: &str = "/org/freedesktop/IBus/Factory";
const ENGINE_PATH_PREFIX: &str = "/org/freedesktop/IBus/Engine/";

const COMPONENT_DESCRIPTION: &str = "Local smart autocorrect with learning";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const AUTHOR: &str = "correctiond contributors";
const LICENSE: &str = "MIT OR Apache-2.0";
const TEXTDOMAIN: &str = "correctiond";

/// IBusModifierType bit set on key releases.
const RELEASE_MASK: u32 = 1 << 30;

/// IBusCapabilite bit: the client can report and delete surrounding text.
/// Without it a `DeleteSurroundingText` is dropped while the commit still
/// lands ("bulidbuild"), so no replacement may be emitted.
const CAP_SURROUNDING_TEXT: u32 = 1 << 5;

/// Cap on cursor moves attributed to our own (or passed-through) edits.
/// Kept small: each absorbed update could hide a real mouse click.
const MAX_PENDING_MOVES: u8 = 2;

/// Text kept from before the cursor: enough to verify a word, no more.
const MAX_SURROUNDING_CHARS: usize = 256;

/// One engine entry of `data/correctiond.xml`.
pub struct EngineSpec {
    pub name: &'static str,
    pub longname: &'static str,
    pub description: &'static str,
    /// GNOME Shell applies this string as a GnomeXkbInfo id, so a variant
    /// goes in here as `layout+variant`: Shell 50 (`ui/status/keyboard.js`
    /// `_getXkbId`) reads `engineDesc.variant`, a property IBusEngineDesc
    /// does not have, and so ignores `<layout_variant>`. It does not
    /// recognize the `us(colemak)` form either.
    pub layout: &'static str,
    /// XKB variant (`<layout_variant>`); empty for none. Kept empty: GNOME
    /// ignores it, and a Shell that one day reads it would build
    /// `us+colemak+colemak`, which is not a valid id.
    pub layout_variant: &'static str,
    pub symbol: &'static str,
    pub variant: LayoutId,
}

/// Must match `data/correctiond.xml` (checked by a test).
pub const ENGINES: [EngineSpec; 2] = [
    EngineSpec {
        name: "correctiond:colemak",
        longname: "Colemak, autocorrect",
        description: "Colemak with local autocorrect",
        layout: "us+colemak",
        layout_variant: "",
        symbol: "CO",
        variant: LayoutId::Colemak,
    },
    EngineSpec {
        name: "correctiond:us",
        longname: "US, autocorrect",
        description: "US QWERTY with local autocorrect",
        layout: "us",
        layout_variant: "",
        symbol: "US",
        variant: LayoutId::Qwerty,
    },
];

/// The layout variant served for a `CreateEngine` name.
pub fn variant_for(name: &str) -> Option<LayoutId> {
    ENGINES.iter().find(|e| e.name == name).map(|e| e.variant)
}

/// Every IBusSerializable starts with its type name and an empty
/// attachment dict `a{sv}`.
type Attachments = HashMap<String, Value<'static>>;

/// Serialized `IBusText` with no attributes:
/// `('IBusText', {}, text, <('IBusAttrList', {}, @av [])>)`.
/// A `Value` tuple element serializes as the variant `v`.
pub fn ibus_text(s: &str) -> Value<'static> {
    let attrs = Structure::from((
        "IBusAttrList",
        Attachments::new(),
        Vec::<Value<'static>>::new(),
    ));
    Value::from(Structure::from((
        "IBusText",
        Attachments::new(),
        s.to_owned(),
        Value::from(attrs),
    )))
}

/// The text of a serialized IBusText (field 2), if `v` is one.
pub fn text_of(v: &Value<'_>) -> Option<String> {
    let v = match v {
        Value::Value(inner) => inner.as_ref(),
        other => other,
    };
    let Value::Structure(s) = v else { return None };
    match s.fields() {
        [Value::Str(kind), _, Value::Str(text), ..] if kind.as_str() == "IBusText" => {
            Some(text.to_string())
        }
        _ => None,
    }
}

/// Serialized `IBusEngineDesc`, field order from `ibusenginedesc.c`:
/// name, longname, description, language, license, author, icon, layout,
/// rank (u), hotkeys, symbol, setup, layout_variant, layout_option, version,
/// textdomain, icon_prop_key.
fn engine_desc(e: &EngineSpec) -> zbus::zvariant::Result<Structure<'static>> {
    let mut b = StructureBuilder::new()
        .add_field("IBusEngineDesc")
        .add_field(Attachments::new());
    for s in [
        e.name,
        e.longname,
        e.description,
        "en",
        LICENSE,
        AUTHOR,
        "ibus-keyboard",
        e.layout,
    ] {
        b = b.add_field(s);
    }
    b = b.add_field(0u32);
    // hotkeys, symbol, setup, layout_variant (field 14), layout_option,
    // version, textdomain, icon_prop_key.
    for s in [
        "",
        e.symbol,
        "",
        e.layout_variant,
        "",
        VERSION,
        TEXTDOMAIN,
        "",
    ] {
        b = b.add_field(s);
    }
    b.build()
}

/// Serialized `IBusComponent` with both engines, for `RegisterComponent`:
/// name, description, version, license, author, homepage, exec, textdomain,
/// observed paths (av), engines (av).
pub fn component(exec: &str) -> zbus::zvariant::Result<Value<'static>> {
    let mut engines = Vec::with_capacity(ENGINES.len());
    for e in &ENGINES {
        engines.push(Value::from(engine_desc(e)?));
    }
    let mut b = StructureBuilder::new()
        .add_field("IBusComponent")
        .add_field(Attachments::new());
    for s in [
        BUS_NAME,
        COMPONENT_DESCRIPTION,
        VERSION,
        LICENSE,
        AUTHOR,
        "",
        exec,
        TEXTDOMAIN,
    ] {
        b = b.add_field(s.to_owned());
    }
    Ok(Value::from(
        b.add_field(Vec::<Value<'static>>::new())
            .add_field(engines)
            .build()?,
    ))
}

/// The ibus-daemon address: `IBUS_ADDRESS`, else the daemon's address file
/// under `$XDG_CONFIG_HOME/ibus/bus/` (`~/.config/ibus/bus/`).
pub fn ibus_address(home: &Path) -> Option<String> {
    if let Some(a) = std::env::var_os("IBUS_ADDRESS").filter(|a| !a.is_empty()) {
        return a.into_string().ok();
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map_or_else(|| home.join(".config"), Into::into);
    let dir = config.join("ibus/bus");
    // Prefer this display's file (ibus names it <machine-id>-unix-<display>),
    // then the most recently written one.
    let display = std::env::var("WAYLAND_DISPLAY").ok().or_else(|| {
        let d = std::env::var("DISPLAY").ok()?;
        let n = d.split(':').nth(1)?.split('.').next()?.to_owned();
        Some(n)
    });
    let machine = std::fs::read_to_string("/etc/machine-id")
        .or_else(|_| std::fs::read_to_string("/var/lib/dbus/machine-id"))
        .ok()
        .map(|m| m.trim().to_owned());
    if let (Some(m), Some(d)) = (&machine, &display) {
        let p = dir.join(format!("{m}-unix-{d}"));
        if let Some(a) = std::fs::read_to_string(p)
            .ok()
            .and_then(|t| parse_address_file(&t))
        {
            return Some(a);
        }
    }
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort();
    files
        .iter()
        .rev()
        .find_map(|(_, p)| parse_address_file(&std::fs::read_to_string(p).ok()?))
}

/// The `IBUS_ADDRESS=` value of an ibus address file.
pub fn parse_address_file(text: &str) -> Option<String> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix("IBUS_ADDRESS="))
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_owned)
}

/// Process-wide state every engine instance shares.
pub struct Shared {
    pub dict: Arc<Dictionary>,
    pub store: SharedStore,
    pub counters: Arc<Counters>,
    pub trace: Trace,
    /// The persistence actor; `None` in tests that run without one.
    pub persist: Option<PersistHandle>,
    /// Focused app, OSK and on/off from the control plane, plus the config
    /// (allowlist, broken) that turns them into a `Mode`.
    pub control: ControlState,
}

pub struct Factory {
    shared: Arc<Shared>,
    next_id: u64,
}

impl Factory {
    pub fn new(shared: Arc<Shared>) -> Factory {
        Factory { shared, next_id: 1 }
    }
}

#[interface(name = "org.freedesktop.IBus.Factory")]
impl Factory {
    async fn create_engine(
        &mut self,
        name: String,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> fdo::Result<OwnedObjectPath> {
        let Some(variant) = variant_for(&name) else {
            return Err(fdo::Error::Failed("unknown engine name".into()));
        };
        let path = format!("{ENGINE_PATH_PREFIX}{}", self.next_id);
        self.next_id += 1;
        let path =
            OwnedObjectPath::try_from(path).map_err(|e| fdo::Error::Failed(e.to_string()))?;
        serve_engine(server, &path, Engine::new(self.shared.clone(), variant)).await?;
        let label = match variant {
            LayoutId::Colemak => "colemak",
            _ => "us",
        };
        self.shared.trace.event(&["CreateEngine", label], &[]);
        Ok(path)
    }
}

/// Puts one engine instance on the bus at `path`.
pub async fn serve_engine(
    server: &ObjectServer,
    path: &OwnedObjectPath,
    engine: Engine,
) -> zbus::Result<()> {
    server.at(path, engine).await?;
    server.at(path, Service).await?;
    // zbus adds its own fdo::Properties to every node, and that interface
    // always spawns a task per call, which lets Properties.Set(ContentType)
    // run after keys that arrived behind it. Replace it with one that runs
    // inline in the object server's ordered dispatch loop.
    server.remove::<fdo::Properties, _>(path).await?;
    server.at(path, EngineProperties).await?;
    Ok(())
}

/// `org.freedesktop.DBus.Properties` for engine paths.
///
/// zbus's object server reads one message at a time and awaits its
/// dispatch, and `spawn = false` interfaces run inside that await. The
/// Engine interface and this one are both `spawn = false`, so a
/// `ContentType` change takes effect before any key that arrived after it.
pub struct EngineProperties;

const ENGINE_IFACE: &str = "org.freedesktop.IBus.Engine";

/// The Engine at the called path, for an `org.freedesktop.IBus.Engine` call.
async fn engine_ref(
    server: &ObjectServer,
    hdr: &Header<'_>,
    interface_name: &str,
) -> fdo::Result<zbus::object_server::InterfaceRef<Engine>> {
    if interface_name != ENGINE_IFACE {
        return Err(fdo::Error::UnknownInterface(format!(
            "no properties on {interface_name}"
        )));
    }
    let path = hdr
        .path()
        .ok_or_else(|| fdo::Error::Failed("missing path".into()))?;
    server
        .interface::<_, Engine>(path)
        .await
        .map_err(|_| fdo::Error::UnknownObject("no engine at this path".into()))
}

fn owned(v: Value<'static>) -> fdo::Result<OwnedValue> {
    OwnedValue::try_from(v).map_err(|e| fdo::Error::Failed(e.to_string()))
}

#[interface(name = "org.freedesktop.DBus.Properties", spawn = false)]
impl EngineProperties {
    async fn get(
        &self,
        interface_name: &str,
        property_name: &str,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(header)] hdr: Header<'_>,
    ) -> fdo::Result<OwnedValue> {
        let engine = engine_ref(server, &hdr, interface_name).await?;
        let v = engine.get().await.property(property_name);
        owned(v.ok_or_else(|| fdo::Error::UnknownProperty(property_name.to_owned()))?)
    }

    async fn set(
        &self,
        interface_name: &str,
        property_name: &str,
        value: Value<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(header)] hdr: Header<'_>,
    ) -> fdo::Result<()> {
        let engine = engine_ref(server, &hdr, interface_name).await?;
        match property_name {
            "ContentType" => {
                let ct = <(u32, u32)>::try_from(value)
                    .map_err(|_| fdo::Error::InvalidArgs("ContentType is (uu)".into()))?;
                engine.get_mut().await.set_content_type(ct);
                Ok(())
            }
            "FocusId" | "ActiveSurroundingText" => {
                Err(fdo::Error::PropertyReadOnly(property_name.to_owned()))
            }
            _ => Err(fdo::Error::UnknownProperty(property_name.to_owned())),
        }
    }

    async fn get_all(
        &self,
        interface_name: &str,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(header)] hdr: Header<'_>,
    ) -> fdo::Result<HashMap<String, OwnedValue>> {
        let engine = engine_ref(server, &hdr, interface_name).await?;
        let e = engine.get().await;
        let mut all = HashMap::new();
        for name in ["FocusId", "ActiveSurroundingText", "ContentType"] {
            if let Some(v) = e.property(name) {
                all.insert(name.to_owned(), owned(v)?);
            }
        }
        Ok(all)
    }
}

/// `org.freedesktop.IBus.Service`: the daemon destroys engines it is done
/// with through this.
pub struct Service;

#[interface(name = "org.freedesktop.IBus.Service")]
impl Service {
    async fn destroy(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] hdr: Header<'_>,
    ) {
        let Some(path) = hdr.path().map(|p| p.to_owned()) else {
            return;
        };
        // Remove from a separate task: not while our own call is dispatched.
        let conn = conn.clone();
        tokio::spawn(async move {
            let server = conn.object_server();
            let _ = server.remove::<Engine, _>(&path).await;
            let _ = server.remove::<Service, _>(&path).await;
        });
    }
}

/// Name of the key class, never its character.
fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Off => "off",
        Mode::Observe => "observe",
        Mode::Silent => "silent",
        Mode::ProseAware => "prose",
    }
}

fn class_name(k: &KeyClass) -> &'static str {
    match k {
        KeyClass::Printable(_) => "Printable",
        KeyClass::Boundary(_) => "Boundary",
        KeyClass::Backspace => "Backspace",
        KeyClass::Invalidate => "Invalidate",
    }
}

/// Keys that edit nothing on their own: releases and bare modifiers.
/// Feeding them would duplicate every char (release) or discard the word
/// on every Shift press.
pub fn is_ignorable(keyval: u32, state: u32) -> bool {
    state & RELEASE_MASK != 0
        || matches!(
            keyval,
            // Shift_L..Hyper_R (incl. Caps_Lock, Shift_Lock, Meta, Alt, Super)
            0xffe1..=0xffee
            // ISO_Lock, ISO_Level2/3/5 shifts and latches, ISO group keys
            | 0xfe01..=0xfe0f
            | 0xfe11..=0xfe13
            // Mode_switch, Num_Lock, Scroll_Lock
            | 0xff7e
            | 0xff7f
            | 0xff14
        )
}

/// `before` with its last `delete_chars` chars replaced by `commit`, kept
/// to the last `MAX_SURROUNDING_CHARS` chars.
fn replaced(before: &str, delete_chars: u32, commit: &str) -> String {
    let mut chars: Vec<char> = before.chars().collect();
    let keep = chars.len().saturating_sub(delete_chars as usize);
    chars.truncate(keep);
    chars.extend(commit.chars());
    let from = chars.len().saturating_sub(MAX_SURROUNDING_CHARS);
    chars[from..].iter().collect()
}

/// Runs `f` with every poll inside `catch_unwind`: a panic anywhere in it
/// gives `None` (and it is not polled again) instead of unwinding out of
/// the D-Bus handler.
async fn guarded<F: Future>(f: F) -> Option<F::Output> {
    let mut f = std::pin::pin!(f);
    std::future::poll_fn(
        move |cx| match catch_unwind(AssertUnwindSafe(|| f.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(v)) => Poll::Ready(Some(v)),
            Err(_) => Poll::Ready(None),
        },
    )
    .await
}

/// One engine instance (one per input context the daemon gives us).
pub struct Engine {
    state: EngineState,
    shared: Arc<Shared>,
    purpose: u32,
    hints: u32,
    /// Text before the cursor from the last SetSurroundingText.
    surrounding: Option<String>,
    /// Cursor moves expected from edits we committed or let through.
    /// A SetCursorLocation beyond these means the user moved the cursor.
    pending_moves: u8,
    /// Client capability bits from the last SetCapabilities (none before).
    caps: u32,
}

impl Engine {
    pub fn new(shared: Arc<Shared>, variant: LayoutId) -> Engine {
        let state = EngineState::new(
            shared.dict.clone(),
            shared.store.clone(),
            shared.counters.clone(),
            variant,
        );
        Engine {
            state,
            shared,
            purpose: 0,
            hints: 0,
            surrounding: None,
            pending_moves: 0,
            caps: 0,
        }
    }

    /// Whether this client can apply a DeleteSurroundingText.
    fn can_replace(&self) -> bool {
        self.caps & CAP_SURROUNDING_TEXT != 0
    }

    /// Last ContentType plus control-plane state. An unidentified app
    /// (no wm_class) only observes. A client without surrounding-text
    /// support is never corrected: Silent becomes Observe (same learning,
    /// no Replace), and ProseAware becomes Off, because terminals never
    /// learn and Observe would.
    fn mode(&self) -> Mode {
        let mode = self.shared.control.mode(self.purpose, self.hints);
        if self.can_replace() {
            mode
        } else {
            without_replace(mode)
        }
    }

    /// Tells the control plane about this field, for `GetStatus`. Any engine
    /// instance's SetContentType/SetCapabilities may update the shared last
    /// field; IBus sends these for the focused context.
    fn report_field(&self) {
        self.shared
            .control
            .set_field(self.purpose, self.hints, self.can_replace());
    }

    /// Trace line, unless the field is sensitive: nothing at all in Off.
    fn trace(&self, words: &[&'static str], fields: &[(&'static str, u64)]) {
        if self.mode() != Mode::Off {
            self.shared.trace.event(words, fields);
        }
    }

    fn invalidate(&mut self) {
        self.state.on_invalidate();
        self.surrounding = None;
        self.pending_moves = 0;
    }

    /// The synchronous half of ProcessKeyEvent: feeds the key to the state
    /// machine and keeps the cursor bookkeeping in step with what the app
    /// will see. Emitting the action is the caller's job.
    fn handle_key(&mut self, keyval: u32, state: u32) -> Action {
        if is_ignorable(keyval, state) {
            return Action::Pass;
        }
        let mode = self.mode();
        if mode == Mode::Off {
            // Nothing typed into a sensitive field enters the word buffer,
            // and no cursor update is attributed to it.
            self.invalidate();
            return Action::Pass;
        }
        let k = classify_key(keyval, state);
        self.trace(&["key", class_name(&k)], &[]);
        let osk = self.shared.control.osk_visible();
        let boundary = matches!(k, KeyClass::Boundary(_));
        // SetLearning reaches the very next key.
        self.state.set_learning(self.shared.control.learning());
        let mut action = self.state.on_key(k, mode, osk, self.surrounding.as_deref());
        if boundary {
            self.trace(
                &["boundary", self.state.last_outcome, mode_name(mode)],
                &[
                    ("allowed", u64::from(self.state.last_allowed)),
                    ("surrounding", u64::from(self.surrounding.is_some())),
                ],
            );
        }
        if !self.can_replace() && action != Action::Pass {
            // Unreachable by mode() and set_capabilities; never emit one
            // regardless, and forget the correction the state believes in.
            self.invalidate();
            action = Action::Pass;
        }
        match &action {
            Action::Replace {
                delete_chars,
                commit,
                consume,
            } => {
                // No length or text: only that a replacement happened.
                self.trace(&["Replace"], &[]);
                // Our delete and commit (and a passed Return) move the cursor.
                self.pending_moves = MAX_PENDING_MOVES;
                // Until the client sends the new text, predict it, so an
                // immediate Backspace can verify the revert. A key that
                // still reaches the app leaves it unknown.
                self.surrounding = if *consume {
                    self.surrounding
                        .as_deref()
                        .map(|s| replaced(s, *delete_chars, commit))
                } else {
                    None
                };
            }
            Action::Pass if k == KeyClass::Invalidate => {
                // An arrow or shortcut moves the cursor itself: its cursor
                // event must not be absorbed, and the surrounding text is
                // now stale.
                self.pending_moves = 0;
                self.surrounding = None;
            }
            Action::Pass => {
                // The app inserts or deletes the char and moves the cursor.
                self.pending_moves = (self.pending_moves + 1).min(MAX_PENDING_MOVES);
            }
        }
        action
    }

    /// `DeleteSurroundingText(-n, n)` then `CommitText`, in that order.
    async fn emit_replace(
        emitter: &SignalEmitter<'_>,
        delete_chars: u32,
        commit: &str,
    ) -> zbus::Result<()> {
        let offset = i32::try_from(delete_chars)
            .map_err(|_| zbus::Error::Failure("delete too long".into()))?;
        Self::delete_surrounding_text(emitter, -offset, delete_chars).await?;
        Self::commit_text(emitter, ibus_text(commit)).await
    }

    /// The `ContentType` property (written by the daemon through
    /// `EngineProperties`, never by zbus's spawned `fdo::Properties`).
    pub fn set_content_type(&mut self, value: (u32, u32)) {
        let was = self.mode();
        (self.purpose, self.hints) = value;
        self.report_field();
        let now = self.mode();
        if now == Mode::Off && was != Mode::Off {
            // Drop anything buffered before the field turned sensitive.
            self.invalidate();
        }
        self.trace(
            &["ContentType"],
            &[
                ("purpose", u64::from(self.purpose)),
                ("hints", u64::from(self.hints)),
            ],
        );
    }

    /// Readable properties of `org.freedesktop.IBus.Engine`:
    /// - `FocusId = false`: we take `FocusIn`/`FocusOut`, not the `*Id` ones.
    /// - `ActiveSurroundingText = true`: the daemon then requests surrounding
    ///   text on focus and enable.
    /// - `ContentType`, which libibus declares write-only, is readable here
    ///   anyway for tests.
    fn property(&self, name: &str) -> Option<Value<'static>> {
        match name {
            "FocusId" => Some(Value::from(false)),
            "ActiveSurroundingText" => Some(Value::from(true)),
            "ContentType" => Some(Value::from(Structure::from((self.purpose, self.hints)))),
            _ => None,
        }
    }

    fn lifecycle(&mut self, name: &'static str) {
        self.invalidate();
        self.trace(&[name], &[]);
    }

    /// A new focus is a good moment to retry a locked keyring.
    fn persist_nudge(&self) {
        if let Some(p) = &self.shared.persist {
            if !p.stats().persistent {
                p.nudge();
            }
        }
    }

    /// Write-behind on focus loss and disable; the handler does not wait.
    fn persist_flush(&self) {
        if let Some(p) = &self.shared.persist {
            p.flush_in_background();
        }
    }
}

#[interface(name = "org.freedesktop.IBus.Engine", spawn = false)]
impl Engine {
    /// True only when a correction swallowed the key (its char is inside
    /// the commit). Never panics, never errors: any failure is false.
    async fn process_key_event(
        &mut self,
        keyval: u32,
        _keycode: u32,
        state: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> bool {
        let action = match catch_unwind(AssertUnwindSafe(|| self.handle_key(keyval, state))) {
            Ok(a) => a,
            Err(_) => {
                // Drop whatever the failed call left behind and keep going.
                let _ = catch_unwind(AssertUnwindSafe(|| self.invalidate()));
                return false;
            }
        };
        let Action::Replace {
            delete_chars,
            commit,
            consume,
        } = action
        else {
            return false;
        };
        match guarded(Self::emit_replace(&emitter, delete_chars, &commit)).await {
            Some(Ok(())) => consume,
            // An emit error or a panic while emitting.
            Some(Err(_)) | None => {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    eprintln!("correctiond: correction signals failed");
                    // The app's text is unknown now; let the key through.
                    self.invalidate();
                }));
                false
            }
        }
    }

    fn set_cursor_location(&mut self, _x: i32, _y: i32, _w: i32, _h: i32) {
        if self.mode() == Mode::Off {
            self.invalidate();
            return;
        }
        if self.pending_moves > 0 {
            self.pending_moves -= 1;
            return;
        }
        // Not caused by an edit we know of: the user moved the cursor.
        self.state.on_invalidate();
        self.surrounding = None;
        self.trace(&["SetCursorLocation", "invalidate"], &[]);
    }

    fn process_hand_writing_event(&mut self, _coordinates: Vec<f64>) {}

    fn cancel_hand_writing(&mut self, _n_strokes: u32) {}

    fn set_capabilities(&mut self, caps: u32) {
        let could = self.can_replace();
        self.caps = caps;
        self.report_field();
        if could && !self.can_replace() {
            // A pending revert would be a Replace too: drop it.
            self.invalidate();
        }
        self.trace(&["SetCapabilities"], &[("caps", u64::from(caps))]);
    }

    fn property_activate(&mut self, _name: String, _state: u32) {}

    fn property_show(&mut self, _name: String) {}

    fn property_hide(&mut self, _name: String) {}

    fn candidate_clicked(&mut self, _index: u32, _button: u32, _state: u32) {}

    fn focus_in(&mut self) {
        self.lifecycle("FocusIn");
        self.report_field();
        self.persist_nudge();
    }

    fn focus_in_id(&mut self, _object_path: String, _client: String) {
        self.lifecycle("FocusIn");
        self.report_field();
        self.persist_nudge();
    }

    /// Leaves the control plane's last field as it is (see `set_field`).
    fn focus_out(&mut self) {
        self.lifecycle("FocusOut");
        self.persist_flush();
    }

    fn focus_out_id(&mut self, _object_path: String) {
        self.lifecycle("FocusOut");
        self.persist_flush();
    }

    fn reset(&mut self) {
        self.lifecycle("Reset");
    }

    /// Asks the client for surrounding text right away (warm-up), so the
    /// first correction does not have to wait for it.
    async fn enable(&mut self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        self.lifecycle("Enable");
        if Self::require_surrounding_text(&emitter).await.is_err() {
            eprintln!("correctiond: RequireSurroundingText signal failed");
        }
    }

    fn disable(&mut self) {
        self.lifecycle("Disable");
        self.persist_flush();
    }

    fn page_up(&mut self) {}

    fn page_down(&mut self) {}

    fn cursor_up(&mut self) {}

    fn cursor_down(&mut self) {}

    fn set_surrounding_text(&mut self, text: Value<'_>, cursor_pos: u32, _anchor_pos: u32) {
        if self.mode() == Mode::Off {
            self.surrounding = None;
            return;
        }
        self.surrounding = text_of(&text).map(|t| {
            let before: Vec<char> = t.chars().take(cursor_pos as usize).collect();
            let from = before.len().saturating_sub(MAX_SURROUNDING_CHARS);
            before[from..].iter().collect()
        });
        // No positions: the cursor offset would reveal the text's length.
        self.trace(&["SetSurroundingText"], &[]);
    }

    fn panel_extension_received(&mut self, _event: Value<'_>) {}

    fn panel_extension_register_keys(&mut self, _data: Value<'_>) {}

    /// `CommitText(v)`: the text is a serialized IBusText (`ibus_text`).
    #[zbus(signal)]
    pub async fn commit_text(emitter: &SignalEmitter<'_>, text: Value<'_>) -> zbus::Result<()>;

    /// `DeleteSurroundingText(iu)`: offset from the cursor, char count.
    #[zbus(signal)]
    pub async fn delete_surrounding_text(
        emitter: &SignalEmitter<'_>,
        offset_from_cursor: i32,
        nchars: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn require_surrounding_text(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn forward_key_event(
        emitter: &SignalEmitter<'_>,
        keyval: u32,
        keycode: u32,
        state: u32,
    ) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use corelib::dictionary::LangId;
    use corelib::learning::LearnStore;
    use corelib::policy::Reason;
    use corelib::scoring::PersonalLookup;
    use serde_json::{json, Map, Value as Json};
    use std::sync::Mutex;

    /// Same shape as `scripts/gen-ibus-golden.py`: structs become
    /// {"signature", "fields"}, variants are unwrapped.
    fn walk(v: &Value<'_>) -> Json {
        match v {
            Value::Value(inner) => walk(inner),
            Value::Structure(s) => json!({
                "signature": s.signature().to_string(),
                "fields": s.fields().iter().map(walk).collect::<Vec<_>>(),
            }),
            Value::Dict(d) => {
                let mut m = Map::new();
                for (k, v) in d.iter() {
                    let Value::Str(k) = k else {
                        panic!("non-string dict key");
                    };
                    m.insert(k.to_string(), walk(v));
                }
                Json::Object(m)
            }
            Value::Array(a) => Json::Array(a.iter().map(walk).collect()),
            Value::Str(s) => Json::String(s.to_string()),
            Value::U32(n) => json!(n),
            other => panic!("unexpected value in IBus serialization: {other:?}"),
        }
    }

    #[test]
    fn ibustext_matches_golden() {
        let golden: Json =
            serde_json::from_str(include_str!("../tests/golden/ibustext_build.json")).unwrap();
        let ours = ibus_text("build ");
        assert_eq!(ours.value_signature().to_string(), "(sa{sv}sv)");
        assert_eq!(walk(&ours), golden);
    }

    #[test]
    fn text_of_reads_back_ibus_text() {
        assert_eq!(text_of(&ibus_text("héllo")).as_deref(), Some("héllo"));
        let wrapped = Value::Value(Box::new(ibus_text("x")));
        assert_eq!(text_of(&wrapped).as_deref(), Some("x"));
        assert_eq!(text_of(&Value::from("plain")), None);
    }

    #[test]
    fn component_signatures_match_libibus() {
        // Captured with python-gi: IBus.Component / IBus.EngineDesc
        // serialize_object().get_type_string() on this machine.
        let c = component("/x/engine").unwrap();
        assert_eq!(c.value_signature().to_string(), "(sa{sv}ssssssssavav)");
        let Value::Structure(s) = &c else {
            panic!("not a struct")
        };
        let Value::Array(engines) = &s.fields()[11] else {
            panic!("engines not an array")
        };
        assert_eq!(engines.len(), 2);
        for (e, spec) in engines.iter().zip(&ENGINES) {
            let e = match e {
                Value::Value(inner) => inner.as_ref(),
                other => other,
            };
            assert_eq!(e.value_signature().to_string(), "(sa{sv}ssssssssussssssss)");
            let Value::Structure(e) = e else { panic!() };
            assert_eq!(e.fields()[2], Value::from(spec.name));
            assert_eq!(e.fields()[9], Value::from(spec.layout));
            assert_eq!(e.fields()[14], Value::from(spec.layout_variant));
        }
    }

    #[test]
    fn engines_match_component_xml() {
        let xml = include_str!("../../data/correctiond.xml");
        assert!(xml.contains(&format!("<name>{BUS_NAME}</name>")));
        assert_eq!(xml.matches("<engine>").count(), ENGINES.len());
        assert_eq!(xml.matches("<exec>").count(), 1);
        let with_variant = ENGINES.iter().filter(|e| !e.layout_variant.is_empty());
        assert_eq!(
            xml.matches("<layout_variant>").count(),
            with_variant.count()
        );
        for e in &ENGINES {
            // Each engine's block, from its <name> to the next </engine>.
            let start = xml
                .find(&format!("<name>{}</name>", e.name))
                .expect("engine in XML");
            let block = &xml[start..start + xml[start..].find("</engine>").unwrap()];
            assert!(block.contains(&format!("<layout>{}</layout>", e.layout)));
            assert!(block.contains(&format!("<longname>{}</longname>", e.longname)));
            if e.layout_variant.is_empty() {
                assert!(!block.contains("<layout_variant>"));
            } else {
                assert!(block.contains(&format!(
                    "<layout_variant>{}</layout_variant>",
                    e.layout_variant
                )));
            }
        }
    }

    #[test]
    fn names_select_layout_variants() {
        assert_eq!(variant_for("correctiond:colemak"), Some(LayoutId::Colemak));
        assert_eq!(variant_for("correctiond:us"), Some(LayoutId::Qwerty));
        assert_eq!(variant_for("xkb:us::eng"), None);
        let colemak = &ENGINES[0];
        assert_eq!((colemak.layout, colemak.layout_variant), ("us+colemak", ""));
        let us = &ENGINES[1];
        assert_eq!((us.layout, us.layout_variant), ("us", ""));
    }

    #[test]
    fn address_file_parsing() {
        let t = "# This file is created by ibus-daemon, please do not modify it.\n\
                 # IBUS_ADDRESS=unix:path=/old\n\
                 IBUS_ADDRESS=unix:path=/home/u/.cache/ibus/dbus-abc,guid=123\n\
                 IBUS_DAEMON_PID=42\n";
        assert_eq!(
            parse_address_file(t).as_deref(),
            Some("unix:path=/home/u/.cache/ibus/dbus-abc,guid=123")
        );
        assert_eq!(parse_address_file("IBUS_DAEMON_PID=1\n"), None);
    }

    #[test]
    fn releases_and_bare_modifiers_are_ignored() {
        assert!(is_ignorable(u32::from('a'), RELEASE_MASK));
        assert!(is_ignorable(0xffe1, 0)); // Shift_L
        assert!(is_ignorable(0xffe5, 0)); // Caps_Lock
        assert!(is_ignorable(0xfe03, 0)); // ISO_Level3_Shift
        assert!(!is_ignorable(u32::from('a'), 0));
        assert!(!is_ignorable(u32::from('A'), 1)); // Shift held
        assert!(!is_ignorable(0xff08, 0)); // BackSpace
        assert!(!is_ignorable(0xff51, 0)); // Left arrow still invalidates
    }

    /// An English dictionary that corrects "bulid" to "build", and a
    /// config whose allowlist is the app "e2e".
    fn shared(trace: Trace) -> Arc<Shared> {
        let words = [("the", 1000u64), ("build", 50), ("hello", 80)]
            .iter()
            .map(|(w, c)| (w.to_string(), *c))
            .collect();
        let config = Config {
            allowlist: vec!["e2e".into()],
            ..Config::default()
        };
        Arc::new(Shared {
            dict: Arc::new(Dictionary::from_lexicons(vec![(LangId(0), words)])),
            store: Arc::new(Mutex::new(LearnStore::memory())),
            counters: Arc::new(Counters::default()),
            trace,
            persist: None,
            control: ControlState::new(&config),
        })
    }

    fn engine() -> Engine {
        Engine::new(shared(Trace::disabled()), LayoutId::Qwerty)
    }

    /// IBUS_CAP_FOCUS | IBUS_CAP_SURROUNDING_TEXT, as GTK sends them.
    const CAPS_SURROUNDING: u32 = 8 | 32;
    /// IBUS_CAP_FOCUS only: the client cannot apply a replacement.
    const CAPS_NO_SURROUNDING: u32 = 8;

    /// An engine in an allowlisted app (Mode::Silent for FREE_FORM), on a
    /// client that supports surrounding text.
    fn silent_engine() -> Engine {
        let mut e = engine();
        e.set_capabilities(CAPS_SURROUNDING);
        e.shared.control.set_focused_app("e2e");
        e
    }

    /// The synchronous half of ProcessKeyEvent.
    fn key(e: &mut Engine, keyval: u32, state: u32) -> Action {
        e.handle_key(keyval, state)
    }

    #[test]
    fn keys_pass_through_in_every_mode() {
        let mut e = engine();
        for (purpose, hints) in [(0, 0), (10, 0), (8, 0), (0, 1 << 11)] {
            (e.purpose, e.hints) = (purpose, hints);
            for k in [u32::from('a'), 0x20, 0xff08, 0xff0d, 0xffe1, 0xff51] {
                assert_eq!(key(&mut e, k, 0), Action::Pass);
                assert_eq!(key(&mut e, k, RELEASE_MASK), Action::Pass);
            }
        }
    }

    #[test]
    fn mode_comes_from_content_type_without_wm_class() {
        let mut e = engine();
        assert_eq!(e.mode(), Mode::Observe);
        e.set_content_type((8, 0)); // PASSWORD
        assert_eq!(e.mode(), Mode::Off);
        e.set_content_type((10, 0)); // TERMINAL, unidentified app
        assert_eq!(e.mode(), Mode::Off);
        e.set_content_type((0, 1 << 1)); // NO_SPELLCHECK
        assert_eq!(e.mode(), Mode::Off);
        e.set_content_type((0, 0));
        assert_eq!(e.mode(), Mode::Observe);
    }

    #[test]
    fn surrounding_text_keeps_only_text_before_cursor() {
        let mut e = engine();
        e.set_surrounding_text(ibus_text("héllo world"), 5, 5);
        assert_eq!(e.surrounding.as_deref(), Some("héllo"));
        e.set_content_type((8, 0));
        e.set_surrounding_text(ibus_text("secret"), 6, 6);
        assert_eq!(e.surrounding, None);
    }

    #[test]
    fn cursor_moves_from_typing_do_not_invalidate() {
        let mut e = engine();
        key(&mut e, u32::from('h'), 0);
        key(&mut e, u32::from('h'), RELEASE_MASK);
        key(&mut e, u32::from('i'), 0);
        assert_eq!(e.pending_moves, 2);
        e.set_cursor_location(1, 0, 1, 10);
        e.set_cursor_location(2, 0, 1, 10);
        assert_eq!(e.pending_moves, 0);
        e.set_surrounding_text(ibus_text("hi"), 2, 2);
        // A move nobody typed for (mouse click) drops the position.
        e.set_cursor_location(40, 0, 1, 10);
        assert_eq!(e.surrounding, None);
    }

    /// Serves one engine over an in-process p2p connection; returns the
    /// client side. Tracing goes to `trace`.
    async fn p2p_engine(trace: &Path, app: Option<&str>) -> (Connection, Connection) {
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let guid = zbus::Guid::generate();
        let server = zbus::connection::Builder::unix_stream(a)
            .server(guid)
            .unwrap()
            .p2p()
            .build();
        let client = zbus::connection::Builder::unix_stream(b).p2p().build();
        let (server, client) = tokio::try_join!(server, client).unwrap();
        let shared = shared(Trace::open(trace.to_path_buf()));
        if let Some(app) = app {
            shared.control.set_focused_app(app);
        }
        let path = OwnedObjectPath::try_from(TEST_PATH).unwrap();
        let engine = Engine::new(shared, LayoutId::Qwerty);
        serve_engine(server.object_server(), &path, engine)
            .await
            .unwrap();
        (server, client)
    }

    const TEST_PATH: &str = "/org/freedesktop/IBus/Engine/1";
    const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";

    fn set_content_type_msg(purpose: u32, hints: u32) -> zbus::Message {
        let v = Value::from(Structure::from((purpose, hints)));
        zbus::Message::method_call(TEST_PATH, "Set")
            .unwrap()
            .interface(PROPS_IFACE)
            .unwrap()
            .build(&(ENGINE_IFACE, "ContentType", v))
            .unwrap()
    }

    fn key_msg(keyval: u32) -> zbus::Message {
        zbus::Message::method_call(TEST_PATH, "ProcessKeyEvent")
            .unwrap()
            .interface(ENGINE_IFACE)
            .unwrap()
            .build(&(keyval, 0u32, 0u32))
            .unwrap()
    }

    fn trace_bodies(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|l| l.split_once(' ').map_or("", |(_, b)| b).to_string())
            .collect()
    }

    /// Properties.Set(ContentType) and ProcessKeyEvent take effect in
    /// message order. The messages are pipelined (no reply awaited in
    /// between) on a single-threaded runtime: a spawned Set handler would
    /// run only after the keys queued behind it.
    #[tokio::test(flavor = "current_thread")]
    async fn content_type_and_keys_apply_in_message_order() {
        let dir = tempfile::tempdir().unwrap();
        let trace = dir.path().join("trace.log");
        let (_server, client) = p2p_engine(&trace, None).await;

        client.send(&set_content_type_msg(8, 0)).await.unwrap(); // PASSWORD
        for c in "hunter2 ".chars() {
            client.send(&key_msg(u32::from(c))).await.unwrap();
        }
        client.send(&set_content_type_msg(0, 0)).await.unwrap(); // FREE_FORM
        client.send(&key_msg(u32::from('k'))).await.unwrap();
        // A call we wait for: everything before it has been dispatched.
        let reply = client
            .call_method(
                None::<&str>,
                TEST_PATH,
                Some(ENGINE_IFACE),
                "ProcessKeyEvent",
                &(u32::from('z'), 0u32, 0u32),
            )
            .await
            .unwrap();
        assert!(!reply.body().deserialize::<bool>().unwrap());

        let want = [
            "ContentType purpose=0 hints=0",
            "key Printable",
            "key Printable",
        ];
        for _ in 0..200 {
            if trace_bodies(&trace).len() >= want.len() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        // Let any late (out-of-order) handler land before judging.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(trace_bodies(&trace), want);
    }

    /// What ibus-daemon reads back: plain `b` for FocusId and
    /// ActiveSurroundingText, `(uu)` for ContentType.
    #[tokio::test(flavor = "current_thread")]
    async fn engine_properties_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let (_server, client) = p2p_engine(&dir.path().join("t.log"), None).await;
        let get = |name: &'static str| {
            let client = client.clone();
            async move {
                let reply = client
                    .call_method(
                        None::<&str>,
                        TEST_PATH,
                        Some(PROPS_IFACE),
                        "Get",
                        &(ENGINE_IFACE, name),
                    )
                    .await
                    .unwrap();
                let v: zbus::zvariant::OwnedValue = reply.body().deserialize().unwrap();
                v
            }
        };
        assert!(!bool::try_from(get("FocusId").await).unwrap());
        assert!(bool::try_from(get("ActiveSurroundingText").await).unwrap());
        client.send(&set_content_type_msg(5, 2)).await.unwrap();
        let ct = get("ContentType").await;
        assert_eq!(<(u32, u32)>::try_from(ct).unwrap(), (5, 2));
    }

    #[test]
    fn pending_cursor_moves_are_capped_at_two() {
        let mut e = engine();
        for c in "hello".chars() {
            key(&mut e, u32::from(c), 0);
        }
        assert_eq!(e.pending_moves, 2);
        e.set_cursor_location(1, 0, 1, 10);
        e.set_cursor_location(2, 0, 1, 10);
        e.set_surrounding_text(ibus_text("hello"), 5, 5);
        // The third update is no longer absorbed: a click shows up here.
        e.set_cursor_location(40, 0, 1, 10);
        assert_eq!(e.surrounding, None);
    }

    #[test]
    fn invalidate_key_clears_pending_moves_and_surrounding() {
        let mut e = engine();
        key(&mut e, u32::from('h'), 0);
        e.set_surrounding_text(ibus_text("h"), 1, 1);
        assert_eq!(e.pending_moves, 1);
        key(&mut e, 0xff51, 0); // Left arrow
        assert_eq!(e.pending_moves, 0);
        assert_eq!(e.surrounding, None);
    }

    #[test]
    fn off_key_zeroes_pending_moves() {
        let mut e = silent_engine();
        key(&mut e, u32::from('h'), 0);
        e.set_surrounding_text(ibus_text("h"), 1, 1);
        assert_eq!(e.pending_moves, 1);
        // The control plane turns correction off between two keys.
        e.shared.control.set_enabled(false);
        assert_eq!(key(&mut e, u32::from('i'), 0), Action::Pass);
        assert_eq!(e.pending_moves, 0);
        assert_eq!(e.surrounding, None);
        // A content type that resolves to Off does the same.
        e.shared.control.set_enabled(true);
        key(&mut e, u32::from('h'), 0);
        assert_eq!(e.pending_moves, 1);
        e.set_content_type((8, 0));
        assert_eq!(e.pending_moves, 0);
    }

    #[test]
    fn mode_uses_control_plane_app() {
        let mut e = engine();
        e.set_capabilities(CAPS_SURROUNDING);
        assert_eq!(e.mode(), Mode::Observe);
        e.shared.control.set_focused_app("e2e");
        assert_eq!(e.mode(), Mode::Silent);
        e.set_content_type((8, 0)); // PASSWORD
        assert_eq!(e.mode(), Mode::Off);
        e.set_content_type((0, 0));
        e.shared.control.set_focused_app("");
        assert_eq!(e.mode(), Mode::Observe);
    }

    fn type_str(e: &mut Engine, s: &str) -> Vec<Action> {
        s.chars().map(|c| key(e, u32::from(c), 0)).collect()
    }

    #[test]
    fn correction_predicts_surrounding_and_reverts() {
        let mut e = silent_engine();
        for (i, c) in "bulid".chars().enumerate() {
            assert_eq!(key(&mut e, u32::from(c), 0), Action::Pass);
            let typed: String = "bulid".chars().take(i + 1).collect();
            e.set_surrounding_text(ibus_text(&typed), i as u32 + 1, i as u32 + 1);
        }
        let a = key(&mut e, u32::from(' '), 0);
        assert_eq!(
            a,
            Action::Replace {
                delete_chars: 5,
                commit: "build ".into(),
                consume: true
            }
        );
        // Our own delete + commit move the cursor; the client has not sent
        // the new text yet, so the engine predicts it.
        assert_eq!(e.pending_moves, MAX_PENDING_MOVES);
        assert_eq!(e.surrounding.as_deref(), Some("build "));
        // An immediate Backspace reverts, verified against the prediction.
        let a = key(&mut e, 0xff08, 0);
        assert_eq!(
            a,
            Action::Replace {
                delete_chars: 6,
                commit: "bulid ".into(),
                consume: true
            }
        );
        assert_eq!(e.surrounding.as_deref(), Some("bulid "));
    }

    #[test]
    fn no_correction_without_app_or_in_password() {
        let mut e = engine();
        assert!(type_str(&mut e, "bulid ")
            .iter()
            .all(|a| *a == Action::Pass));
        let mut e = silent_engine();
        e.set_content_type((8, 0));
        assert!(type_str(&mut e, "bulid ")
            .iter()
            .all(|a| *a == Action::Pass));
    }

    /// One message the client saw: a signal (member, body) or a reply.
    #[derive(Debug, PartialEq)]
    enum Seen {
        Delete(i32, u32),
        Commit(String),
        Reply(bool),
    }

    /// Feeds `keys` (awaiting each reply) and returns what came back for
    /// the last one, in wire order.
    async fn feed(client: &Connection, keys: &str, last: u32) -> Vec<Seen> {
        use zbus::message::Type;
        for c in keys.chars() {
            let r = client
                .call_method(
                    None::<&str>,
                    TEST_PATH,
                    Some(ENGINE_IFACE),
                    "ProcessKeyEvent",
                    &(u32::from(c), 0u32, 0u32),
                )
                .await
                .unwrap();
            assert!(!r.body().deserialize::<bool>().unwrap());
        }
        // Subscribed only now: earlier replies are not in this stream.
        let mut stream = std::pin::pin!(zbus::MessageStream::from(client));
        client.send(&key_msg(last)).await.unwrap();
        let mut seen = Vec::new();
        loop {
            let next = std::future::poll_fn(|cx| {
                zbus::export::futures_core::Stream::poll_next(stream.as_mut(), cx)
            });
            let m = tokio::time::timeout(std::time::Duration::from_secs(5), next)
                .await
                .expect("reply within 5 s")
                .unwrap()
                .unwrap();
            let h = m.header();
            match (m.message_type(), h.member().map(|n| n.as_str())) {
                (Type::Signal, Some("DeleteSurroundingText")) => {
                    let (o, n): (i32, u32) = m.body().deserialize().unwrap();
                    seen.push(Seen::Delete(o, n));
                }
                (Type::Signal, Some("CommitText")) => {
                    let body = m.body();
                    let v: Value<'_> = body.deserialize().unwrap();
                    seen.push(Seen::Commit(text_of(&v).unwrap()));
                }
                (Type::MethodReturn, _) => {
                    let b: bool = m.body().deserialize().unwrap();
                    seen.push(Seen::Reply(b));
                    return seen;
                }
                _ => {}
            }
        }
    }

    async fn set_caps(client: &Connection, caps: u32) {
        client
            .call_method(
                None::<&str>,
                TEST_PATH,
                Some(ENGINE_IFACE),
                "SetCapabilities",
                &(caps,),
            )
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replace_deletes_then_commits_then_consumes() {
        let dir = tempfile::tempdir().unwrap();
        let (_server, client) = p2p_engine(&dir.path().join("t.log"), Some("e2e")).await;
        set_caps(&client, CAPS_SURROUNDING).await;
        let seen = feed(&client, "bulid", u32::from(' ')).await;
        assert_eq!(
            seen,
            [
                Seen::Delete(-5, 5),
                Seen::Commit("build ".into()),
                Seen::Reply(true)
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn return_boundary_commits_and_passes_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let (_server, client) = p2p_engine(&dir.path().join("t.log"), Some("e2e")).await;
        set_caps(&client, CAPS_SURROUNDING).await;
        let seen = feed(&client, "bulid", 0xff0d).await;
        assert_eq!(
            seen,
            [
                Seen::Delete(-5, 5),
                Seen::Commit("build".into()),
                Seen::Reply(false)
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unidentified_app_emits_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (_server, client) = p2p_engine(&dir.path().join("t.log"), None).await;
        set_caps(&client, CAPS_SURROUNDING).await;
        assert_eq!(
            feed(&client, "bulid", u32::from(' ')).await,
            [Seen::Reply(false)]
        );
    }

    /// A client without IBUS_CAP_SURROUNDING_TEXT cannot apply
    /// DeleteSurroundingText: it would end up with "bulidbuild" and lose
    /// the space. Nothing is emitted and the key passes; with the bit set
    /// the same keys are corrected.
    #[tokio::test(flavor = "current_thread")]
    async fn no_surrounding_capability_never_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let (_server, client) = p2p_engine(&dir.path().join("t.log"), Some("e2e")).await;
        set_caps(&client, CAPS_NO_SURROUNDING).await;
        assert_eq!(
            feed(&client, "bulid", u32::from(' ')).await,
            [Seen::Reply(false)]
        );
        set_caps(&client, CAPS_SURROUNDING).await;
        assert_eq!(
            feed(&client, "bulid", u32::from(' ')).await,
            [
                Seen::Delete(-5, 5),
                Seen::Commit("build ".into()),
                Seen::Reply(true)
            ]
        );
    }

    #[test]
    fn no_surrounding_capability_observes_or_is_off() {
        let mut e = silent_engine();
        e.set_capabilities(CAPS_NO_SURROUNDING);
        // Silent observes (learns as Observe does, never corrects)...
        assert_eq!(e.mode(), Mode::Observe);
        // ...and a terminal, which never learns, is Off rather than
        // Observe: nothing typed there may be learned.
        e.set_content_type((10, 0)); // TERMINAL in an allowlisted app
        assert_eq!(e.mode(), Mode::Off);
        e.set_capabilities(CAPS_SURROUNDING);
        assert_eq!(e.mode(), Mode::ProseAware);
        // The capability is only ever a downgrade.
        e.set_content_type((8, 0));
        assert_eq!(e.mode(), Mode::Off);
    }

    #[test]
    fn losing_surrounding_capability_closes_the_revert_window() {
        let mut e = silent_engine();
        assert!(matches!(
            type_str(&mut e, "bulid ").last(),
            Some(Action::Replace { .. })
        ));
        e.set_capabilities(CAPS_NO_SURROUNDING);
        // No revert either: that, too, would be a Replace.
        assert_eq!(key(&mut e, 0xff08, 0), Action::Pass);
    }

    /// Opening Quick Settings moves focus into the shell: the menu must
    /// still describe the field the user was typing in.
    #[test]
    fn focus_out_keeps_last_field() {
        let mut e = engine();
        e.shared.control.set_focused_app("e2e");
        assert_eq!(e.shared.control.app_state(), None);
        e.caps = CAPS_SURROUNDING;
        e.focus_in();
        let silent = Some((Mode::Silent, Reason::Allowlisted));
        assert_eq!(e.shared.control.app_state(), silent);
        e.focus_out();
        e.focus_out_id(String::new());
        assert_eq!(e.shared.control.app_state(), silent);
        // FocusInId records the field too.
        e.set_content_type((8, 0));
        e.focus_in_id(String::new(), String::new());
        assert_eq!(
            e.shared.control.app_state(),
            Some((Mode::Off, Reason::PrivateField))
        );
    }

    /// The reported state follows the engine's own effective mode.
    #[test]
    fn set_capabilities_without_surrounding_gives_no_replace() {
        let mut e = silent_engine();
        let state = |e: &Engine| e.shared.control.app_state().unwrap();
        assert_eq!(state(&e), (Mode::Silent, Reason::Allowlisted));
        e.set_capabilities(CAPS_NO_SURROUNDING);
        assert_eq!(state(&e), (Mode::Observe, Reason::NoReplace));
        assert_eq!(state(&e).0, e.mode());
        e.set_content_type((10, 0)); // TERMINAL in an allowlisted app
        assert_eq!(state(&e), (Mode::Off, Reason::NoReplace));
        assert_eq!(state(&e).0, e.mode());
        e.set_capabilities(CAPS_SURROUNDING);
        assert_eq!(state(&e), (Mode::ProseAware, Reason::Allowlisted));
        assert_eq!(state(&e).0, e.mode());
    }

    /// SetLearning(false) reaches the next key: nothing typed is recorded.
    #[test]
    fn paused_learning_records_nothing() {
        let mut e = silent_engine();
        e.shared.control.set_learning(false);
        type_str(&mut e, "kadto kadto ");
        assert_eq!(e.shared.store.lock().unwrap().personal_freq("kadto"), 0);
        e.shared.control.set_learning(true);
        type_str(&mut e, "kadto kadto ");
        assert_eq!(e.shared.store.lock().unwrap().personal_freq("kadto"), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn guarded_turns_a_panicking_future_into_none() {
        assert_eq!(guarded(async { 7 }).await, Some(7));
        let panics = async {
            if std::hint::black_box(true) {
                panic!("emit failed");
            }
            7
        };
        assert_eq!(guarded(panics).await, None);
    }
}
