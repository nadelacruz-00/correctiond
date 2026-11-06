//! Opt-in event trace (`CORRECTIOND_TRACE=1`).
//!
//! The API only accepts `&'static str` words and integers, so typed
//! characters, words and key material cannot reach the log by construction.
//! Lines go to a background thread: the key path never touches the disk.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::time::Instant;

/// The log is truncated once it grows past this.
const MAX_BYTES: u64 = 1 << 20;

pub struct Trace {
    tx: Option<Sender<String>>,
    start: Instant,
}

impl Trace {
    pub fn disabled() -> Trace {
        Trace {
            tx: None,
            start: Instant::now(),
        }
    }

    pub fn path(home: &Path) -> PathBuf {
        home.join(".local/state/correctiond/trace.log")
    }

    /// Enabled only when `CORRECTIOND_TRACE` is exactly `1`.
    pub fn from_env(home: &Path) -> Trace {
        if std::env::var_os("CORRECTIOND_TRACE").is_some_and(|v| v == "1") {
            Trace::open(Trace::path(home))
        } else {
            Trace::disabled()
        }
    }

    pub fn open(path: PathBuf) -> Trace {
        let (tx, rx) = mpsc::channel::<String>();
        let spawned = std::thread::Builder::new()
            .name("trace".into())
            .spawn(move || {
                let Some(mut w) = Writer::open(path) else {
                    eprintln!("correctiond: cannot open trace log, tracing disabled");
                    return;
                };
                for line in rx {
                    w.write(&line);
                }
            });
        Trace {
            tx: spawned.is_ok().then_some(tx),
            start: Instant::now(),
        }
    }

    /// One line: `<seconds> <words...> <key=value...>`.
    pub fn event(&self, words: &[&'static str], fields: &[(&'static str, u64)]) {
        let Some(tx) = &self.tx else { return };
        let mut line = format!("{:.3}", self.start.elapsed().as_secs_f64());
        for w in words {
            line.push(' ');
            line.push_str(w);
        }
        for (k, v) in fields {
            line.push_str(&format!(" {k}={v}"));
        }
        // A dead writer thread just means no trace.
        let _ = tx.send(line);
    }

    /// Dictionary build stats: milliseconds and index size in MiB.
    pub fn dict_stats(&self, ms: u64, index_bytes: usize) {
        let Some(tx) = &self.tx else { return };
        let mib = index_bytes as f64 / f64::from(1u32 << 20);
        let _ = tx.send(format!(
            "{:.3} dictionary ms={ms} index_mib={mib:.1}",
            self.start.elapsed().as_secs_f64()
        ));
    }
}

struct Writer {
    file: File,
    len: u64,
}

impl Writer {
    fn open(path: PathBuf) -> Option<Writer> {
        if let Some(dir) = path.parent() {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .ok()?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .ok()?;
        // A pre-existing file keeps its old mode unless we fix it.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .ok()?;
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        Some(Writer { file, len })
    }

    fn write(&mut self, line: &str) {
        if self.len > MAX_BYTES && self.file.set_len(0).is_ok() {
            self.len = 0;
        }
        if writeln!(self.file, "{line}").is_ok() {
            self.len += line.len() as u64 + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn read_until(p: &Path, done: impl Fn(&str) -> bool) -> String {
        for _ in 0..200 {
            if let Ok(s) = std::fs::read_to_string(p) {
                if done(&s) {
                    return s;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::read_to_string(p).unwrap_or_default()
    }

    #[test]
    fn writes_words_and_numbers_to_a_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state/correctiond/trace.log");
        let t = Trace::open(p.clone());
        t.event(&["key", "Printable"], &[]);
        t.event(&["ContentType"], &[("purpose", 0), ("hints", 4)]);
        let s = read_until(&p, |s| s.lines().count() >= 2);
        let lines: Vec<&str> = s.lines().collect();
        assert!(lines[0].ends_with(" key Printable"), "{s}");
        assert!(lines[1].ends_with(" ContentType purpose=0 hints=4"), "{s}");
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dmode = std::fs::metadata(p.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dmode, 0o700);
    }

    #[test]
    fn truncates_past_one_mib() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("trace.log");
        std::fs::write(&p, vec![b'x'; (MAX_BYTES + 10) as usize]).unwrap();
        let t = Trace::open(p.clone());
        t.event(&["FocusIn"], &[]);
        let s = read_until(&p, |s| s.ends_with("FocusIn\n"));
        assert_eq!(s.lines().count(), 1, "old content kept");
        assert!(s.ends_with(" FocusIn\n"));
    }

    #[test]
    fn disabled_trace_is_inert() {
        let t = Trace::disabled();
        assert!(t.tx.is_none());
        t.event(&["key", "Printable"], &[]);
    }
}
