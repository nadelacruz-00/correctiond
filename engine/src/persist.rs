//! The single persistence actor: keyring, attach, write-behind and purge.
//!
//! One task owns every keyring call and every database operation, so attach,
//! flush and purge can never interleave. It has two states, with the
//! invariant that the store is persistent exactly when the actor holds a
//! `Db`:
//! - **Acquire** (no `Db`): delete a pending-forget key item, then fetch or
//!   create the key and open the database; on failure back off 1, 2, 4 …
//!   60 s. `nudge` cuts a backoff short; it never starts a second attempt.
//! - **Persistent**: write-behind of `take_dirty` every 30 s and on
//!   `flush_now`.
//!
//! Every wait (backoff, flush timer, keyring call) also listens for
//! requests: `purge` is served at once in any state (it cancels a keyring
//! call in flight), `flush_now` answers `Ok(0)` while acquiring, and
//! `import` is refused while acquiring (the duplicate-import record lives
//! in the database, so it can never be bypassed). `forget_word` is
//! refused while acquiring if a database file exists (the words would come
//! back on attach), and is memory-only when there is none; `forget_words`
//! does the same for a whole batch in one request.
//! Blocking database work runs to completion in `spawn_blocking` with no
//! store lock held; requests queue behind it for those milliseconds.
//!
//! Logs and returned errors are fixed texts and counts: never key material,
//! words, or `StoreError::Io` details (which can quote SQL).

use crate::keyring::{
    delete_key_with, fetch_or_create_key_with, KeyState, Oo7Backend, SecretBackend,
};
use corelib::db::{self, AttachOutcome, Db, DirtyBatch, Snapshot, StoreError};
use corelib::key::StoreKey;
use corelib::learning::SharedStore;
use corelib::vocab::VocabBatch;
use std::future::Future;
use std::os::unix::fs::OpenOptionsExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::time::Instant;

const KEY_PURPOSE: &str = "learn-db-key";
const MARKER: &str = "key-delete-pending";
const FLUSH_EVERY: Duration = Duration::from_secs(30);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

const STOPPED: &str = "persistence stopped";
const WRITE_FAILED: &str = "learning store write failed";
const FILES_FAILED: &str = "learned data files could not be removed";
const DELETE_PENDING: &str =
    "learned data deleted; keyring item will be removed when the keyring is unlocked";
pub const IMPORT_UNAVAILABLE: &str = "learning store not available; unlock your keyring and retry";
pub const IMPORT_DUPLICATE: &str = "this file was already imported";
pub const IMPORT_WRITE_FAILED: &str = WRITE_FAILED;
pub const IMPORT_STOPPED: &str = STOPPED;
pub const FORGET_LOCKED: &str = "unlock your keyring first";
pub const FORGET_FAILED: &str = "could not forget";

/// Imported rows are read and added under the store lock this many at a
/// time, so a large import never holds the typing path for more than a
/// moment.
const IMPORT_CHUNK: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PersistStats {
    pub persistent: bool,
    pub quarantined_files: u32,
    pub key_delete_pending: bool,
}

#[derive(Default)]
struct Stats {
    persistent: AtomicBool,
    quarantined_files: AtomicU32,
    key_delete_pending: AtomicBool,
}

type FlushReply = oneshot::Sender<Result<usize, String>>;
type PurgeReply = oneshot::Sender<Result<(), String>>;
type ImportReply = oneshot::Sender<Result<(u32, u32), String>>;
type ForgetReply = oneshot::Sender<Result<u32, String>>;

struct ImportReq {
    batch: VocabBatch,
    sha: [u8; 32],
    reply: ImportReply,
}

enum Req {
    Flush(FlushReply),
    Purge(PurgeReply),
    Import(Box<ImportReq>),
    /// Already-lowercased words.
    Forget(Vec<String>, ForgetReply),
}

/// Cheap to clone. The actor stops once every handle is dropped.
#[derive(Clone)]
pub struct PersistHandle {
    tx: mpsc::Sender<Req>,
    wake: Arc<Notify>,
    stats: Arc<Stats>,
}

impl PersistHandle {
    /// Cut a pending backoff short (for example on FocusIn). Non-blocking.
    pub fn nudge(&self) {
        self.wake.notify_one();
    }

    /// Write pending rows now; `Ok(0)` while the store is memory-only.
    pub async fn flush_now(&self) -> Result<usize, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Req::Flush(tx))
            .await
            .map_err(|_| STOPPED.to_owned())?;
        rx.await.map_err(|_| STOPPED.to_owned())?
    }

    /// `flush_now` without waiting for it (D-Bus handlers return at once).
    pub fn flush_in_background(&self) {
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let h = self.clone();
            rt.spawn(async move {
                let _ = h.flush_now().await;
            });
        }
    }

    /// Forget everything: memory, database files and the keyring item.
    pub async fn purge(&self) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Req::Purge(tx))
            .await
            .map_err(|_| STOPPED.to_owned())?;
        rx.await.map_err(|_| STOPPED.to_owned())?
    }

    /// Add an imported vocabulary file's counts (`sha` = SHA-256 of its raw
    /// bytes); the persistable rows and the import record are committed in
    /// one transaction before the reply. Replies (words, word pairs)
    /// imported; refused
    /// while the store is not persistent and for a file already imported.
    pub async fn import(&self, batch: VocabBatch, sha: [u8; 32]) -> Result<(u32, u32), String> {
        let (tx, rx) = oneshot::channel();
        let req = ImportReq {
            batch,
            sha,
            reply: tx,
        };
        self.tx
            .send(Req::Import(Box::new(req)))
            .await
            .map_err(|_| STOPPED.to_owned())?;
        rx.await.map_err(|_| STOPPED.to_owned())?
    }

    /// Forget one word (any case): `forget_words` with one word.
    /// `Ok(found)`.
    pub async fn forget_word(&self, word: String) -> Result<bool, String> {
        self.forget_words(vec![word]).await.map(|n| n == 1)
    }

    /// Forget words (any case): their counts, word pairs on either side,
    /// blacklist pairs and recent entries, in one request (and, when
    /// attached, one transaction). `Ok(n)` = how many distinct words
    /// existed, once they are gone from memory, and from disk when the
    /// store is attached. Refused (`FORGET_LOCKED`) while a database file
    /// exists but is not attached; `FORGET_FAILED` if the delete fails
    /// (memory is then untouched). The words are lowercased once, here:
    /// `Db::forget_words` binds them as given, and store keys are
    /// lowercase.
    pub async fn forget_words(&self, words: Vec<String>) -> Result<u32, String> {
        let words = words.into_iter().map(|w| w.to_lowercase()).collect();
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Req::Forget(words, tx))
            .await
            .map_err(|_| STOPPED.to_owned())?;
        rx.await.map_err(|_| STOPPED.to_owned())?
    }

    pub fn stats(&self) -> PersistStats {
        PersistStats {
            persistent: self.stats.persistent.load(Ordering::Relaxed),
            quarantined_files: self.stats.quarantined_files.load(Ordering::Relaxed),
            key_delete_pending: self.stats.key_delete_pending.load(Ordering::Relaxed),
        }
    }
}

/// The actor on the real keyring. Must be called inside the tokio runtime.
pub fn spawn_persistence(store: SharedStore, dir: PathBuf) -> PersistHandle {
    spawn_persistence_with(store, dir, Oo7Backend::new())
}

