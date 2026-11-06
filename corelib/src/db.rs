//! Encrypted (SQLCipher) database writer. Only called off the typing path,
//! with no store lock held.

use crate::key::StoreKey;
use crate::layout::LayoutId;
use rusqlite::{Connection, ErrorCode};
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

const DB_NAME: &str = "learn.db";
const PRUNE_AGE_SECS: i64 = 90 * 24 * 3600;
const VOCAB_CAP: i64 = 200_000;
const BIGRAM_CAP: i64 = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VocabRow {
    pub word: String,
    pub freq: u64,
    pub last_seen: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlacklistRow {
    pub orig: String,
    pub cand: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigramRow {
    pub w1: String,
    pub w2: String,
    pub count: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlipRow {
    pub layout: LayoutId,
    pub typed: char,
    pub intended: char,
    pub count: u64,
}

/// Everything loaded from the database at attach time.
#[derive(Debug, Default, Clone)]
pub struct Snapshot {
    pub vocab: Vec<VocabRow>,
    pub blacklist: Vec<BlacklistRow>,
    pub bigrams: Vec<BigramRow>,
    pub slips: Vec<SlipRow>,
}

/// Rows to upsert, carrying current absolute totals.
#[derive(Debug, Default, Clone)]
pub struct DirtyBatch {
    pub vocab: Vec<VocabRow>,
    pub blacklist: Vec<BlacklistRow>,
    pub bigrams: Vec<BigramRow>,
    pub slips: Vec<SlipRow>,
}

#[derive(Debug)]
pub enum AttachOutcome {
    Opened,
    Created,
    /// Path of the quarantined (renamed) old file; a fresh `learn.db` exists.
    QuarantinedAndCreated(PathBuf),
}

#[derive(Debug)]
pub enum StoreError {
    NotSqlcipher,
    Unreadable,
    Io(String),
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}
impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}

pub struct Db {
    conn: Connection,
    dir: PathBuf,
}

fn layout_str(l: LayoutId) -> &'static str {
    match l {
        LayoutId::Colemak => "colemak",
        LayoutId::Qwerty => "qwerty",
        LayoutId::OskTap => "osktap",
    }
}

fn layout_parse(s: &str) -> Option<LayoutId> {
    match s {
        "colemak" => Some(LayoutId::Colemak),
        "qwerty" => Some(LayoutId::Qwerty),
        "osktap" => Some(LayoutId::OskTap),
        _ => None,
    }
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn sha_hex(sha: &[u8; 32]) -> String {
    sha.iter().map(|b| format!("{b:02x}")).collect()
}

/// The database file in `dir` (whether or not it exists).
pub fn db_path(dir: &Path) -> PathBuf {
    dir.join(DB_NAME)
}

fn side_file(dir: &Path, suffix: &str) -> PathBuf {
    dir.join(format!("{DB_NAME}{suffix}"))
}

fn chmod_600(p: &Path) {
    if p.exists() {
        let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o600));
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS vocab(word TEXT PRIMARY KEY, freq INTEGER NOT NULL, last_seen INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS slips(layout TEXT NOT NULL, typed TEXT NOT NULL, intended TEXT NOT NULL, count INTEGER NOT NULL, PRIMARY KEY(layout, typed, intended));
CREATE TABLE IF NOT EXISTS blacklist(orig TEXT NOT NULL, cand TEXT NOT NULL, PRIMARY KEY(orig, cand));
CREATE TABLE IF NOT EXISTS bigram(w1 TEXT NOT NULL, w2 TEXT NOT NULL, count INTEGER NOT NULL, PRIMARY KEY(w1, w2));
CREATE TABLE IF NOT EXISTS imports(sha256 TEXT PRIMARY KEY, at INTEGER);
";

const KEY_PREFIX: &str = "PRAGMA key = \"x'";
const KEY_SUFFIX: &str = "'\";";

/// The PRAGMA key statement, built in place: one exact allocation, so no
/// freed intermediate buffer ever holds key hex.
fn key_statement(key: &StoreKey) -> Zeroizing<String> {
    let hex = key.to_hex();
    let mut s = Zeroizing::new(String::with_capacity(
        KEY_PREFIX.len() + hex.len() + KEY_SUFFIX.len(),
    ));
    s.push_str(KEY_PREFIX);
    s.push_str(&hex);
    s.push_str(KEY_SUFFIX);
    s
}

/// Runs the key statement. An error's text can quote the SQL (and with it
/// the key hex), so it is replaced by a fixed one.
fn exec_key(conn: &Connection, stmt: &str) -> Result<(), StoreError> {
    conn.execute_batch(stmt)
        .map_err(|_| StoreError::Io("key statement failed".into()))
}

/// Open (creating if absent) without ever quarantining.
pub fn try_open(dir: &Path, key: &StoreKey) -> Result<(Db, Snapshot), StoreError> {
    open_inner(dir, key).map(|(d, s, _)| (d, s))
}

/// Creates `dir` (0700) and tightens it to 0700 if it already existed with
/// a looser mode; the tightening is best effort.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    Ok(())
}

fn open_inner(dir: &Path, key: &StoreKey) -> Result<(Db, Snapshot, bool), StoreError> {
    create_private_dir(dir)?;
    let path = dir.join(DB_NAME);
    let existed = path.exists();
    if !existed {
        // Create the empty file with 0600 from the start (no umask games).
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)?;
    }
    chmod_600(&path);
    let conn = Connection::open(&path)?;

    // PRAGMA key must be the first statement on the connection.
    exec_key(&conn, &key_statement(key))?;

    let ver: Option<String> = conn
        .query_row("PRAGMA cipher_version", [], |r| r.get(0))
        .ok();
    if ver.is_none_or(|v| v.is_empty()) {
        return Err(StoreError::NotSqlcipher);
    }
    conn.execute_batch("PRAGMA busy_timeout = 2000;")?;
    match conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    }) {
        Ok(_) => {}
        Err(rusqlite::Error::SqliteFailure(e, _))
            if matches!(e.code, ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt) =>
        {
            return Err(StoreError::Unreadable);
        }
        Err(e) => return Err(e.into()),
    }
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA secure_delete=ON; PRAGMA temp_store=MEMORY;",
    )?;
    conn.execute_batch(SCHEMA)?;
    prune(&conn)?;
    let snap = load(&conn)?;
    for s in ["-wal", "-shm"] {
        chmod_600(&side_file(dir, s));
    }
    Ok((
        Db {
            conn,
            dir: dir.to_path_buf(),
        },
        snap,
        existed,
    ))
}

