//! correctiond IBus engine process.
//!
//! ibus-daemon execs this binary (no arguments) for the installed component
//! and expects it to own `org.freedesktop.IBus.Correctiond`. With
//! `--register` it also registers the component itself (dev loop and E2E
//! harness, no installed XML needed). It also serves the control plane
//! `org.nadelacruz.Correctiond1` on the session bus (`control.rs`).
//!
//! `engine import FILE` is a client mode instead: it passes FILE (a
//! "correctiond vocab v1" file, docs/vocab-format.md) to the running
//! engine's `ImportVocabulary` and prints the counts or a fixed error.

mod config;
mod control;
mod ibus;
mod keyring;
mod persist;
mod trace;

use config::Config;
use corelib::learning::LearnStore;
use engine::{build_dictionary, Counters};
use ibus::{Factory, Shared, BUS_NAME, FACTORY_PATH};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use trace::Trace;
use zbus::export::futures_core::Stream;

/// Dictionary builds slower than this get a follow-up (docs/designs/notes.md).
const SLOW_BUILD_MS: u64 = 2000;

/// How long the exit path waits for the final write-behind flush.
const EXIT_FLUSH: Duration = Duration::from_secs(2);

/// Crash reports and ptrace must not capture keystroke buffers or keys.
fn harden() {
    // SAFETY: plain syscalls with constant arguments and a valid pointer.
    unsafe {
        if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 {
            eprintln!("correctiond: PR_SET_DUMPABLE failed");
        }
        let none = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::setrlimit(libc::RLIMIT_CORE, &none) != 0 {
            eprintln!("correctiond: RLIMIT_CORE failed");
        }
    }
}

/// Panic messages can quote the data being processed (a bad string slice
/// prints the string), so report only where it happened.
fn quiet_panics() {
    std::panic::set_hook(Box::new(|info| match info.location() {
        Some(l) => eprintln!("correctiond: internal error at {}:{}", l.file(), l.line()),
        None => eprintln!("correctiond: internal error"),
    }));
}

/// Printed at startup when `allow_any_caller` is set.
const ANY_CALLER_WARNING: &str = "correctiond: WARNING: allow_any_caller is on; \
    any session-bus client can call the control interface";

const IMPORT_USAGE: &str = "usage: engine import FILE";
const IMPORT_CANNOT_OPEN: &str = "cannot open the file";
const IMPORT_NO_BUS: &str = "cannot reach the session bus";
const IMPORT_NOT_RUNNING: &str = "correctiond is not running";
const IMPORT_NO_ANSWER: &str = "correctiond did not answer";
const IMPORT_GENERIC: &str = "import failed";
/// A 200,000-record import into SQLCipher takes seconds, not minutes.
const IMPORT_TIMEOUT: Duration = Duration::from_secs(120);

/// The text to print for a failed `ImportVocabulary` call: the engine's own
/// fixed texts pass through, anything else becomes a generic one, so the
/// client never prints what some other bus peer sent.
fn import_error_line(name: &str, desc: Option<&str>) -> &'static str {
    match name {
        "org.nadelacruz.Correctiond1.Error.ImportFailed" => {
            control::import_error_text(desc.unwrap_or_default())
        }
        "org.freedesktop.DBus.Error.ServiceUnknown"
        | "org.freedesktop.DBus.Error.NameHasNoOwner"
        | "org.freedesktop.DBus.Error.UnknownObject"
        | "org.freedesktop.DBus.Error.UnknownMethod" => IMPORT_NOT_RUNNING,
        "org.freedesktop.DBus.Error.NoReply" | "org.freedesktop.DBus.Error.Timeout" => {
            IMPORT_NO_ANSWER
        }
        _ => IMPORT_GENERIC,
    }
}

fn import_ok_line(words: u32, pairs: u32) -> String {
    format!("Imported {words} words and {pairs} word pairs.")
}

/// `engine import FILE`. Prints only the counts line or a fixed error,
/// never file content or the path.
fn import_main(args: &[std::ffi::OsString]) -> ExitCode {
    let [path] = args else {
        eprintln!("{IMPORT_USAGE}");
        return ExitCode::FAILURE;
    };
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => {
            eprintln!("{IMPORT_CANNOT_OPEN}");
            return ExitCode::FAILURE;
        }
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        eprintln!("{IMPORT_GENERIC}");
        return ExitCode::FAILURE;
    };
    match rt.block_on(import_call(&file)) {
        Ok((words, pairs)) => {
            println!("{}", import_ok_line(words, pairs));
            ExitCode::SUCCESS
        }
        Err(line) => {
            eprintln!("{line}");
            ExitCode::FAILURE
        }
    }
}

async fn import_call(file: &std::fs::File) -> Result<(u32, u32), &'static str> {
    let conn = zbus::connection::Builder::session()
        .map_err(|_| IMPORT_NO_BUS)?
        .build()
        .await
        .map_err(|_| IMPORT_NO_BUS)?;
    let body = (zbus::zvariant::Fd::from(file),);
    let call = conn.call_method(
        Some(control::CONTROL_NAME),
        control::CONTROL_PATH,
        Some(control::CONTROL_NAME),
        "ImportVocabulary",
        &body,
    );
    let reply = match tokio::time::timeout(IMPORT_TIMEOUT, call).await {
        Err(_) => return Err(IMPORT_NO_ANSWER),
        Ok(Ok(m)) => m,
        Ok(Err(zbus::Error::MethodError(name, desc, _))) => {
            return Err(import_error_line(name.as_str(), desc.as_deref()))
        }
        Ok(Err(_)) => return Err(IMPORT_GENERIC),
    };
    reply.body().deserialize().map_err(|_| IMPORT_GENERIC)
}