pub fn spawn_persistence_with<B: SecretBackend + 'static>(
    store: SharedStore,
    dir: PathBuf,
    backend: B,
) -> PersistHandle {
    let (tx, rx) = mpsc::channel(32);
    let wake = Arc::new(Notify::new());
    let stats = Arc::new(Stats::default());
    // A forget that could not reach the keyring last time is finished first.
    if dir.join(MARKER).exists() {
        stats.key_delete_pending.store(true, Ordering::Relaxed);
    }
    let actor = Actor {
        store,
        dir,
        backend,
        rx,
        wake: wake.clone(),
        stats: stats.clone(),
        db: None,
        backoff: BACKOFF_MIN,
        last_problem: None,
        write_failing: false,
    };
    tokio::spawn(actor.run());
    PersistHandle { tx, wake, stats }
}

/// How a cancellable keyring step ended.
enum Step<T> {
    Done(T),
    Purge(PurgeReply),
    Stop,
}

/// The blocking open's result: the database, its rows, and whether an
/// unreadable file was set aside; or a fixed failure description.
type Opened = Result<(Db, Snapshot, bool), &'static str>;

struct Actor<B> {
    store: SharedStore,
    dir: PathBuf,
    backend: B,
    rx: mpsc::Receiver<Req>,
    wake: Arc<Notify>,
    stats: Arc<Stats>,
    /// `Some` exactly in the Persistent state.
    db: Option<Db>,
    backoff: Duration,
    /// Last reported failure, so a retry loop logs each change once.
    last_problem: Option<String>,
    /// The previous write failed (logged once per failing streak).
    write_failing: bool,
}

/// Fixed description of a store error; the `Io` text is never shown.
fn store_error_kind(e: &StoreError) -> &'static str {
    match e {
        StoreError::NotSqlcipher => "store is not sqlcipher",
        StoreError::Unreadable => "store unreadable",
        StoreError::Io(_) => "store io error",
    }
}

fn refuse_import(reply: ImportReply) {
    let _ = reply.send(Err(IMPORT_UNAVAILABLE.to_owned()));
}

/// `forget_words` in the Acquire state. A database file that is not
/// attached would bring the words back on attach, so the forget is
/// refused; with no file, memory is all there is.
fn forget_unattached(dir: &Path, store: &SharedStore, ws: &[String]) -> Result<u32, String> {
    if db::db_path(dir).exists() {
        return Err(FORGET_LOCKED.to_owned());
    }
    Ok(store
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .forget_words(ws))
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Add `batch` to the store, `IMPORT_CHUNK` rows per lock.
fn add_in_chunks(store: &SharedStore, batch: VocabBatch) {
    let lock = || store.lock().unwrap_or_else(|e| e.into_inner());
    let mut words = batch.words.into_iter().peekable();
    while words.peek().is_some() {
        let chunk = VocabBatch {
            words: words.by_ref().take(IMPORT_CHUNK).collect(),
            bigrams: Vec::new(),
        };
        lock().add_imported(&chunk);
    }
    let mut bigrams = batch.bigrams.into_iter().peekable();
    while bigrams.peek().is_some() {
        let chunk = VocabBatch {
            words: Vec::new(),
            bigrams: bigrams.by_ref().take(IMPORT_CHUNK).collect(),
        };
        lock().add_imported(&chunk);
    }
}

/// The rows the import leaves at or above the persistence threshold
/// (memory + imported), read `IMPORT_CHUNK` keys per lock.
fn import_rows(store: &SharedStore, batch: &VocabBatch) -> DirtyBatch {
    let lock = || store.lock().unwrap_or_else(|e| e.into_inner());
    let mut rows = DirtyBatch::default();
    for chunk in batch.words.chunks(IMPORT_CHUNK) {
        rows.vocab.extend(lock().import_totals(chunk, &[]).vocab);
    }
    for chunk in batch.bigrams.chunks(IMPORT_CHUNK) {
        rows.bigrams
            .extend(lock().import_totals(&[], chunk).bigrams);
    }
    rows
}

/// The import's blocking part. The rows (absolute totals) and the import
/// record commit in ONE transaction, and memory changes only after that
/// commit: a failure leaves no record and memory untouched, and the reply
/// comes only once the counts are on disk. A key typed between the read
/// and the add stays dirty, so the next flush writes its true total.
fn import_blocking(
    db: &mut Db,
    store: &SharedStore,
    batch: VocabBatch,
    sha: &[u8; 32],
) -> Result<Result<(u32, u32), &'static str>, StoreError> {
    if db.has_import(sha)? {
        return Ok(Err(IMPORT_DUPLICATE));
    }
    let rows = import_rows(store, &batch);
    db.write_import(&rows, sha)?;
    let counts = (len_u32(batch.words.len()), len_u32(batch.bigrams.len()));
    add_in_chunks(store, batch);
    Ok(Ok(counts))
}

fn batch_is_empty(b: &DirtyBatch) -> bool {
    b.vocab.is_empty() && b.blacklist.is_empty() && b.bigrams.is_empty() && b.slips.is_empty()
}

/// Try every candidate key; quarantine only when all of them see an
/// unreadable file (never on I/O errors, which may be transient).
fn open_with(dir: &Path, keys: Vec<StoreKey>) -> Opened {
    let mut failure = None;
    for key in &keys {
        match db::try_open(dir, key) {
            Ok((db, snap)) => return Ok((db, snap, false)),
            Err(StoreError::Unreadable) => {}
            Err(e) => failure = Some(store_error_kind(&e)),
        }
    }
    if let Some(kind) = failure {
        return Err(kind);
    }
    let Some(first) = keys.first() else {
        return Err("no key");
    };
    match db::attach(dir, first) {
        Ok((db, snap, outcome)) => {
            let quarantined = matches!(outcome, AttachOutcome::QuarantinedAndCreated(_));
            Ok((db, snap, quarantined))
        }
        Err(e) => Err(store_error_kind(&e)),
    }
    // `keys` drops (and zeroizes) here.
}

/// Purge's file work. The pending-delete marker (0600, empty, in a 0700
/// dir) goes down FIRST: a crash at any later point (during the file
/// removal, or before the keyring delete) still makes the next start delete
/// the old key item before it could be found and reused. `purge_files` only
/// removes `learn.db*` names, so the marker survives it.
fn purge_files_and_mark(dir: &Path) -> (bool, bool) {
    mark_then_remove(dir, db::purge_files)
}

/// `purge_files_and_mark` with the removal injectable (tests crash it).
fn mark_then_remove(dir: &Path, remove: impl FnOnce(&Path) -> std::io::Result<()>) -> (bool, bool) {
    let marked = db::create_private_dir(dir)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(dir.join(MARKER))
        })
        .is_ok();
    let files_ok = remove(dir).is_ok();
    (files_ok, marked)
}