/// `try_open`, plus quarantine-and-recreate when the file is unreadable.
pub fn attach(dir: &Path, key: &StoreKey) -> Result<(Db, Snapshot, AttachOutcome), StoreError> {
    match open_inner(dir, key) {
        Ok((db, snap, existed)) => {
            let out = if existed {
                AttachOutcome::Opened
            } else {
                AttachOutcome::Created
            };
            Ok((db, snap, out))
        }
        Err(StoreError::Unreadable) => {
            let ts = now();
            let mut n = 0u32;
            let target = loop {
                let name = if n == 0 {
                    format!("{DB_NAME}.unreadable-{ts}")
                } else {
                    format!("{DB_NAME}.unreadable-{ts}-{n}")
                };
                let t = dir.join(name);
                if !t.exists() {
                    break t;
                }
                n += 1;
            };
            fs::rename(dir.join(DB_NAME), &target)?;
            for s in ["-wal", "-shm"] {
                let from = side_file(dir, s);
                if from.exists() {
                    let mut to = target.clone().into_os_string();
                    to.push(s);
                    fs::rename(from, PathBuf::from(to))?;
                }
            }
            let (db, snap, _) = open_inner(dir, key)?;
            Ok((db, snap, AttachOutcome::QuarantinedAndCreated(target)))
        }
        Err(e) => Err(e),
    }
}

