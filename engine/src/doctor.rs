//! `engine doctor [--json]`: read-only diagnostics (spec 10.3).
//!
//! It never opens the keyring or the database, never prompts and makes no
//! network call. Values are paths (with `~` for `$HOME`), ids, versions and
//! counts only: no config contents, typed words or learned words. D-Bus
//! presence uses `NameHasOwner`; the two method calls carry `NoAutoStart`
//! and a 2 s timeout, so a missing service is never started.

use crate::config::{self, Config};
use serde::Serialize;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;
use zbus::proxy::{CacheProperties, MethodFlags};
use zbus::zvariant::{OwnedValue, Value};

const DOCTOR_USAGE: &str = "usage: correctiond doctor [--json]";
const EXIT_USAGE: u8 = 64;
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

const COMPANION_UUID: &str = "correctiond-companion@nadelacruz-00.github.io";
const GJS_OSK_UUID: &str = "gjsosk@vishram1123.com";
const SYSTEM_COMPONENT_DIR: &str = "/usr/share/ibus/component";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub key: &'static str,
    pub status: Status,
    pub value: String,
}

fn check(key: &'static str, status: Status, value: impl Into<String>) -> Check {
    Check {
        key,
        status,
        value: value.into(),
    }
}

/// What `GetExtensionInfo` told about one extension.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtInfo {
    pub state: f64,
    pub version: String,
}

fn exit_code(checks: &[Check]) -> u8 {
    match checks.iter().map(|c| c.status).max() {
        None | Some(Status::Ok) => 0,
        Some(Status::Warn) => 1,
        Some(Status::Fail) => 2,
    }
}

fn render_text(checks: &[Check]) -> String {
    let mut out = String::new();
    for c in checks {
        out.push_str(&format!("{}: {} {}\n", c.key, c.status.as_str(), c.value));
    }
    out
}

#[derive(Serialize)]
struct JsonCheck<'a> {
    key: &'a str,
    status: &'a str,
    value: &'a str,
}

#[derive(Serialize)]
struct JsonOut<'a> {
    checks: Vec<JsonCheck<'a>>,
}

fn render_json(checks: &[Check]) -> String {
    let out = JsonOut {
        checks: checks
            .iter()
            .map(|c| JsonCheck {
                key: c.key,
                status: c.status.as_str(),
                value: &c.value,
            })
            .collect(),
    };
    serde_json::to_string(&out).unwrap_or_else(|_| String::from("{\"checks\":[]}"))
}

/// `~` in place of `home`, only for a whole leading path component.
fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn component_dirs(env: Option<&OsStr>) -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from(SYSTEM_COMPONENT_DIR)];
    if let Some(e) = env {
        dirs.extend(
            std::env::split_paths(e)
                .filter(|p| !p.as_os_str().is_empty())
                .collect::<Vec<_>>(),
        );
    }
    dirs
}

/// GLib's defaults: the user data dir, then `XDG_DATA_DIRS`.
fn data_dirs(
    xdg_data_home: Option<&OsStr>,
    xdg_data_dirs: Option<&OsStr>,
    home: &Path,
) -> Vec<PathBuf> {
    let mut dirs = vec![match xdg_data_home.filter(|s| !s.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => home.join(".local/share"),
    }];
    let system = xdg_data_dirs
        .filter(|s| !s.is_empty())
        .map(OsStr::to_os_string)
        .unwrap_or_else(|| OsString::from("/usr/local/share:/usr/share"));
    dirs.extend(std::env::split_paths(&system).filter(|p| !p.as_os_str().is_empty()));
    dirs
}

fn gjs_osk_installed(dirs: &[PathBuf]) -> bool {
    dirs.iter()
        .any(|d| d.join("gnome-shell/extensions").join(GJS_OSK_UUID).is_dir())
}

