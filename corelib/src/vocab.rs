//! Reader for the "correctiond vocab v1" import format (docs/vocab-format.md).
//!
//! The engine re-validates everything a converter wrote: words are
//! lowercased, anything `is_learnable` rejects is dropped, counts are
//! clamped and duplicates summed. Errors and stats carry no file content.

use crate::policy::{is_learnable, Mode};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const HEADER: &str = "#correctiond-vocab v1";
/// Files over this many bytes are rejected whole.
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
/// Files over this many records are rejected whole.
pub const MAX_RECORDS: usize = 200_000;
const MAX_COUNT: u64 = 1_000_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VocabBatch {
    pub words: Vec<(String, u64)>,
    pub bigrams: Vec<(String, String, u64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VocabError {
    /// The first non-empty line is not exactly `HEADER`.
    BadHeader,
    /// Over `MAX_BYTES` or `MAX_RECORDS`.
    TooLarge,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VocabStats {
    /// Records not imported: malformed, or holding a word that is not
    /// learnable.
    pub skipped: u32,
}

/// A positive decimal integer, clamped to 1..=MAX_COUNT; `None` otherwise.
fn count(field: &str) -> Option<u64> {
    if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // All digits: a parse failure can only be overflow.
    let n = field.parse::<u64>().unwrap_or(u64::MAX);
    (n > 0).then(|| n.min(MAX_COUNT))
}

/// The lowercased word, if it is learnable as written.
fn word(field: &str) -> Option<String> {
    is_learnable(field, Mode::Silent).then(|| field.to_lowercase())
}

fn add(total: &mut u64, n: u64) {
    *total = total.saturating_add(n).min(MAX_COUNT);
}

/// Parse a vocab v1 file, applying every reader rule of the format.
pub fn parse_vocab(text: &str) -> Result<(VocabBatch, VocabStats), VocabError> {
    if text.len() > MAX_BYTES {
        return Err(VocabError::TooLarge);
    }
    parse_unbounded(text)
}

/// `parse_vocab` minus the byte limit, which the caller has applied.
fn parse_unbounded(text: &str) -> Result<(VocabBatch, VocabStats), VocabError> {
    let mut lines = text.split('\n').skip_while(|l| l.is_empty());
    if lines.next() != Some(HEADER) {
        return Err(VocabError::BadHeader);
    }
    // First-seen order, so the result does not depend on hash order.
    let mut words: HashMap<String, usize> = HashMap::new();
    let mut bigrams: HashMap<(String, String), usize> = HashMap::new();
    let mut batch = VocabBatch::default();
    let mut stats = VocabStats::default();
    let mut records = 0usize;
    for line in lines {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        records += 1;
        if records > MAX_RECORDS {
            return Err(VocabError::TooLarge);
        }
        // At most five pieces and no Vec: a line of a million TABs costs
        // nothing (the fifth piece is the unsplit rest, so a `Some` there
        // means too many fields for any known record).
        let mut it = line.splitn(5, '\t');
        let kind = it.next().unwrap_or_default();
        let fields = [it.next(), it.next(), it.next(), it.next()];
        let imported =
            match kind {
                "w" => match fields {
                    [Some(w), Some(c), None, None] => {
                        count(c).zip(word(w)).map(|(c, w)| match words.get(&w) {
                            Some(&i) => add(&mut batch.words[i].1, c),
                            None => {
                                words.insert(w.clone(), batch.words.len());
                                batch.words.push((w, c));
                            }
                        })
                    }
                    _ => None,
                },
                "b" => match fields {
                    [Some(w1), Some(w2), Some(c), None] => count(c)
                        .zip(word(w1).zip(word(w2)))
                        .map(|(c, k)| match bigrams.get(&k) {
                            Some(&i) => add(&mut batch.bigrams[i].2, c),
                            None => {
                                bigrams.insert(k.clone(), batch.bigrams.len());
                                batch.bigrams.push((k.0, k.1, c));
                            }
                        }),
                    _ => None,
                },
                // Forward compatibility: other record types are ignored.
                _ => Some(()),
            };
        if imported.is_none() {
            stats.skipped = stats.skipped.saturating_add(1);
        }
    }
    Ok((batch, stats))
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Raw file bytes to a batch plus the SHA-256 of exactly those bytes (the
/// duplicate-import key). Invalid UTF-8 becomes U+FFFD, which no learnable
/// word contains. The byte limit applies to the raw bytes (the U+FFFD
/// replacement can make the text longer).
pub fn read_vocab(bytes: &[u8]) -> Result<(VocabBatch, VocabStats, [u8; 32]), VocabError> {
    if bytes.len() > MAX_BYTES {
        return Err(VocabError::TooLarge);
    }
    let sha = sha256(bytes);
    let text = String::from_utf8_lossy(bytes);
    let (batch, stats) = parse_unbounded(&text)?;
    Ok((batch, stats, sha))
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "#correctiond-vocab v1\n";

    fn parse(body: &str) -> (VocabBatch, VocabStats) {
        parse_vocab(&format!("{H}{body}")).unwrap()
    }

    #[test]
    fn parse_minimal() {
        let (b, s) = parse_vocab("#correctiond-vocab v1\nw\tako\t12\nb\tako\tnaman\t3\n").unwrap();
        assert_eq!(b.words, vec![("ako".to_owned(), 12)]);
        assert_eq!(b.bigrams, vec![("ako".to_owned(), "naman".to_owned(), 3)]);
        assert_eq!(s.skipped, 0);
    }

    /// The brief's literal pair ("ako", "po") is dropped: "po" has two
    /// letters and fails `is_learnable`, and a typed "ako po" is not
    /// learned as a pair either (state.rs `learn`).
    #[test]
    fn pair_with_unlearnable_word_dropped() {
        let (b, s) = parse_vocab("#correctiond-vocab v1\nw\tako\t12\nb\tako\tpo\t3\n").unwrap();
        assert_eq!(b.words, vec![("ako".to_owned(), 12)]);
        assert!(b.bigrams.is_empty());
        assert_eq!(s.skipped, 1);
    }

    #[test]
    fn bad_header_rejected() {
        for text in [
            "",
            "w\tako\t12\n",
            "#correctiond-vocab v2\nw\tako\t12\n",
            "# correctiond-vocab v1\n",
            "#correctiond-vocab v1 \n",
            "#correctiond-vocab v1\r\nw\tako\t12\r\n",
            "\u{feff}#correctiond-vocab v1\n",
            "# a comment first\n#correctiond-vocab v1\n",
        ] {
            assert!(
                matches!(parse_vocab(text), Err(VocabError::BadHeader)),
                "{text:?}"
            );
        }
        // Leading empty lines are fine: the header is the first non-empty line.
        assert!(parse_vocab("\n\n#correctiond-vocab v1\n").is_ok());
    }

    #[test]
    fn comments_unknown_and_malformed() {
        let (b, s) = parse("# note\nx\tfoo\t1\nw\tako\nw\tsalamat\t2\n\n");
        assert_eq!(b.words, vec![("salamat".to_owned(), 2)]);
        assert!(b.bigrams.is_empty());
        assert_eq!(s.skipped, 1);
        // Every malformed shape is skipped and counted.
        let (b, s) = parse(
            "w\tako\t0\nw\tako\t-3\nw\tako\t1.5\nw\tako\t\nw\tako\t2\textra\n\
             b\tako\t3\nb\tako\tpo\t\nb\tako\tpo\tx\nw\tako\t+4\n",
        );
        assert!(b.words.is_empty() && b.bigrams.is_empty());
        assert_eq!(s.skipped, 9);
    }

    #[test]
    fn secrets_dropped_and_counts_clamped() {
        let (b, s) = parse(
            "w\thunter2\t5\nw\tKEY=x\t5\nw\tAko\t9999999\nb\tako\thunter2\t4\n\
             w\tpAssWord\t3\nw\tpo\t3\nw\tmabuti\t99999999999999999999999\n",
        );
        let mut words = b.words.clone();
        words.sort();
        assert_eq!(
            words,
            vec![
                ("ako".to_owned(), 1_000_000),
                ("mabuti".to_owned(), 1_000_000)
            ]
        );
        assert!(b.bigrams.is_empty());
        // hunter2, KEY=x, the pair, pAssWord and the too-short "po".
        assert_eq!(s.skipped, 5);
    }

    #[test]
    fn duplicates_summed() {
        let (b, _) = parse("w\tako\t2\nw\tako\t2\nb\tAko\tNaman\t1\nb\tako\tnaman\t1\n");
        assert_eq!(b.words, vec![("ako".to_owned(), 4)]);
        assert_eq!(b.bigrams, vec![("ako".to_owned(), "naman".to_owned(), 2)]);
        // The sum is clamped as well.
        let (b, _) = parse("w\tako\t900000\nw\tako\t900000\n");
        assert_eq!(b.words, vec![("ako".to_owned(), 1_000_000)]);
    }

    /// Counts the bytes allocated on the current thread, so a test can tell
    /// how much one call allocated.
    mod alloc_count {
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;

        thread_local! {
            static BYTES: Cell<usize> = const { Cell::new(0) };
        }

        pub struct Counting;

        fn add(n: usize) {
            let _ = BYTES.try_with(|b| b.set(b.get().saturating_add(n)));
        }

        // SAFETY: forwards to the system allocator unchanged.
        unsafe impl GlobalAlloc for Counting {
            unsafe fn alloc(&self, l: Layout) -> *mut u8 {
                add(l.size());
                unsafe { System.alloc(l) }
            }
            unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
                unsafe { System.dealloc(p, l) }
            }
            unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
                add(new);
                unsafe { System.realloc(p, l, new) }
            }
        }

        pub fn allocated() -> usize {
            BYTES.try_with(|b| b.get()).unwrap_or(0)
        }
    }

    #[global_allocator]
    static ALLOC: alloc_count::Counting = alloc_count::Counting;

    /// A line of a million TABs must not become a million field slices
    /// (16 MB here, ~268 MB for a 16 MiB line) in the engine process.
    #[test]
    fn tab_flood_line_parses_without_field_vec() {
        let mut text = String::from(H);
        text.push('w');
        text.push_str(&"\t".repeat(1_000_000));
        text.push('\n');
        let t0 = std::time::Instant::now();
        let before = alloc_count::allocated();
        let (b, s) = parse_vocab(&text).unwrap();
        let during = alloc_count::allocated() - before;
        assert!(b.words.is_empty() && b.bigrams.is_empty());
        assert_eq!(s.skipped, 1);
        assert!(during < 64 * 1024, "parse allocated {during} bytes");
        assert!(t0.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn too_large_rejected() {
        let mut text = String::from(H);
        for _ in 0..200_000 {
            text.push_str("w\tako\t1\n");
        }
        assert!(parse_vocab(&text).is_ok());
        text.push_str("z\tfuture\n");
        assert!(matches!(parse_vocab(&text), Err(VocabError::TooLarge)));

        let mut big = String::from(H);
        big.push_str("# ");
        big.push_str(&"a".repeat(MAX_BYTES));
        assert!(matches!(parse_vocab(&big), Err(VocabError::TooLarge)));
        assert!(matches!(
            read_vocab(big.as_bytes()),
            Err(VocabError::TooLarge)
        ));
    }

    #[test]
    fn read_vocab_hashes_raw_bytes() {
        let raw = b"#correctiond-vocab v1\nw\tako\t2\nw\tbad\xff\t2\n";
        let (b, s, sha) = read_vocab(raw).unwrap();
        assert_eq!(b.words, vec![("ako".to_owned(), 2)]);
        // Invalid UTF-8 never becomes a learnable word.
        assert_eq!(s.skipped, 1);
        assert_eq!(sha, sha256(raw));
        assert_ne!(sha, sha256(b"#correctiond-vocab v1\n"));
        // Exactly at the limit, mostly invalid UTF-8 (each byte becomes a
        // 3-byte U+FFFD): still accepted.
        let mut edge = b"#correctiond-vocab v1\n#".to_vec();
        edge.resize(MAX_BYTES, 0xff);
        assert!(read_vocab(&edge).is_ok());
        // SHA-256("abc"), FIPS 180-2 test vector.
        let abc = sha256(b"abc");
        assert_eq!(abc[..4], [0xba, 0x78, 0x16, 0xbf]);
        assert_eq!(abc[28..], [0xf2, 0x00, 0x15, 0xad]);
    }
}