fn prune(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM vocab WHERE freq <= 2 AND last_seen < ?1",
        [now() - PRUNE_AGE_SECS],
    )?;
    conn.execute(
        "DELETE FROM vocab WHERE word IN (SELECT word FROM vocab ORDER BY freq ASC, last_seen ASC \
         LIMIT max(0, (SELECT count(*) FROM vocab) - ?1))",
        [VOCAB_CAP],
    )?;
    conn.execute(
        "DELETE FROM bigram WHERE rowid IN (SELECT rowid FROM bigram ORDER BY count ASC \
         LIMIT max(0, (SELECT count(*) FROM bigram) - ?1))",
        [BIGRAM_CAP],
    )?;
    Ok(())
}

fn load(conn: &Connection) -> rusqlite::Result<Snapshot> {
    let mut s = Snapshot::default();
    let mut q = conn.prepare("SELECT word, freq, last_seen FROM vocab")?;
    for r in q.query_map([], |r| {
        Ok(VocabRow {
            word: r.get(0)?,
            freq: r.get::<_, i64>(1)?.max(0) as u64,
            last_seen: r.get(2)?,
        })
    })? {
        s.vocab.push(r?);
    }
    let mut q = conn.prepare("SELECT orig, cand FROM blacklist")?;
    for r in q.query_map([], |r| {
        Ok(BlacklistRow {
            orig: r.get(0)?,
            cand: r.get(1)?,
        })
    })? {
        s.blacklist.push(r?);
    }
    let mut q = conn.prepare("SELECT w1, w2, count FROM bigram")?;
    for r in q.query_map([], |r| {
        Ok(BigramRow {
            w1: r.get(0)?,
            w2: r.get(1)?,
            count: r.get::<_, i64>(2)?.max(0) as u64,
        })
    })? {
        s.bigrams.push(r?);
    }
    let mut q = conn.prepare("SELECT layout, typed, intended, count FROM slips")?;
    for r in q.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })? {
        let (l, t, i, c) = r?;
        if let (Some(layout), Some(typed), Some(intended)) =
            (layout_parse(&l), t.chars().next(), i.chars().next())
        {
            s.slips.push(SlipRow {
                layout,
                typed,
                intended,
                count: c.max(0) as u64,
            });
        }
    }
    Ok(s)
}

/// Upsert the batch's rows inside `tx` (absolute totals).
fn upsert(tx: &rusqlite::Transaction<'_>, batch: &DirtyBatch) -> rusqlite::Result<()> {
    let mut v = tx.prepare_cached(
        "INSERT INTO vocab(word, freq, last_seen) VALUES(?1, ?2, ?3) \
             ON CONFLICT(word) DO UPDATE SET freq = excluded.freq, last_seen = excluded.last_seen",
    )?;
    for r in &batch.vocab {
        v.execute(rusqlite::params![r.word, to_i64(r.freq), r.last_seen])?;
    }
    let mut b = tx.prepare_cached("INSERT OR IGNORE INTO blacklist(orig, cand) VALUES(?1, ?2)")?;
    for r in &batch.blacklist {
        b.execute(rusqlite::params![r.orig, r.cand])?;
    }
    let mut g = tx.prepare_cached(
        "INSERT INTO bigram(w1, w2, count) VALUES(?1, ?2, ?3) \
             ON CONFLICT(w1, w2) DO UPDATE SET count = excluded.count",
    )?;
    for r in &batch.bigrams {
        g.execute(rusqlite::params![r.w1, r.w2, to_i64(r.count)])?;
    }
    let mut s = tx.prepare_cached(
        "INSERT INTO slips(layout, typed, intended, count) VALUES(?1, ?2, ?3, ?4) \
             ON CONFLICT(layout, typed, intended) DO UPDATE SET count = excluded.count",
    )?;
    for r in &batch.slips {
        s.execute(rusqlite::params![
            layout_str(r.layout),
            r.typed.to_string(),
            r.intended.to_string(),
            to_i64(r.count)
        ])?;
    }
    Ok(())
}

fn batch_len(b: &DirtyBatch) -> usize {
    b.vocab.len() + b.blacklist.len() + b.bigrams.len() + b.slips.len()
}