/// Run a keyring future while still serving requests.
async fn step<F: Future>(
    rx: &mut mpsc::Receiver<Req>,
    wake: &Notify,
    dir: &Path,
    store: &SharedStore,
    fut: F,
) -> Step<F::Output> {
    let mut fut = std::pin::pin!(fut);
    loop {
        tokio::select! {
            out = &mut fut => return Step::Done(out),
            req = rx.recv() => match req {
                None => return Step::Stop,
                Some(Req::Flush(r)) => {
                    let _ = r.send(Ok(0));
                }
                Some(Req::Purge(r)) => return Step::Purge(r),
                Some(Req::Import(r)) => refuse_import(r.reply),
                Some(Req::Forget(ws, r)) => {
                    let _ = r.send(forget_unattached(dir, store, &ws));
                }
            },
            // Already attempting: swallow nudges instead of queueing a retry.
            _ = wake.notified() => {}
        }
    }
}

impl<B: SecretBackend> Actor<B> {
    async fn run(mut self) {
        loop {
            let go_on = if self.db.is_some() {
                self.persistent().await
            } else {
                self.acquire().await
            };
            if !go_on {
                break;
            }
        }
    }

    fn lock_store(&self) -> std::sync::MutexGuard<'_, corelib::learning::LearnStore> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn report(&mut self, problem: &str) {
        if self.last_problem.as_deref() != Some(problem) {
            eprintln!("correctiond: {problem}; learning stays in memory");
            self.last_problem = Some(problem.to_owned());
        }
    }

    fn set_delete_pending(&self, on: bool) {
        self.stats.key_delete_pending.store(on, Ordering::Relaxed);
    }

    /// After the key item is gone: drop the marker, then the flag. A marker
    /// left behind would delete the NEXT key on restart, so the flag stays
    /// set (and Acquire keeps retrying) unless the marker is really gone.
    fn clear_delete_pending(&mut self) -> bool {
        match std::fs::remove_file(self.dir.join(MARKER)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                self.report("cannot remove the key-delete marker");
                return false;
            }
        }
        self.set_delete_pending(false);
        true
    }

    /// One attempt, then (on failure) one backoff wait. Returns false to stop.
    async fn acquire(&mut self) -> bool {
        match self.attempt().await {
            Step::Done(true) => return true,
            Step::Done(false) => {}
            Step::Purge(r) => {
                self.purge(r).await;
                return true;
            }
            Step::Stop => return false,
        }
        let sleep = tokio::time::sleep(self.backoff);
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
        let mut sleep = std::pin::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => return true,
                _ = self.wake.notified() => return true,
                req = self.rx.recv() => match req {
                    None => return false,
                    Some(Req::Flush(r)) => {
                        let _ = r.send(Ok(0));
                    }
                    Some(Req::Purge(r)) => {
                        self.purge(r).await;
                        return true;
                    }
                    Some(Req::Import(r)) => refuse_import(r.reply),
                    Some(Req::Forget(ws, r)) => {
                        let _ = r.send(forget_unattached(&self.dir, &self.store, &ws));
                    }
                },
            }
        }
    }

    /// `Done(true)` once attached.
    async fn attempt(&mut self) -> Step<bool> {
        if self.stats.key_delete_pending.load(Ordering::Relaxed) {
            match step(
                &mut self.rx,
                &self.wake,
                &self.dir,
                &self.store,
                delete_key_with(&self.backend, KEY_PURPOSE),
            )
            .await
            {
                Step::Done(Ok(())) => {
                    if !self.clear_delete_pending() {
                        return Step::Done(false);
                    }
                }
                // Never look up the key while the forgotten one may remain.
                Step::Done(Err(_)) => return Step::Done(false),
                Step::Purge(r) => return Step::Purge(r),
                Step::Stop => return Step::Stop,
            }
        }
        let state = match step(
            &mut self.rx,
            &self.wake,
            &self.dir,
            &self.store,
            fetch_or_create_key_with(&self.backend, KEY_PURPOSE),
        )
        .await
        {
            Step::Done(s) => s,
            Step::Purge(r) => return Step::Purge(r),
            Step::Stop => return Step::Stop,
        };
        let keys = match state {
            KeyState::Ready(keys) => keys,
            KeyState::Locked => {
                self.report("keyring locked");
                return Step::Done(false);
            }
            KeyState::Unavailable(reason) => {
                self.report(&format!("keyring unavailable ({reason})"));
                return Step::Done(false);
            }
        };
        let dir = self.dir.clone();
        // The keys move into the blocking task and are dropped there.
        let opened = tokio::task::spawn_blocking(move || open_with(&dir, keys)).await;
        match opened {
            Ok(Ok((db, snap, quarantined))) => {
                {
                    let mut s = self.lock_store();
                    s.merge_snapshot(snap);
                    s.set_persistent(true);
                }
                if quarantined {
                    self.stats.quarantined_files.fetch_add(1, Ordering::Relaxed);
                    eprintln!(
                        "correctiond: unreadable learning store set aside; started a new one"
                    );
                }
                self.db = Some(db);
                self.stats.persistent.store(true, Ordering::Relaxed);
                self.backoff = BACKOFF_MIN;
                self.last_problem = None;
                eprintln!("correctiond: encrypted learning store attached");
                Step::Done(true)
            }
            Ok(Err(kind)) => {
                self.report(kind);
                Step::Done(false)
            }
            Err(_) => {
                self.report("store open failed");
                Step::Done(false)
            }
        }
    }

    /// Wait for the flush timer or a request. Returns false to stop.
    async fn persistent(&mut self) -> bool {
        let deadline = Instant::now() + FLUSH_EVERY;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    let _ = self.flush().await;
                    return true;
                }
                _ = self.wake.notified() => {}
                req = self.rx.recv() => match req {
                    None => {
                        let _ = self.flush().await;
                        return false;
                    }
                    Some(Req::Flush(r)) => {
                        let res = self.flush().await;
                        let _ = r.send(res);
                        if self.db.is_none() {
                            return true;
                        }
                    }
                    Some(Req::Purge(r)) => {
                        self.purge(r).await;
                        return true;
                    }
                    Some(Req::Import(r)) => {
                        let res = self.import(r.batch, r.sha).await;
                        let _ = r.reply.send(res);
                        if self.db.is_none() {
                            return true;
                        }
                    }
                    Some(Req::Forget(ws, r)) => {
                        let res = self.forget(ws).await;
                        let _ = r.send(res);
                        if self.db.is_none() {
                            return true;
                        }
                    }
                },
            }
        }
    }

    /// Persistent → Acquire after the connection became unusable.
    fn detach(&mut self) {
        self.db = None;
        self.lock_store().set_persistent(false);
        self.stats.persistent.store(false, Ordering::Relaxed);
    }

    async fn flush(&mut self) -> Result<usize, String> {
        let batch = self.lock_store().take_dirty();
        if batch_is_empty(&batch) {
            return Ok(0);
        }
        let Some(mut db) = self.db.take() else {
            self.lock_store().restore_dirty(batch);
            return Ok(0);
        };
        let joined = tokio::task::spawn_blocking(move || {
            let res = catch_unwind(AssertUnwindSafe(|| db.write(&batch)));
            (db, batch, res)
        })
        .await;
        match joined {
            Ok((db, _, Ok(Ok(n)))) => {
                self.db = Some(db);
                self.write_failing = false;
                Ok(n)
            }
            Ok((db, batch, Ok(Err(e)))) => {
                self.db = Some(db);
                self.lock_store().restore_dirty(batch);
                if !self.write_failing {
                    eprintln!("correctiond: {}, will retry", store_error_kind(&e));
                    self.write_failing = true;
                }
                Err(WRITE_FAILED.to_owned())
            }
            Ok((_db, batch, Err(_))) => {
                self.lock_store().restore_dirty(batch);
                self.detach();
                Err(WRITE_FAILED.to_owned())
            }
            Err(_) => {
                self.detach();
                Err(WRITE_FAILED.to_owned())
            }
        }
    }

    /// Persistent only. The duplicate check, the one-transaction commit and
    /// the chunked memory add run in `spawn_blocking` (`import_blocking`).
    /// No flush: the imported rows are already committed, and unrelated
    /// dirty rows go out with the regular write-behind.
    async fn import(&mut self, batch: VocabBatch, sha: [u8; 32]) -> Result<(u32, u32), String> {
        let Some(mut db) = self.db.take() else {
            return Err(IMPORT_UNAVAILABLE.to_owned());
        };
        let store = self.store.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let res = catch_unwind(AssertUnwindSafe(|| {
                import_blocking(&mut db, &store, batch, &sha)
            }));
            (db, res)
        })
        .await;
        match joined {
            Ok((db, Ok(Ok(res)))) => {
                self.db = Some(db);
                res.map_err(str::to_owned)
            }
            Ok((db, Ok(Err(e)))) => {
                self.db = Some(db);
                eprintln!("correctiond: import failed: {}", store_error_kind(&e));
                Err(WRITE_FAILED.to_owned())
            }
            Ok((_, Err(_))) | Err(_) => {
                self.detach();
                Err(WRITE_FAILED.to_owned())
            }
        }
    }

    /// Persistent only. The database delete (one transaction for the whole
    /// batch) runs first, in `spawn_blocking` inside the actor (so no flush
    /// can run between it and the memory forget); memory changes only
    /// after it commits. `ws` is already lowercased (`forget_words`).
    async fn forget(&mut self, ws: Vec<String>) -> Result<u32, String> {
        let Some(mut db) = self.db.take() else {
            return forget_unattached(&self.dir, &self.store, &ws);
        };
        let joined = tokio::task::spawn_blocking(move || {
            let res = catch_unwind(AssertUnwindSafe(|| db.forget_words(&ws)));
            (db, ws, res)
        })
        .await;
        match joined {
            Ok((db, ws, Ok(Ok(())))) => {
                self.db = Some(db);
                Ok(self.lock_store().forget_words(&ws))
            }
            Ok((db, _, Ok(Err(e)))) => {
                self.db = Some(db);
                eprintln!("correctiond: forget failed: {}", store_error_kind(&e));
                Err(FORGET_FAILED.to_owned())
            }
            Ok((_, _, Err(_))) | Err(_) => {
                eprintln!("correctiond: forget failed");
                self.detach();
                Err(FORGET_FAILED.to_owned())
            }
        }
    }

    /// Any state → Acquire, with memory, files and (if reachable) the key
    /// item gone.
    async fn purge(&mut self, reply: PurgeReply) {
        self.db = None;
        {
            let mut s = self.lock_store();
            s.clear();
            s.set_persistent(false);
        }
        self.stats.persistent.store(false, Ordering::Relaxed);
        self.set_delete_pending(true);
        let dir = self.dir.clone();
        let (files_ok, marked) = tokio::task::spawn_blocking(move || purge_files_and_mark(&dir))
            .await
            .unwrap_or((false, false));
        if files_ok {
            self.stats.quarantined_files.store(0, Ordering::Relaxed);
        }
        if !marked {
            eprintln!("correctiond: cannot write the key-delete marker");
        }
        self.backoff = BACKOFF_MIN;
        self.last_problem = None;
        let deleted = delete_key_with(&self.backend, KEY_PURPOSE).await.is_ok()
            && self.clear_delete_pending();
        let res = if !files_ok {
            Err(FILES_FAILED.to_owned())
        } else if !deleted {
            Err(DELETE_PENDING.to_owned())
        } else {
            Ok(())
        };
        let _ = reply.send(res);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyring::fake::FakeBackend;
    use corelib::key::StoreKey;
    use corelib::learning::LearnStore;
    use corelib::scoring::PersonalLookup;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn new_store() -> SharedStore {
        Arc::new(Mutex::new(LearnStore::memory()))
    }

    fn lock(s: &SharedStore) -> std::sync::MutexGuard<'_, LearnStore> {
        s.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Polls in virtual time (start_paused); gives up after ~10 virtual minutes.
    async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        for _ in 0..60_000 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    async fn persistent_actor(dir: &Path) -> (SharedStore, FakeBackend, PersistHandle) {
        let store = new_store();
        let fake = FakeBackend::default();
        let h = spawn_persistence_with(store.clone(), dir.to_path_buf(), fake.clone());
        wait_until("persistent", || h.stats().persistent).await;
        (store, fake, h)
    }

    fn key_of(secret: &[u8]) -> StoreKey {
        StoreKey::from_hex(std::str::from_utf8(secret).unwrap()).unwrap()
    }

    fn index_of(log: &[&str], call: &str) -> Option<usize> {
        log.iter().position(|c| *c == call)
    }

    #[tokio::test(start_paused = true)]
    async fn locked_stays_memory_then_attaches() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("correctiond");
        let store = new_store();
        let fake = FakeBackend::default();
        fake.state().locked_calls = 2;
        let h = spawn_persistence_with(store.clone(), dir.clone(), fake.clone());

        // First attempt (t=0) and second (t=1 s) both find the keyring locked.
        wait_until("two locked attempts", || fake.count("locked") >= 2).await;
        assert!(!h.stats().persistent);
        assert!(!lock(&store).is_persistent());
        assert!(!dir.join("learn.db").exists());
        assert_eq!(fake.log(), ["locked", "locked"]);

        // The third attempt (t=3 s) is unlocked and empty: create + attach.
        tokio::time::sleep(Duration::from_secs(4)).await;
        wait_until("persistent", || h.stats().persistent).await;
        assert!(lock(&store).is_persistent());
        assert!(dir.join("learn.db").exists());
        assert_eq!(fake.count("create"), 1);
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[tokio::test(start_paused = true)]
    async fn nudge_does_not_double_create() {
        let tmp = tempfile::tempdir().unwrap();
        let store = new_store();
        let fake = FakeBackend::default();
        fake.state().find_delay = Some(Duration::from_secs(5));
        let h = spawn_persistence_with(store, tmp.path().to_path_buf(), fake.clone());
        wait_until("attempt under way", || fake.count("find") == 1).await;
        for _ in 0..5 {
            h.nudge();
        }
        wait_until("persistent", || h.stats().persistent).await;
        // Let any (wrongly) queued extra attempt run.
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(fake.count("create"), 1);
        assert_eq!(fake.count("find"), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn flush_writes_second_sightings() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        {
            let mut s = lock(&store);
            s.record_word("gstack");
            s.record_word("gstack");
            s.record_word("once");
        }
        let n = h.flush_now().await.unwrap();
        assert!(n >= 1);
        // Nothing left to write.
        assert_eq!(h.flush_now().await, Ok(0));

        let key = key_of(&fake.secrets()[0]);
        let (_db, snap) = corelib::db::try_open(tmp.path(), &key).unwrap();
        let words: Vec<_> = snap.vocab.iter().map(|r| r.word.as_str()).collect();
        assert_eq!(words, ["gstack"]);
        assert_eq!(snap.vocab[0].freq, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn flush_runs_every_30_seconds() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, _h) = persistent_actor(tmp.path()).await;
        {
            let mut s = lock(&store);
            s.record_word("gstack");
            s.record_word("gstack");
        }
        tokio::time::sleep(Duration::from_secs(31)).await;
        let key = key_of(&fake.secrets()[0]);
        let (_db, snap) = corelib::db::try_open(tmp.path(), &key).unwrap();
        assert_eq!(snap.vocab.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn attach_merges_snapshot_into_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeBackend::default();
        {
            let store = new_store();
            let h = spawn_persistence_with(store.clone(), tmp.path().to_path_buf(), fake.clone());
            wait_until("persistent", || h.stats().persistent).await;
            lock(&store).record_word("gstack");
            lock(&store).record_word("gstack");
            h.flush_now().await.unwrap();
        }
        // A fresh process: same keyring item, same files.
        let store = new_store();
        let h = spawn_persistence_with(store.clone(), tmp.path().to_path_buf(), fake.clone());
        wait_until("persistent", || h.stats().persistent).await;
        assert_eq!(lock(&store).personal_freq("gstack"), 2);
        assert_eq!(fake.count("create"), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn purge_when_locked_marks_pending() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        lock(&store).record_word("gstack");
        lock(&store).record_word("gstack");
        h.flush_now().await.unwrap();

        fake.set_locked(true);
        let err = h.purge().await.unwrap_err();
        assert_eq!(
            err,
            "learned data deleted; keyring item will be removed when the keyring is unlocked"
        );
        assert!(!tmp.path().join("learn.db").exists());
        assert!(h.stats().key_delete_pending);
        assert!(!h.stats().persistent);
        assert!(tmp.path().join("key-delete-pending").exists());
        assert_eq!(lock(&store).personal_freq("gstack"), 0);
        let old = fake.secrets();
        assert_eq!(old.len(), 1);

        // Still locked: backoff attempts never search for the old key.
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(fake.count("find"), 1);

        fake.clear_log();
        fake.set_locked(false);
        h.nudge();
        wait_until("persistent", || h.stats().persistent).await;
        let log = fake.log();
        let del = index_of(&log, "delete").expect("delete issued");
        assert!(index_of(&log, "find").unwrap() > del, "{log:?}");
        assert!(index_of(&log, "create").unwrap() > del, "{log:?}");
        assert!(!h.stats().key_delete_pending);
        assert!(!tmp.path().join("key-delete-pending").exists());
        let new = fake.secrets();
        assert_eq!(new.len(), 1);
        assert_ne!(new, old);
    }

    #[tokio::test(start_paused = true)]
    async fn pending_delete_survives_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let (_store, fake, h) = persistent_actor(tmp.path()).await;
        fake.set_locked(true);
        assert!(h.purge().await.is_err());
        let marker = tmp.path().join("key-delete-pending");
        assert!(marker.exists());
        let mode = std::fs::metadata(&marker).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::metadata(&marker).unwrap().len(), 0);
        let old = fake.secrets();
        drop(h);
        // Let the old actor notice its handles are gone and stop.
        tokio::time::sleep(Duration::from_secs(1)).await;

        fake.clear_log();
        fake.set_locked(false);
        let store = new_store();
        let h = spawn_persistence_with(store, tmp.path().to_path_buf(), fake.clone());
        assert!(h.stats().key_delete_pending);
        wait_until("persistent", || h.stats().persistent).await;
        let log = fake.log();
        let del = index_of(&log, "delete").expect("delete issued");
        assert!(index_of(&log, "find").unwrap() > del, "{log:?}");
        assert!(index_of(&log, "create").unwrap() > del, "{log:?}");
        assert!(!marker.exists());
        assert!(!h.stats().key_delete_pending);
        assert_ne!(fake.secrets(), old);
    }

    /// A data dir that already exists with a loose mode (a hand-made
    /// `mkdir -p` under umask 022) is tightened to 0700.
    #[test]
    fn mark_tightens_an_existing_loose_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("correctiond");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (files_ok, marked) = mark_then_remove(&dir, corelib::db::purge_files);
        assert!(files_ok && marked);
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    /// A crash in the middle of removing the files (or anywhere after)
    /// must find the marker already written.
    #[test]
    fn marker_is_written_before_files_are_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("correctiond");
        {
            let _ = corelib::db::try_open(&dir, &StoreKey::generate()).unwrap();
        }
        assert!(dir.join("learn.db").exists());
        let crashed = std::panic::catch_unwind(AssertUnwindSafe(|| {
            mark_then_remove(&dir, |d| {
                corelib::db::purge_files(d)?;
                panic!("simulated crash after purge_files");
            })
        }));
        assert!(crashed.is_err());
        assert!(!dir.join("learn.db").exists());
        assert!(dir.join("key-delete-pending").exists());
    }

    /// The process dies after the files are gone but before the keyring
    /// delete returns: the next start must delete the old item before any
    /// lookup, so the forgotten key is never reused.
    #[tokio::test(start_paused = true)]
    async fn crash_before_key_delete_restart_deletes_old_key() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        lock(&store).record_word("gstack");
        lock(&store).record_word("gstack");
        h.flush_now().await.unwrap();
        let old = fake.secrets();
        assert_eq!(old.len(), 1);

        fake.state().delete_hang = true;
        let purging = {
            let h = h.clone();
            tokio::spawn(async move { h.purge().await })
        };
        wait_until("purge reached the key delete", || {
            fake.count("delete") == 1 && !tmp.path().join("learn.db").exists()
        })
        .await;
        // "Crash": the old actor never runs again; the item is still there.
        purging.abort();
        assert_eq!(fake.secrets(), old);
        let marker = tmp.path().join("key-delete-pending");
        assert!(marker.exists());

        fake.clear_log();
        fake.state().delete_hang = false;
        let store2 = new_store();
        let h2 = spawn_persistence_with(store2.clone(), tmp.path().to_path_buf(), fake.clone());
        assert!(h2.stats().key_delete_pending);
        wait_until("persistent", || h2.stats().persistent).await;
        let log = fake.log();
        let del = index_of(&log, "delete").expect("delete issued");
        assert!(index_of(&log, "find").unwrap() > del, "{log:?}");
        assert!(index_of(&log, "create").unwrap() > del, "{log:?}");
        let new = fake.secrets();
        assert_eq!(new.len(), 1);
        assert_ne!(new, old);
        assert!(!marker.exists());
        assert_eq!(lock(&store2).personal_freq("gstack"), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_pending_delete_never_fetches() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("key-delete-pending"), b"").unwrap();
        let fake = FakeBackend::default();
        fake.insert(
            crate::keyring::key_attributes("learn-db-key"),
            StoreKey::generate().to_hex().as_bytes(),
        );
        fake.state().fail_delete = true;
        let h = spawn_persistence_with(new_store(), tmp.path().to_path_buf(), fake.clone());
        tokio::time::sleep(Duration::from_secs(20)).await;
        assert!(fake.count("delete") >= 3);
        assert_eq!(fake.count("find"), 0, "{:?}", fake.log());
        assert_eq!(fake.count("create"), 0);
        assert!(!h.stats().persistent);
        assert!(h.stats().key_delete_pending);

        fake.state().fail_delete = false;
        h.nudge();
        wait_until("persistent", || h.stats().persistent).await;
        assert!(!tmp.path().join("key-delete-pending").exists());
        assert_eq!(fake.count("create"), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn requests_not_blocked_by_backoff() {
        let tmp = tempfile::tempdir().unwrap();
        let store = new_store();
        let fake = FakeBackend::default();
        fake.set_locked(true);
        let h = spawn_persistence_with(store, tmp.path().to_path_buf(), fake.clone());
        // Backoffs 1+2+4+8+16+32 s, then 60 s each: at t=200 s a 60 s
        // backoff is pending.
        tokio::time::sleep(Duration::from_secs(200)).await;
        let attempts = fake.count("locked");
        assert!(attempts >= 7, "{attempts} attempts");

        let t0 = tokio::time::Instant::now();
        assert_eq!(h.flush_now().await, Ok(0));
        assert!(h.purge().await.is_err()); // locked: key delete pending
        assert_eq!(h.flush_now().await, Ok(0));
        assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    }

    #[tokio::test(start_paused = true)]
    async fn purge_returns_to_acquire() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        let first = fake.secrets();
        lock(&store).record_word("gstack");
        lock(&store).record_word("gstack");
        h.flush_now().await.unwrap();

        assert_eq!(h.purge().await, Ok(()));
        assert!(!h.stats().key_delete_pending);
        assert!(!tmp.path().join("key-delete-pending").exists());
        wait_until("persistent again", || h.stats().persistent).await;
        assert_eq!(fake.count("create"), 2);
        let second = fake.secrets();
        assert_eq!(second.len(), 1);
        assert_ne!(second, first);
        // The new database is empty and opens with the new key only.
        let (_db, snap) = corelib::db::try_open(tmp.path(), &key_of(&second[0])).unwrap();
        assert!(snap.vocab.is_empty());
        assert!(lock(&store).is_persistent());
        assert_eq!(lock(&store).personal_freq("gstack"), 0);
    }

    /// An I/O failure with the right key must not quarantine the file, even
    /// when another candidate key reports it unreadable. `learn.db-shm` is
    /// made a directory: the right key decrypts page 1, sees WAL mode and
    /// cannot open the shm file (`StoreError::Io`, SQLITE_CANTOPEN); a wrong
    /// key fails decryption first (`Unreadable`). Not permission-based, so
    /// it also holds when run as root.
    #[tokio::test(start_paused = true)]
    async fn io_error_does_not_quarantine() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let right = StoreKey::generate();
        let wrong = StoreKey::generate();
        {
            let (mut db, _) = corelib::db::try_open(dir, &right).unwrap();
            let mut s = LearnStore::memory();
            s.record_word("gstack");
            s.record_word("gstack");
            db.write(&s.take_dirty()).unwrap();
        }
        let shm = dir.join("learn.db-shm");
        assert!(!shm.exists(), "WAL side files are removed on close");
        std::fs::create_dir(&shm).unwrap();
        assert!(matches!(
            corelib::db::try_open(dir, &right),
            Err(StoreError::Io(_))
        ));
        assert!(matches!(
            corelib::db::try_open(dir, &wrong),
            Err(StoreError::Unreadable)
        ));

        // Keyring holds both items; the wrong one comes first.
        let fake = FakeBackend::default();
        let attrs = crate::keyring::key_attributes("learn-db-key");
        fake.insert(attrs.clone(), wrong.to_hex().as_bytes());
        fake.insert(attrs, right.to_hex().as_bytes());
        let store = new_store();
        let h = spawn_persistence_with(store.clone(), dir.to_path_buf(), fake.clone());
        // Several attempts, each failing with Io.
        tokio::time::sleep(Duration::from_secs(20)).await;
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        // learn.db is untouched (a -wal may appear), nothing set aside.
        assert!(names.contains(&"learn.db".to_owned()), "{names:?}");
        assert!(
            !names.iter().any(|n| n.starts_with("learn.db.unreadable-")),
            "{names:?}"
        );
        assert_eq!(h.stats().quarantined_files, 0);
        assert!(!h.stats().persistent);
        assert!(!lock(&store).is_persistent());
        assert_eq!(h.flush_now().await, Ok(0));
        assert!(fake.count("find") >= 3);
        assert_eq!(fake.count("create"), 0);

        // Once the I/O problem is gone, the original data is all there.
        std::fs::remove_dir(&shm).unwrap();
        h.nudge();
        wait_until("persistent", || h.stats().persistent).await;
        assert_eq!(lock(&store).personal_freq("gstack"), 2);
        assert_eq!(h.stats().quarantined_files, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn unreadable_db_is_quarantined() {
        let tmp = tempfile::tempdir().unwrap();
        // A database made with a key the keyring no longer holds.
        {
            let (_db, _) = corelib::db::try_open(tmp.path(), &StoreKey::generate()).unwrap();
        }
        let store = new_store();
        let fake = FakeBackend::default();
        let h = spawn_persistence_with(store, tmp.path().to_path_buf(), fake.clone());
        wait_until("persistent", || h.stats().persistent).await;
        assert_eq!(h.stats().quarantined_files, 1);
        let quarantined = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("learn.db.unreadable-")
            })
            .count();
        assert_eq!(quarantined, 1);
        // Purge removes the quarantined copy too.
        assert_eq!(h.purge().await, Ok(()));
        assert_eq!(h.stats().quarantined_files, 0);
    }

    fn batch(words: &[(&str, u64)], bigrams: &[(&str, &str, u64)]) -> VocabBatch {
        VocabBatch {
            words: words.iter().map(|&(w, c)| (w.to_owned(), c)).collect(),
            bigrams: bigrams
                .iter()
                .map(|&(a, b, c)| (a.to_owned(), b.to_owned(), c))
                .collect(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn import_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        let sha = [7u8; 32];
        let b = batch(&[("ako", 12), ("once", 1)], &[("ako", "naman", 3)]);
        assert_eq!(h.import(b.clone(), sha).await, Ok((2, 1)));
        assert_eq!(lock(&store).personal_freq("ako"), 12);
        assert_eq!(
            h.import(b, sha).await,
            Err("this file was already imported".to_owned())
        );
        // Not added twice.
        assert_eq!(lock(&store).personal_freq("ako"), 12);

        // Already on disk (count >= 2 only), with the import recorded.
        let key = key_of(&fake.secrets()[0]);
        let (db, snap) = corelib::db::try_open(tmp.path(), &key).unwrap();
        let words: Vec<_> = snap
            .vocab
            .iter()
            .map(|r| (r.word.as_str(), r.freq))
            .collect();
        assert_eq!(words, [("ako", 12)]);
        assert_eq!(snap.bigrams.len(), 1);
        assert!(db.has_import(&sha).unwrap());
        drop(db);

        // A restart still knows the file.
        drop(h);
        tokio::time::sleep(Duration::from_secs(1)).await;
        let store2 = new_store();
        let h2 = spawn_persistence_with(store2.clone(), tmp.path().to_path_buf(), fake.clone());
        wait_until("persistent", || h2.stats().persistent).await;
        assert_eq!(
            h2.import(batch(&[("ako", 12)], &[]), sha).await,
            Err("this file was already imported".to_owned())
        );
        assert_eq!(lock(&store2).personal_freq("ako"), 12);
        // A different file is fine.
        assert_eq!(
            h2.import(batch(&[("ako", 3)], &[]), [8u8; 32]).await,
            Ok((1, 0))
        );
        assert_eq!(lock(&store2).personal_freq("ako"), 15);
    }

    /// The rows cannot be written (a trigger aborts every vocab insert):
    /// nothing is recorded, memory is untouched, and the reply is the
    /// fixed write-failed text. Afterwards the same file imports.
    #[tokio::test(start_paused = true)]
    async fn import_commit_failure_records_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        let hex = String::from_utf8(fake.secrets()[0].clone()).unwrap();
        let c = rusqlite::Connection::open(tmp.path().join("learn.db")).unwrap();
        c.execute_batch(&format!("PRAGMA key = \"x'{hex}'\";"))
            .unwrap();
        c.execute_batch(
            "CREATE TRIGGER no_vocab BEFORE INSERT ON vocab \
             BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .unwrap();
        let sha = [9u8; 32];
        let b = batch(&[("ako", 12)], &[("ako", "naman", 3)]);
        assert_eq!(
            h.import(b.clone(), sha).await,
            Err("learning store write failed".to_owned())
        );
        assert_eq!(lock(&store).personal_freq("ako"), 0);
        assert_eq!(lock(&store).bigram_count("ako", "naman"), 0);
        let (db, snap) = corelib::db::try_open(tmp.path(), &key_of(&fake.secrets()[0])).unwrap();
        assert!(!db.has_import(&sha).unwrap());
        assert!(snap.vocab.is_empty() && snap.bigrams.is_empty());
        drop(db);

        c.execute_batch("DROP TRIGGER no_vocab;").unwrap();
        assert_eq!(h.import(b, sha).await, Ok((1, 1)));
        assert_eq!(lock(&store).personal_freq("ako"), 12);
    }

    /// A crash right after the reply: the counts and the record are already
    /// committed. Read through a second connection while the actor is
    /// still up and has not flushed (dropping it would flush). The typed,
    /// unflushed word proves no flush put them there.
    #[tokio::test(start_paused = true)]
    async fn import_is_on_disk_when_it_replies() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        lock(&store).record_word("gstack");
        lock(&store).record_word("gstack");
        let sha = [6u8; 32];
        let b = batch(&[("ako", 12), ("once", 1)], &[("ako", "naman", 3)]);
        assert_eq!(h.import(b, sha).await, Ok((2, 1)));
        let (db, snap) = corelib::db::try_open(tmp.path(), &key_of(&fake.secrets()[0])).unwrap();
        assert!(db.has_import(&sha).unwrap());
        let words: Vec<_> = snap
            .vocab
            .iter()
            .map(|r| (r.word.as_str(), r.freq))
            .collect();
        assert_eq!(words, [("ako", 12)]);
        assert_eq!(snap.bigrams.len(), 1);
        assert_eq!(snap.bigrams[0].count, 3);
        // In memory: imported, and the typed word still pending.
        assert_eq!(lock(&store).personal_freq("ako"), 12);
        assert_eq!(lock(&store).personal_freq("once"), 1);
        assert_eq!(lock(&store).personal_freq("gstack"), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn import_refused_when_not_persistent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = new_store();
        let fake = FakeBackend::default();
        fake.set_locked(true);
        let h = spawn_persistence_with(store.clone(), tmp.path().to_path_buf(), fake.clone());
        let b = batch(&[("ako", 12)], &[("ako", "naman", 3)]);
        let refused = Err("learning store not available; unlock your keyring and retry".to_owned());
        // During the first attempt and during a backoff wait.
        assert_eq!(h.import(b.clone(), [1u8; 32]).await, refused);
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(h.import(b.clone(), [1u8; 32]).await, refused);
        assert_eq!(lock(&store).personal_freq("ako"), 0);
        assert_eq!(lock(&store).bigram_count("ako", "naman"), 0);
        assert!(!tmp.path().join("learn.db").exists());

        // Once unlocked, the same file imports.
        fake.set_locked(false);
        h.nudge();
        wait_until("persistent", || h.stats().persistent).await;
        assert_eq!(h.import(b, [1u8; 32]).await, Ok((1, 1)));
        assert_eq!(lock(&store).personal_freq("ako"), 12);
    }

    /// A keyring call in flight is an Acquire state too.
    #[tokio::test(start_paused = true)]
    async fn import_refused_during_keyring_call() {
        let tmp = tempfile::tempdir().unwrap();
        let store = new_store();
        let fake = FakeBackend::default();
        fake.state().find_delay = Some(Duration::from_secs(5));
        let h = spawn_persistence_with(store.clone(), tmp.path().to_path_buf(), fake.clone());
        wait_until("attempt under way", || fake.count("find") == 1).await;
        assert_eq!(
            h.import(batch(&[("ako", 12)], &[]), [1u8; 32]).await,
            Err("learning store not available; unlock your keyring and retry".to_owned())
        );
        assert_eq!(lock(&store).personal_freq("ako"), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn handles_dropped_stops_actor_after_final_flush() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        lock(&store).record_word("gstack");
        lock(&store).record_word("gstack");
        drop(h);
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (_db, snap) = corelib::db::try_open(tmp.path(), &key_of(&fake.secrets()[0])).unwrap();
        assert_eq!(snap.vocab.len(), 1);
    }

    /// A new actor on the same files and keyring item (the old one stops
    /// once its handle is dropped, after a final flush).
    async fn restart(
        dir: &Path,
        fake: &FakeBackend,
        h: PersistHandle,
    ) -> (SharedStore, PersistHandle) {
        drop(h);
        tokio::time::sleep(Duration::from_secs(1)).await;
        let store = new_store();
        let h = spawn_persistence_with(store.clone(), dir.to_path_buf(), fake.clone());
        wait_until("persistent", || h.stats().persistent).await;
        (store, h)
    }

    #[tokio::test(start_paused = true)]
    async fn forget_when_persistent_deletes_and_survives_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        lock(&store).record_word("kadto");
        lock(&store).record_word("kadto");
        h.flush_now().await.unwrap();
        assert_eq!(h.forget_word("kadto".to_owned()).await, Ok(true));
        assert_eq!(lock(&store).personal_freq("kadto"), 0);

        let (store2, _h2) = restart(tmp.path(), &fake, h).await;
        assert_eq!(lock(&store2).personal_freq("kadto"), 0);
    }

    /// Store keys are lowercase; the word is lowercased once for both the
    /// database and memory.
    #[tokio::test(start_paused = true)]
    async fn forget_is_case_insensitive_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        lock(&store).record_word("kadto");
        lock(&store).record_word("kadto");
        h.flush_now().await.unwrap();
        assert_eq!(h.forget_word("KaDto".to_owned()).await, Ok(true));
        let (store2, _h2) = restart(tmp.path(), &fake, h).await;
        assert_eq!(lock(&store2).personal_freq("kadto"), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn forget_while_locked_is_refused_and_attach_keeps_word() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        lock(&store).record_word("kadto");
        lock(&store).record_word("kadto");
        h.flush_now().await.unwrap();
        drop(h);
        tokio::time::sleep(Duration::from_secs(1)).await;

        fake.clear_log();
        fake.state().locked_calls = 2;
        let store = new_store();
        let h = spawn_persistence_with(store.clone(), tmp.path().to_path_buf(), fake.clone());
        wait_until("a locked attempt", || fake.count("locked") >= 1).await;
        assert!(!h.stats().persistent);
        assert_eq!(
            h.forget_word("kadto".to_owned()).await,
            Err("unlock your keyring first".to_owned())
        );

        wait_until("persistent", || h.stats().persistent).await;
        assert!(lock(&store).personal_freq("kadto") >= 2);
        assert_eq!(h.forget_word("kadto".to_owned()).await, Ok(true));
        assert_eq!(lock(&store).personal_freq("kadto"), 0);
    }

    /// A keyring call in flight is an Acquire state too.
    #[tokio::test(start_paused = true)]
    async fn forget_refused_during_keyring_call_when_store_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        lock(&store).record_word("kadto");
        lock(&store).record_word("kadto");
        h.flush_now().await.unwrap();
        drop(h);
        tokio::time::sleep(Duration::from_secs(1)).await;

        fake.clear_log();
        fake.state().find_delay = Some(Duration::from_secs(5));
        let h = spawn_persistence_with(new_store(), tmp.path().to_path_buf(), fake.clone());
        wait_until("attempt under way", || fake.count("find") == 1).await;
        assert_eq!(
            h.forget_word("kadto".to_owned()).await,
            Err("unlock your keyring first".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn forget_without_store_file_is_memory_only() {
        let tmp = tempfile::tempdir().unwrap();
        let store = new_store();
        let fake = FakeBackend::default();
        fake.set_locked(true);
        let h = spawn_persistence_with(store.clone(), tmp.path().to_path_buf(), fake.clone());
        wait_until("a locked attempt", || fake.count("locked") >= 1).await;
        assert!(!tmp.path().join("learn.db").exists());
        lock(&store).record_word("kadto");
        lock(&store).record_word("kadto");
        assert_eq!(h.forget_word("kadto".to_owned()).await, Ok(true));
        assert_eq!(lock(&store).personal_freq("kadto"), 0);
        assert_eq!(h.forget_word("kadto".to_owned()).await, Ok(false));
    }

    #[tokio::test(start_paused = true)]
    async fn forget_queued_after_dirty_flush_stays_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        lock(&store).record_word("kadto");
        lock(&store).record_word("kadto");
        assert_eq!(h.forget_word("kadto".to_owned()).await, Ok(true));
        h.flush_now().await.unwrap();
        let (store2, _h2) = restart(tmp.path(), &fake, h).await;
        assert_eq!(lock(&store2).personal_freq("kadto"), 0);
    }

    /// The delete cannot run (a trigger aborts it): memory is untouched
    /// and the reply is the fixed failure text.
    #[tokio::test(start_paused = true)]
    async fn forget_db_failure_keeps_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        lock(&store).record_word("kadto");
        lock(&store).record_word("kadto");
        h.flush_now().await.unwrap();
        let hex = String::from_utf8(fake.secrets()[0].clone()).unwrap();
        let c = rusqlite::Connection::open(tmp.path().join("learn.db")).unwrap();
        c.execute_batch(&format!("PRAGMA key = \"x'{hex}'\";"))
            .unwrap();
        c.execute_batch(
            "CREATE TRIGGER no_forget BEFORE DELETE ON vocab \
             BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .unwrap();
        assert_eq!(
            h.forget_word("kadto".to_owned()).await,
            Err("could not forget".to_owned())
        );
        assert_eq!(lock(&store).personal_freq("kadto"), 2);
        assert!(h.stats().persistent);

        c.execute_batch("DROP TRIGGER no_forget;").unwrap();
        assert_eq!(h.forget_word("kadto".to_owned()).await, Ok(true));
        assert_eq!(lock(&store).personal_freq("kadto"), 0);
    }

    fn record_twice(store: &SharedStore, words: &[&str]) {
        let mut s = lock(store);
        for w in words {
            s.record_word(w);
            s.record_word(w);
        }
    }

    fn words(ws: &[&str]) -> Vec<String> {
        ws.iter().map(|w| (*w).to_owned()).collect()
    }

    /// One request forgets several words (any case) from memory and disk;
    /// the reply counts the distinct words that existed.
    #[tokio::test(start_paused = true)]
    async fn forget_words_batch_survives_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        record_twice(&store, &["kadto", "gstack", "bahay"]);
        lock(&store).bump_bigram("kadto", "bahay");
        lock(&store).bump_bigram("kadto", "bahay");
        h.flush_now().await.unwrap();
        assert_eq!(
            h.forget_words(words(&["KaDto", "gstack", "kadto", "nope"]))
                .await,
            Ok(2)
        );
        assert_eq!(lock(&store).personal_freq("kadto"), 0);
        assert_eq!(lock(&store).personal_freq("gstack"), 0);
        assert_eq!(lock(&store).personal_freq("bahay"), 2);

        let (store2, h2) = restart(tmp.path(), &fake, h).await;
        assert_eq!(lock(&store2).personal_freq("kadto"), 0);
        assert_eq!(lock(&store2).personal_freq("gstack"), 0);
        assert_eq!(lock(&store2).personal_freq("bahay"), 2);
        assert_eq!(lock(&store2).bigram_count("kadto", "bahay"), 0);
        assert_eq!(h2.forget_words(words(&["kadto"])).await, Ok(0));
    }

    #[tokio::test(start_paused = true)]
    async fn forget_words_refused_while_locked() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, fake, h) = persistent_actor(tmp.path()).await;
        record_twice(&store, &["kadto", "gstack"]);
        h.flush_now().await.unwrap();
        drop(h);
        tokio::time::sleep(Duration::from_secs(1)).await;

        fake.clear_log();
        fake.state().locked_calls = 2;
        let store = new_store();
        let h = spawn_persistence_with(store.clone(), tmp.path().to_path_buf(), fake.clone());
        wait_until("a locked attempt", || fake.count("locked") >= 1).await;
        assert!(!h.stats().persistent);
        assert_eq!(
            h.forget_words(words(&["kadto", "gstack"])).await,
            Err("unlock your keyring first".to_owned())
        );

        wait_until("persistent", || h.stats().persistent).await;
        assert!(lock(&store).personal_freq("kadto") >= 2);
        assert!(lock(&store).personal_freq("gstack") >= 2);
        assert_eq!(h.forget_words(words(&["kadto", "gstack"])).await, Ok(2));
    }

    #[tokio::test(start_paused = true)]
    async fn forget_words_memory_only_without_store_file() {
        let tmp = tempfile::tempdir().unwrap();
        let store = new_store();
        let fake = FakeBackend::default();
        fake.set_locked(true);
        let h = spawn_persistence_with(store.clone(), tmp.path().to_path_buf(), fake.clone());
        wait_until("a locked attempt", || fake.count("locked") >= 1).await;
        assert!(!tmp.path().join("learn.db").exists());
        record_twice(&store, &["kadto", "gstack", "bahay"]);
        assert_eq!(
            h.forget_words(words(&["KADTO", "gstack", "nope"])).await,
            Ok(2)
        );
        assert_eq!(lock(&store).personal_freq("kadto"), 0);
        assert_eq!(lock(&store).personal_freq("gstack"), 0);
        assert_eq!(lock(&store).personal_freq("bahay"), 2);
        assert_eq!(h.forget_words(words(&["kadto", "gstack"])).await, Ok(0));
        assert!(!tmp.path().join("learn.db").exists());
    }
}