fn main() -> ExitCode {
    harden();
    quiet_panics();
    // Client mode, before any engine or IBus startup.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "import") {
        return import_main(&args[1..]);
    }
    let register = std::env::args().skip(1).any(|a| a == "--register");
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => {
            eprintln!("correctiond: cannot start runtime");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(run(register)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("correctiond: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(register: bool) -> Result<(), String> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let config = Config::load(&home);
    if config.allow_any_caller {
        eprintln!("{ANY_CALLER_WARNING}");
    }
    let trace = Trace::from_env(&home);
    trace.event(&["start"], &[("register", u64::from(register))]);

    // Everything an engine needs is ready before the daemon can ask for one.
    // One dictionary for the whole process, languages in config order
    // (`LangId` = position).
    let langs = config.languages.clone();
    let t0 = Instant::now();
    let dict = tokio::task::spawn_blocking(move || {
        let pairs: Vec<(&str, &str)> = langs
            .iter()
            .map(|l| (l.code.as_str(), l.lexicon.as_str()))
            .collect();
        build_dictionary(&pairs)
    })
    .await
    .map_err(|_| "dictionary build failed")?;
    let ms = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
    trace.dict_stats(ms, dict.index_bytes());
    if ms > SLOW_BUILD_MS {
        eprintln!("correctiond: dictionary build took {ms} ms");
    }
    // The one store of this process. It starts memory-only; the actor
    // attaches the encrypted database once the keyring hands out the key.
    let store = Arc::new(Mutex::new(LearnStore::memory()));
    let persist = persist::spawn_persistence(store.clone(), home.join(".local/share/correctiond"));
    let shared = Arc::new(Shared {
        dict: Arc::new(dict),
        store,
        counters: Arc::new(Counters::default()),
        trace,
        persist: Some(persist.clone()),
        control: control::ControlState::new(&config),
    });
    // The control plane lives on the session bus, apart from ibus-daemon's
    // bus. In the background: a slow or missing session bus must not delay
    // typing, and without it every app stays unidentified (observe-only).
    tokio::spawn(control::serve(shared.clone()));

    let addr = ibus::ibus_address(&home).ok_or("ibus-daemon address not found")?;
    let conn = zbus::connection::Builder::address(addr.as_str())
        .map_err(|e| format!("bad ibus address: {e}"))?
        .build()
        .await
        .map_err(|e| format!("cannot connect to ibus-daemon: {e}"))?;
    conn.object_server()
        .at(FACTORY_PATH, Factory::new(shared.clone()))
        .await
        .map_err(|e| format!("cannot serve factory: {e}"))?;
    conn.request_name(BUS_NAME)
        .await
        .map_err(|e| format!("cannot own {BUS_NAME}: {e}"))?;
    if register {
        let exec = std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let component = ibus::component(&exec).map_err(|e| format!("component: {e}"))?;
        conn.call_method(
            Some("org.freedesktop.IBus"),
            "/org/freedesktop/IBus",
            Some("org.freedesktop.IBus"),
            "RegisterComponent",
            &(component,),
        )
        .await
        .map_err(|e| format!("RegisterComponent failed: {e}"))?;
    }
    shared.trace.event(&["ready"], &[]);

    // Serve until ibus-daemon goes away or we are told to stop.
    // The stream ends when the connection closes; messages are just drained.
    let mut incoming = std::pin::pin!(zbus::MessageStream::from(&conn));
    let closed = async {
        while std::future::poll_fn(|cx| incoming.as_mut().poll_next(cx))
            .await
            .is_some()
        {}
    };
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| "cannot watch SIGTERM")?;
    tokio::select! {
        _ = closed => {}
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    // Last write-behind, bounded so a stuck disk cannot hold up exit.
    if tokio::time::timeout(EXIT_FLUSH, persist.flush_now())
        .await
        .is_err()
    {
        eprintln!("correctiond: final flush timed out");
    }
    shared.trace.event(&["exit"], &[]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_client_lines_are_fixed() {
        assert_eq!(import_ok_line(12, 3), "Imported 12 words and 3 word pairs.");
        let failed = "org.nadelacruz.Correctiond1.Error.ImportFailed";
        for t in control::IMPORT_ERRORS {
            assert_eq!(import_error_line(failed, Some(t)), *t);
        }
        // Text from anywhere else is never echoed.
        assert_eq!(
            import_error_line(failed, Some("sikreto /home/x/chat.vocab.tsv")),
            "import failed"
        );
        assert_eq!(import_error_line(failed, None), "import failed");
        assert_eq!(
            import_error_line("com.example.Error", Some("sikreto")),
            "import failed"
        );
        assert_eq!(
            import_error_line("org.freedesktop.DBus.Error.ServiceUnknown", Some("x")),
            "correctiond is not running"
        );
    }

    #[test]
    fn any_caller_warning_text() {
        assert_eq!(
            ANY_CALLER_WARNING,
            "correctiond: WARNING: allow_any_caller is on; any session-bus client can call the control interface"
        );
    }
}