impl Db {
    /// Upsert the batch in one transaction; returns the row count. Writes are
    /// absolute (the batch carries current totals).
    pub fn write(&mut self, batch: &DirtyBatch) -> Result<usize, StoreError> {
        let tx = self.conn.transaction()?;
        upsert(&tx, batch)?;
        tx.commit()?;
        for s in ["-wal", "-shm"] {
            chmod_600(&side_file(&self.dir, s));
        }
        Ok(batch_len(batch))
    }

    /// Delete one word's rows; see `forget_words`. `w` must already be
    /// lowercased.
    pub fn forget_word(&mut self, w: &str) -> Result<(), StoreError> {
        self.forget_words(&[w.to_owned()])
    }

    /// Delete the words' vocab rows and every bigram and blacklist row
    /// naming any of them, in ONE transaction, binding at most 500 words per
    /// statement. The words must already be lowercased.
    pub fn forget_words(&mut self, ws: &[String]) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for chunk in ws.chunks(500) {
            let marks = vec!["?"; chunk.len()].join(",");
            tx.execute(
                &format!("DELETE FROM vocab WHERE word IN ({marks})"),
                rusqlite::params_from_iter(chunk.iter()),
            )?;
            // Two IN lists, so the same words are bound twice.
            let twice = rusqlite::params_from_iter(chunk.iter().chain(chunk.iter()));
            tx.execute(
                &format!("DELETE FROM bigram WHERE w1 IN ({marks}) OR w2 IN ({marks})"),
                twice,
            )?;
            let twice = rusqlite::params_from_iter(chunk.iter().chain(chunk.iter()));
            tx.execute(
                &format!("DELETE FROM blacklist WHERE orig IN ({marks}) OR cand IN ({marks})"),
                twice,
            )?;
        }
        tx.commit()?;
        for s in ["-wal", "-shm"] {
            chmod_600(&side_file(&self.dir, s));
        }
        Ok(())
    }

    /// `write` plus the import record of `sha`, in ONE transaction: either
    /// the rows and the record are both on disk, or neither is.
    pub fn write_import(
        &mut self,
        batch: &DirtyBatch,
        sha: &[u8; 32],
    ) -> Result<usize, StoreError> {
        let tx = self.conn.transaction()?;
        upsert(&tx, batch)?;
        tx.execute(
            "INSERT OR IGNORE INTO imports(sha256, at) VALUES(?1, ?2)",
            rusqlite::params![sha_hex(sha), now()],
        )?;
        tx.commit()?;
        for s in ["-wal", "-shm"] {
            chmod_600(&side_file(&self.dir, s));
        }
        Ok(batch_len(batch))
    }

    /// Whether a vocabulary file with this SHA-256 was already imported.
    pub fn has_import(&self, sha: &[u8; 32]) -> Result<bool, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM imports WHERE sha256 = ?1",
            [sha_hex(sha)],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Remember an imported file's SHA-256 (idempotent).
    pub fn record_import(&mut self, sha: &[u8; 32]) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO imports(sha256, at) VALUES(?1, ?2)",
            rusqlite::params![sha_hex(sha), now()],
        )?;
        for s in ["-wal", "-shm"] {
            chmod_600(&side_file(&self.dir, s));
        }
        Ok(())
    }

    #[cfg(test)]
    fn cipher_version(&self) -> String {
        self.conn
            .query_row("PRAGMA cipher_version", [], |r| r.get(0))
            .unwrap_or_default()
    }
}