/// The text of the first `<exec>` element.
fn exec_of(xml: &str) -> Option<&str> {
    let start = xml.find("<exec>")? + "<exec>".len();
    let len = xml[start..].find("</exec>")?;
    Some(xml[start..start + len].trim())
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn check_component(dirs: &[PathBuf], exe: &Path, home: &Path) -> Check {
    let exe = canon(exe);
    let mut found: Option<PathBuf> = None;
    for d in dirs {
        let file = d.join("correctiond.xml");
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Some(exec) = exec_of(&text) else {
            found.get_or_insert(file);
            continue;
        };
        if canon(Path::new(exec)) == exe {
            return check("component", Status::Ok, tilde(&file, home));
        }
        found.get_or_insert(file);
    }
    match found {
        Some(f) => check(
            "component",
            Status::Warn,
            format!("{} exec differs from this binary", tilde(&f, home)),
        ),
        None => check(
            "component",
            Status::Fail,
            format!(
                "correctiond.xml not found in {}",
                dirs.iter()
                    .map(|d| tilde(d, home))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
    }
}

fn check_config(path: &Path, home: &Path) -> (Check, Option<Config>) {
    let shown = tilde(path, home);
    let cfg = match std::fs::read_to_string(path) {
        Ok(text) => match config::parse_report(&text, home).0 {
            Ok(c) => c,
            Err(e) => {
                return (
                    check(
                        "config",
                        Status::Fail,
                        format!("{shown} invalid at line {}, column {}", e.line, e.column),
                    ),
                    None,
                )
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default_for(home),
        Err(_) => {
            return (
                check("config", Status::Fail, format!("{shown} unreadable")),
                None,
            )
        }
    };
    let mut status = Status::Ok;
    let langs: Vec<String> = cfg
        .languages
        .iter()
        .map(|l| {
            let state = if l.lexicon.starts_with("builtin:") {
                "builtin"
            } else if Path::new(&config::expand_tilde(&l.lexicon, home)).is_file() {
                "lexicon present"
            } else {
                status = Status::Warn;
                "lexicon missing"
            };
            format!("{} ({state})", l.code)
        })
        .collect();
    let value = format!(
        "{shown} ok; allowlist {}; languages {}",
        cfg.allowlist.len(),
        langs.join(", ")
    );
    (check("config", status, value), Some(cfg))
}

fn check_store(path: &Path, home: &Path) -> Check {
    let shown = tilde(path, home);
    match std::fs::metadata(path) {
        Ok(m) => check("store", Status::Ok, format!("{shown} {} bytes", m.len())),
        Err(_) => check("store", Status::Warn, format!("{shown} absent")),
    }
}

fn check_touch(cfg: Option<&Config>, dirs: &[PathBuf]) -> (Check, bool) {
    let (kind, on) = match cfg.and_then(|c| c.modules.touch) {
        Some(b) => ("explicit", b),
        None => ("derived", gjs_osk_installed(dirs)),
    };
    (
        check("modules.touch", Status::Ok, format!("{kind} {on}")),
        on,
    )
}

fn ext_status(state: f64) -> Status {
    if state == 1.0 {
        Status::Ok
    } else if state == 3.0 || state == 4.0 {
        Status::Fail
    } else {
        Status::Warn
    }
}

fn check_extension(
    uuid_info: Option<ExtInfo>,
    gjs_info: Option<ExtInfo>,
    effective_touch: bool,
) -> Check {
    let mut status = Status::Ok;
    let mut parts = Vec::new();
    for (uuid, info, absent_ok) in [
        (COMPANION_UUID, uuid_info, false),
        (GJS_OSK_UUID, gjs_info, !effective_touch),
    ] {
        match info {
            Some(i) => {
                status = status.max(ext_status(i.state));
                parts.push(format!("{uuid} {} {}", i.state as i64, i.version));
            }
            None => {
                if !absent_ok {
                    status = status.max(Status::Warn);
                }
                parts.push(format!("{uuid} absent"));
            }
        }
    }
    check("extension", status, parts.join("; "))
}

/// Number, or text, as the Shell wrote the version.
fn version_text(v: &OwnedValue) -> String {
    let v: &Value<'_> = v;
    match v {
        Value::Str(s) => s.to_string(),
        Value::F64(f) => format!("{}", *f as i64),
        Value::U32(n) => n.to_string(),
        Value::I32(n) => n.to_string(),
        Value::U64(n) => n.to_string(),
        Value::I64(n) => n.to_string(),
        _ => "?".into(),
    }
}

fn parse_ext(map: &HashMap<String, OwnedValue>) -> Option<ExtInfo> {
    // An empty map means absent. The `error` key is never looked at.
    let state = match &**map.get("state")? {
        Value::F64(f) => *f,
        Value::U32(n) => f64::from(*n),
        Value::I32(n) => f64::from(*n),
        _ => return None,
    };
    let version = map.get("version").map_or_else(|| "?".into(), version_text);
    Some(ExtInfo { state, version })
}

struct BusFacts {
    ibus: Check,
    running: Check,
    companion: Option<ExtInfo>,
    gjs: Option<ExtInfo>,
}

async fn has_owner(conn: &zbus::Connection, name: &str) -> Option<bool> {
    let body = (name,);
    let call = conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "NameHasOwner",
        &body,
    );
    let reply = tokio::time::timeout(CALL_TIMEOUT, call).await.ok()?.ok()?;
    reply.body().deserialize::<bool>().ok()
}

async fn call_noautostart<R>(
    conn: &zbus::Connection,
    dest: &str,
    path: &str,
    iface: &str,
    method: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> Option<R>
where
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    let proxy = zbus::proxy::Builder::<zbus::Proxy>::new(conn)
        .destination(dest.to_owned())
        .ok()?
        .path(path.to_owned())
        .ok()?
        .interface(iface.to_owned())
        .ok()?
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .ok()?;
    let call = proxy.call_with_flags::<_, _, R>(method, MethodFlags::NoAutoStart.into(), body);
    tokio::time::timeout(CALL_TIMEOUT, call).await.ok()?.ok()?
}

fn stats_value(json: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return "answered".into();
    };
    let n = |k: &str| v.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
    format!(
        "corrections {}, reverts {}, words_learned {}",
        n("corrections"),
        n("reverts"),
        n("words_learned")
    )
}

async fn extension_info(conn: &zbus::Connection, uuid: &str) -> Option<ExtInfo> {
    let map: HashMap<String, OwnedValue> = call_noautostart(
        conn,
        "org.gnome.Shell",
        "/org/gnome/Shell",
        "org.gnome.Shell.Extensions",
        "GetExtensionInfo",
        &(uuid,),
    )
    .await?;
    parse_ext(&map)
}

async fn bus_facts() -> BusFacts {
    let conn = match tokio::time::timeout(CALL_TIMEOUT, zbus::Connection::session()).await {
        Ok(Ok(c)) => c,
        _ => {
            return BusFacts {
                ibus: check("ibus", Status::Fail, "session bus not reachable"),
                running: check("engine_running", Status::Warn, "session bus not reachable"),
                companion: None,
                gjs: None,
            }
        }
    };
    let ibus = match has_owner(&conn, "org.freedesktop.IBus").await {
        Some(true) => check("ibus", Status::Ok, "org.freedesktop.IBus has an owner"),
        _ => check("ibus", Status::Fail, "org.freedesktop.IBus has no owner"),
    };
    let running = if has_owner(&conn, crate::control::CONTROL_NAME).await == Some(true) {
        let stats: Option<String> = call_noautostart(
            &conn,
            crate::control::CONTROL_NAME,
            crate::control::CONTROL_PATH,
            crate::control::CONTROL_NAME,
            "GetStats",
            &(),
        )
        .await;
        match stats {
            Some(s) => check("engine_running", Status::Ok, stats_value(&s)),
            None => check(
                "engine_running",
                Status::Warn,
                "name is owned but GetStats did not answer",
            ),
        }
    } else {
        check("engine_running", Status::Warn, "not running")
    };
    BusFacts {
        ibus,
        running,
        companion: extension_info(&conn, COMPANION_UUID).await,
        gjs: extension_info(&conn, GJS_OSK_UUID).await,
    }
}

fn check_engine(home: &Path) -> Check {
    let version = env!("CARGO_PKG_VERSION");
    match std::env::current_exe() {
        Ok(p) => check(
            "engine",
            Status::Ok,
            format!("{} {version}", tilde(&canon(&p), home)),
        ),
        Err(_) => check("engine", Status::Warn, format!("unknown path {version}")),
    }
}

/// `Ok(true)` for `--json`.
fn parse_args(args: &[OsString]) -> Result<bool, ()> {
    match args {
        [] => Ok(false),
        [a] if a == "--json" => Ok(true),
        _ => Err(()),
    }
}

pub fn run(args: &[OsString]) -> ExitCode {
    let Ok(json) = parse_args(args) else {
        eprintln!("{DOCTOR_USAGE}");
        return ExitCode::from(EXIT_USAGE);
    };
    let Some(home) = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
    else {
        eprintln!("correctiond: HOME is not set");
        return ExitCode::from(2);
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        eprintln!("correctiond: cannot start runtime");
        return ExitCode::from(2);
    };
    let facts = rt.block_on(bus_facts());

    let env_data_home = std::env::var_os("XDG_DATA_HOME");
    let env_data_dirs = std::env::var_os("XDG_DATA_DIRS");
    let dirs = data_dirs(env_data_home.as_deref(), env_data_dirs.as_deref(), &home);
    let exe = std::env::current_exe().unwrap_or_default();
    let (config_check, cfg) = check_config(&Config::path(&home), &home);
    let (touch_check, effective) = check_touch(cfg.as_ref(), &dirs);
    let checks = vec![
        check_engine(&home),
        check_component(
            &component_dirs(std::env::var_os("IBUS_COMPONENT_PATH").as_deref()),
            &exe,
            &home,
        ),
        facts.ibus,
        facts.running,
        check_extension(facts.companion, facts.gjs, effective),
        config_check,
        check_store(&home.join(".local/share/correctiond/learn.db"), &home),
        touch_check,
    ];
    if json {
        println!("{}", render_json(&checks));
    } else {
        print!("{}", render_text(&checks));
    }
    ExitCode::from(exit_code(&checks))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(key: &'static str, status: Status, value: &str) -> Check {
        check(key, status, value)
    }

    fn info(state: f64) -> Option<ExtInfo> {
        Some(ExtInfo {
            state,
            version: "1".into(),
        })
    }

    #[test]
    fn exit_code_is_worst_status() {
        assert_eq!(exit_code(&[c("a", Status::Ok, "")]), 0);
        assert_eq!(
            exit_code(&[c("a", Status::Ok, ""), c("b", Status::Warn, "")]),
            1
        );
        assert_eq!(
            exit_code(&[c("a", Status::Warn, ""), c("b", Status::Fail, "")]),
            2
        );
    }

    #[test]
    fn json_shape() {
        let checks = [c("a", Status::Ok, "x"), c("b", Status::Warn, "y")];
        assert_eq!(
            render_json(&checks),
            r#"{"checks":[{"key":"a","status":"ok","value":"x"},{"key":"b","status":"warn","value":"y"}]}"#
        );
    }

    #[test]
    fn text_shape() {
        let checks = [c("a", Status::Ok, "x"), c("b", Status::Warn, "y")];
        assert_eq!(render_text(&checks), "a: ok x\nb: warn y\n");
    }

    #[test]
    fn tilde_replaces_only_whole_home_component() {
        let home = Path::new("/home/u");
        assert_eq!(tilde(Path::new("/home/u/x"), home), "~/x");
        assert_eq!(tilde(Path::new("/home/u2/x"), home), "/home/u2/x");
        assert_eq!(tilde(Path::new("/home/u"), home), "~");
    }

    #[test]
    fn component_dirs_skip_empty_entries() {
        assert_eq!(
            component_dirs(Some(OsStr::new(":/opt/x:"))),
            vec![
                PathBuf::from("/usr/share/ibus/component"),
                PathBuf::from("/opt/x")
            ]
        );
        assert_eq!(component_dirs(None).len(), 1);
    }

    #[test]
    fn data_dirs_use_glib_defaults() {
        let home = Path::new("/home/u");
        assert_eq!(
            data_dirs(None, None, home),
            vec![
                PathBuf::from("/home/u/.local/share"),
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share")
            ]
        );
        assert_eq!(
            data_dirs(Some(OsStr::new("/d")), Some(OsStr::new(":/s:")), home),
            vec![PathBuf::from("/d"), PathBuf::from("/s")]
        );
    }

    fn write_component(dir: &Path, exec: &Path) {
        std::fs::write(
            dir.join("correctiond.xml"),
            format!("<component><exec>{}</exec></component>", exec.display()),
        )
        .unwrap();
    }

    #[test]
    fn component_exec_must_match() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        let exe = t.path().join("engine");
        std::fs::write(&exe, "").unwrap();
        let dir = t.path().join("component");
        std::fs::create_dir(&dir).unwrap();
        let dirs = [dir.clone()];
        assert_eq!(check_component(&dirs, &exe, home).status, Status::Fail);
        write_component(&dir, &t.path().join("other"));
        assert_eq!(check_component(&dirs, &exe, home).status, Status::Warn);
        write_component(&dir, &exe);
        let ok = check_component(&dirs, &exe, home);
        assert_eq!(ok.status, Status::Ok);
        assert_eq!(ok.value, "~/component/correctiond.xml");
    }

    #[test]
    fn gjs_osk_dir_rule() {
        let t = tempfile::tempdir().unwrap();
        let dirs = [t.path().to_path_buf()];
        assert!(!gjs_osk_installed(&dirs));
        std::fs::create_dir_all(
            t.path()
                .join("gnome-shell/extensions/gjsosk@vishram1123.com"),
        )
        .unwrap();
        assert!(gjs_osk_installed(&dirs));
    }

    #[test]
    fn touch_explicit_and_derived() {
        let t = tempfile::tempdir().unwrap();
        let dirs = [t.path().to_path_buf()];
        let mut cfg = Config::default();
        cfg.modules.touch = Some(false);
        std::fs::create_dir_all(
            t.path()
                .join("gnome-shell/extensions/gjsosk@vishram1123.com"),
        )
        .unwrap();
        let (chk, on) = check_touch(Some(&cfg), &dirs);
        assert_eq!((chk.value.as_str(), on), ("explicit false", false));
        let (chk, on) = check_touch(None, &dirs);
        assert_eq!((chk.value.as_str(), on), ("derived true", true));
    }

    #[test]
    fn missing_gjs_osk_ok_when_touch_off() {
        let chk = check_extension(info(1.0), None, false);
        assert_eq!(chk.status, Status::Ok);
        let chk = check_extension(info(1.0), None, true);
        assert_eq!(chk.status, Status::Warn);
        assert_eq!(check_extension(None, None, false).status, Status::Warn);
    }

    #[test]
    fn extension_error_state_fails() {
        assert_eq!(check_extension(info(3.0), None, false).status, Status::Fail);
        assert_eq!(check_extension(info(4.0), None, false).status, Status::Fail);
        assert_eq!(check_extension(info(2.0), None, false).status, Status::Warn);
    }

    #[test]
    fn config_invalid_reports_position_not_contents() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("config.toml");
        std::fs::write(&p, "broken = 3\n[[languages]\n").unwrap();
        let (chk, cfg) = check_config(&p, t.path());
        assert_eq!(chk.status, Status::Fail);
        assert!(cfg.is_none());
        assert!(chk.value.contains("invalid at line"), "{}", chk.value);
        assert!(!chk.value.contains("broken"));
    }

    #[test]
    fn config_reports_lexicon_presence() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("config.toml");
        std::fs::write(
            &p,
            "allowlist = [\"a\"]\n[[languages]]\ncode = \"en\"\nlexicon = \"builtin:en\"\n\
             [[languages]]\ncode = \"tl\"\nlexicon = \"~/nope.txt\"\n",
        )
        .unwrap();
        let (chk, cfg) = check_config(&p, t.path());
        assert!(cfg.is_some());
        assert_eq!(chk.status, Status::Warn);
        assert!(chk.value.contains("allowlist 1"));
        assert!(chk.value.contains("tl (lexicon missing)"));
        assert!(chk.value.contains("en (builtin)"));
    }

    #[test]
    fn store_is_stat_only() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("learn.db");
        assert_eq!(check_store(&p, t.path()).status, Status::Warn);
        std::fs::write(&p, "12345").unwrap();
        let chk = check_store(&p, t.path());
        assert_eq!(
            (chk.status, chk.value.as_str()),
            (Status::Ok, "~/learn.db 5 bytes")
        );
    }

    #[test]
    fn unknown_flag_is_usage() {
        assert_eq!(parse_args(&[]), Ok(false));
        assert_eq!(parse_args(&["--json".into()]), Ok(true));
        assert_eq!(parse_args(&["--bogus".into()]), Err(()));
        assert_eq!(parse_args(&["--json".into(), "x".into()]), Err(()));
    }
}