/// Delete the database, its WAL/SHM and every quarantined copy. Works whether
/// or not a `Db` is open; missing files are not errors.
pub fn purge_files(dir: &Path) -> io::Result<()> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in rd {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == DB_NAME
            || name == "learn.db-wal"
            || name == "learn.db-shm"
            || name.starts_with("learn.db.unreadable-")
        {
            match fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learning::LearnStore;
    use crate::scoring::PersonalLookup;
    use std::fs;

    fn two(s: &mut LearnStore, w: &str) {
        s.record_word(w);
        s.record_word(w);
    }

    fn reload(dir: &Path, k: &StoreKey) -> LearnStore {
        let (_db, snap, _) = attach(dir, k).unwrap();
        let mut m = LearnStore::memory();
        m.merge_snapshot(snap);
        m
    }

    #[test]
    fn cipher_version_present() {
        let d = tempfile::tempdir().unwrap();
        let (db, _, out) = attach(d.path(), &StoreKey::generate()).unwrap();
        assert!(matches!(out, AttachOutcome::Created));
        assert!(!db.cipher_version().is_empty());
    }

    #[test]
    fn file_is_encrypted() {
        let d = tempfile::tempdir().unwrap();
        let k = StoreKey::generate();
        let (mut db, _, _) = attach(d.path(), &k).unwrap();
        let mut c = LearnStore::memory();
        two(&mut c, "gstack");
        db.write(&c.take_dirty()).unwrap();
        for f in ["learn.db", "learn.db-wal"] {
            if let Ok(b) = fs::read(d.path().join(f)) {
                assert!(!b.windows(6).any(|w| w == b"gstack"), "{f}");
                assert!(!b.starts_with(b"SQLite format 3\0"), "{f}");
            }
        }
    }

    #[test]
    fn roundtrip_across_reopen() {
        let d = tempfile::tempdir().unwrap();
        let k = StoreKey::generate();
        {
            let (mut db, _, _) = attach(d.path(), &k).unwrap();
            let mut c = LearnStore::memory();
            two(&mut c, "gstack");
            c.record_blacklist("teh", "the");
            c.bump_bigram("a", "b");
            c.bump_bigram("a", "b");
            db.write(&c.take_dirty()).unwrap();
        }
        let m = reload(d.path(), &k);
        assert_eq!(m.personal_freq("gstack"), 2);
        assert!(m.is_blacklisted("teh", "the"));
        assert_eq!(m.bigram_count("a", "b"), 2);
    }

    #[test]
    fn slips_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let k = StoreKey::generate();
        {
            let (mut db, _, _) = attach(d.path(), &k).unwrap();
            let mut c = LearnStore::memory();
            for l in [LayoutId::Colemak, LayoutId::Qwerty, LayoutId::OskTap] {
                c.record_slip(l, 'n', 'e');
                c.record_slip(l, 'n', 'e');
            }
            assert_eq!(db.write(&c.take_dirty()).unwrap(), 3);
        }
        let m = reload(d.path(), &k);
        assert_eq!(m.slip_count(LayoutId::Colemak, 'n', 'e'), 2);
        assert_eq!(m.slip_count(LayoutId::OskTap, 'n', 'e'), 2);
        assert_eq!(m.slip_count(LayoutId::Qwerty, 'e', 'n'), 0);
    }

    #[test]
    fn try_open_wrong_key_leaves_file() {
        let d = tempfile::tempdir().unwrap();
        let (k1, k2) = (StoreKey::generate(), StoreKey::generate());
        drop(attach(d.path(), &k1).unwrap());
        let before = fs::read(d.path().join("learn.db")).unwrap();
        assert!(matches!(
            try_open(d.path(), &k2),
            Err(StoreError::Unreadable)
        ));
        assert_eq!(before, fs::read(d.path().join("learn.db")).unwrap());
        assert!(try_open(d.path(), &k1).is_ok());
    }

    #[test]
    fn wrong_key_quarantines() {
        let d = tempfile::tempdir().unwrap();
        let (k1, k2) = (StoreKey::generate(), StoreKey::generate());
        drop(attach(d.path(), &k1).unwrap());
        let (_db, _, out) = attach(d.path(), &k2).unwrap();
        match out {
            AttachOutcome::QuarantinedAndCreated(p) => {
                assert!(p.exists());
                assert!(p.to_string_lossy().contains("learn.db.unreadable-"));
            }
            _ => panic!("expected quarantine"),
        }
        drop(_db);
        assert!(matches!(
            attach(d.path(), &k2).unwrap().2,
            AttachOutcome::Opened
        ));
    }

    #[test]
    fn corrupt_file_quarantines() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("learn.db"), vec![0x5au8; 8192]).unwrap();
        let out = attach(d.path(), &StoreKey::generate()).unwrap().2;
        assert!(matches!(out, AttachOutcome::QuarantinedAndCreated(_)));
    }

    #[test]
    fn busy_file_not_quarantined() {
        let d = tempfile::tempdir().unwrap();
        let k = StoreKey::generate();
        drop(attach(d.path(), &k).unwrap());
        // Rollback-journal mode + BEGIN EXCLUSIVE + a write really blocks readers.
        let path = d.path().join("learn.db");
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(&format!("PRAGMA key = \"x'{}'\";", *k.to_hex()))
            .unwrap();
        c.execute_batch("PRAGMA journal_mode=DELETE;").unwrap();
        c.execute_batch("BEGIN EXCLUSIVE; INSERT INTO vocab VALUES('zz',5,1);")
            .unwrap();
        let r = attach(d.path(), &k);
        assert!(matches!(r, Err(StoreError::Io(_))), "{:?}", r.map(|_| ()));
        assert!(path.exists());
        let n = fs::read_dir(d.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains("unreadable")
            })
            .count();
        assert_eq!(n, 0);
        drop(c);
    }

    #[test]
    fn rewrite_is_absolute() {
        let d = tempfile::tempdir().unwrap();
        let k = StoreKey::generate();
        {
            let (mut db, _, _) = attach(d.path(), &k).unwrap();
            let mut c = LearnStore::memory();
            two(&mut c, "x");
            db.write(&c.take_dirty()).unwrap();
            c.record_word("x");
            db.write(&c.take_dirty()).unwrap();
        }
        assert_eq!(reload(d.path(), &k).personal_freq("x"), 3);
    }

    #[test]
    fn permissions_restrictive() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("a").join("store");
        let (mut db, _, _) = attach(&dir, &StoreKey::generate()).unwrap();
        let mut c = LearnStore::memory();
        two(&mut c, "x");
        db.write(&c.take_dirty()).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("learn.db")), 0o600);
        for f in ["learn.db-wal", "learn.db-shm"] {
            if dir.join(f).exists() {
                assert_eq!(mode(&dir.join(f)), 0o600, "{f}");
            }
        }

        // A dir that already exists with a loose mode (a hand-made
        // `mkdir -p .../correctiond/lexicons` under umask 022) ends up 0700.
        let loose = d.path().join("b").join("correctiond");
        fs::create_dir_all(loose.join("lexicons")).unwrap();
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(mode(&loose), 0o755);
        let _ = attach(&loose, &StoreKey::generate()).unwrap();
        assert_eq!(mode(&loose), 0o700);
    }

    #[test]
    fn key_statement_is_one_exact_allocation() {
        let k = StoreKey::generate();
        let s = key_statement(&k);
        assert_eq!(*s, format!("PRAGMA key = \"x'{}'\";", *k.to_hex()));
        assert_eq!(s.capacity(), s.len());
    }

    #[test]
    fn key_statement_error_never_quotes_the_sql() {
        let conn = Connection::open_in_memory().unwrap();
        // Malformed on purpose, so prepare fails and the error could quote it.
        let bad = format!("PRAGMA key = \"x'{}' garbage(;", "ab".repeat(32));
        match exec_key(&conn, &bad) {
            Err(StoreError::Io(t)) => assert_eq!(t, "key statement failed"),
            other => panic!("unexpected {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn purge_files_unconditional() {
        let d = tempfile::tempdir().unwrap();
        for f in [
            "learn.db",
            "learn.db-wal",
            "learn.db-shm",
            "learn.db.unreadable-123",
            "keep",
        ] {
            fs::write(d.path().join(f), b"x").unwrap();
        }
        purge_files(d.path()).unwrap();
        let left: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("keep")]);
        purge_files(&d.path().join("missing")).unwrap();
    }

    #[test]
    fn write_import_is_one_transaction() {
        let d = tempfile::tempdir().unwrap();
        let k = StoreKey::generate();
        let (ok, bad) = ([3u8; 32], [4u8; 32]);
        let rows = |w: &str, f: u64| DirtyBatch {
            vocab: vec![VocabRow {
                word: w.into(),
                freq: f,
                last_seen: now(),
            }],
            bigrams: vec![BigramRow {
                w1: w.into(),
                w2: "naman".into(),
                count: f,
            }],
            ..DirtyBatch::default()
        };
        {
            let (mut db, _, _) = attach(d.path(), &k).unwrap();
            assert_eq!(db.write_import(&rows("ako", 12), &ok).unwrap(), 2);
            assert!(db.has_import(&ok).unwrap());
            // Absolute, like `write`.
            assert_eq!(db.write_import(&rows("ako", 13), &[5u8; 32]).unwrap(), 2);
            // The import record fails after the rows went in: all or nothing.
            db.conn
                .execute_batch(
                    "CREATE TRIGGER no_imports BEFORE INSERT ON imports \
                     BEGIN SELECT RAISE(ABORT, 'injected'); END;",
                )
                .unwrap();
            assert!(db.write_import(&rows("mabuti", 5), &bad).is_err());
            assert!(!db.has_import(&bad).unwrap());
        }
        let (db, snap) = try_open(d.path(), &k).unwrap();
        let words: Vec<_> = snap
            .vocab
            .iter()
            .map(|r| (r.word.as_str(), r.freq))
            .collect();
        assert_eq!(words, [("ako", 13)]);
        assert_eq!(snap.bigrams.len(), 1);
        assert!(db.has_import(&ok).unwrap());
        assert!(!db.has_import(&bad).unwrap());
    }

    #[test]
    fn forget_word_deletes_rows_and_survives_reopen() {
        let d = tempfile::tempdir().unwrap();
        let k = StoreKey::generate();
        {
            let (mut db, _, _) = attach(d.path(), &k).unwrap();
            let batch = DirtyBatch {
                vocab: vec![
                    VocabRow {
                        word: "kadto".into(),
                        freq: 3,
                        last_seen: now(),
                    },
                    VocabRow {
                        word: "ako".into(),
                        freq: 5,
                        last_seen: now(),
                    },
                ],
                bigrams: vec![
                    BigramRow {
                        w1: "ako".into(),
                        w2: "kadto".into(),
                        count: 2,
                    },
                    BigramRow {
                        w1: "kadto".into(),
                        w2: "na".into(),
                        count: 2,
                    },
                    BigramRow {
                        w1: "ako".into(),
                        w2: "na".into(),
                        count: 2,
                    },
                ],
                ..DirtyBatch::default()
            };
            db.write(&batch).unwrap();
            db.forget_word("kadto").unwrap();
        }
        let (_db, snap) = try_open(d.path(), &k).unwrap();
        assert!(snap.vocab.iter().all(|r| r.word != "kadto"));
        assert!(snap.vocab.iter().any(|r| r.word == "ako"));
        assert!(snap
            .bigrams
            .iter()
            .all(|r| r.w1 != "kadto" && r.w2 != "kadto"));
        assert_eq!(snap.bigrams.len(), 1);
    }

    #[test]
    fn forget_word_deletes_blacklist_rows_and_survives_reopen() {
        let d = tempfile::tempdir().unwrap();
        let k = StoreKey::generate();
        {
            let (mut db, _, _) = attach(d.path(), &k).unwrap();
            let batch = DirtyBatch {
                blacklist: vec![
                    BlacklistRow {
                        orig: "teh".into(),
                        cand: "the".into(),
                    },
                    BlacklistRow {
                        orig: "hte".into(),
                        cand: "teh".into(),
                    },
                    BlacklistRow {
                        orig: "adn".into(),
                        cand: "and".into(),
                    },
                ],
                ..DirtyBatch::default()
            };
            db.write(&batch).unwrap();
            db.forget_word("teh").unwrap();
        }
        let (_db, snap) = try_open(d.path(), &k).unwrap();
        assert!(snap
            .blacklist
            .iter()
            .all(|r| r.orig != "teh" && r.cand != "teh"));
        assert_eq!(snap.blacklist.len(), 1);
    }

    #[test]
    fn forget_words_chunks_and_reopen() {
        let d = tempfile::tempdir().unwrap();
        let k = StoreKey::generate();
        let words: Vec<String> = (0..1200).map(|i| format!("w{i}")).collect();
        {
            let (mut db, _, _) = attach(d.path(), &k).unwrap();
            let batch = DirtyBatch {
                vocab: words
                    .iter()
                    .map(|w| VocabRow {
                        word: w.clone(),
                        freq: 3,
                        last_seen: now(),
                    })
                    .collect(),
                bigrams: vec![
                    BigramRow {
                        w1: "w5".into(),
                        w2: "keep".into(),
                        count: 2,
                    },
                    BigramRow {
                        w1: "keep".into(),
                        w2: "w1050".into(),
                        count: 2,
                    },
                    BigramRow {
                        w1: "keep".into(),
                        w2: "w1150".into(),
                        count: 2,
                    },
                ],
                blacklist: vec![
                    BlacklistRow {
                        orig: "teh".into(),
                        cand: "w1099".into(),
                    },
                    BlacklistRow {
                        orig: "adn".into(),
                        cand: "and".into(),
                    },
                ],
                ..DirtyBatch::default()
            };
            db.write(&batch).unwrap();
            db.forget_words(&words[..1100]).unwrap();
        }
        let (_db, snap) = try_open(d.path(), &k).unwrap();
        let mut left: Vec<&str> = snap.vocab.iter().map(|r| r.word.as_str()).collect();
        left.sort();
        let mut want: Vec<&str> = words[1100..].iter().map(|s| s.as_str()).collect();
        want.sort();
        assert_eq!(left, want);
        assert_eq!(snap.bigrams.len(), 1);
        assert_eq!(snap.bigrams[0].w2, "w1150");
        assert_eq!(snap.blacklist.len(), 1);
        assert_eq!(snap.blacklist[0].orig, "adn");
    }

    #[test]
    fn imports_recorded_across_reopen() {
        let d = tempfile::tempdir().unwrap();
        let k = StoreKey::generate();
        let (a, b) = ([1u8; 32], [2u8; 32]);
        {
            let (mut db, _, _) = attach(d.path(), &k).unwrap();
            assert!(!db.has_import(&a).unwrap());
            db.record_import(&a).unwrap();
            // Recording twice is not an error.
            db.record_import(&a).unwrap();
            assert!(db.has_import(&a).unwrap());
            assert!(!db.has_import(&b).unwrap());
        }
        let (db, _) = try_open(d.path(), &k).unwrap();
        assert!(db.has_import(&a).unwrap());
        assert!(!db.has_import(&b).unwrap());
        // The table holds hex digests only.
        let hex: String = db
            .conn
            .query_row("SELECT sha256 FROM imports", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hex, "01".repeat(32));
        // Purge removes the database, and with it the import records.
        drop(db);
        purge_files(d.path()).unwrap();
        let (db, _) = try_open(d.path(), &k).unwrap();
        assert!(!db.has_import(&a).unwrap());
    }

    #[test]
    fn prune_drops_stale_and_caps() {
        let d = tempfile::tempdir().unwrap();
        let k = StoreKey::generate();
        {
            let (mut db, _, _) = attach(d.path(), &k).unwrap();
            let b = DirtyBatch {
                vocab: vec![
                    VocabRow {
                        word: "stale".into(),
                        freq: 2,
                        last_seen: 1,
                    },
                    VocabRow {
                        word: "oldbutfrequent".into(),
                        freq: 50,
                        last_seen: 1,
                    },
                ],
                ..DirtyBatch::default()
            };
            db.write(&b).unwrap();
        }
        let m = reload(d.path(), &k);
        assert_eq!(m.personal_freq("stale"), 0);
        assert_eq!(m.personal_freq("oldbutfrequent"), 50);
    }
}
