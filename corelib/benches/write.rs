use corelib::db::{attach, BigramRow, DirtyBatch, VocabRow};
use corelib::key::StoreKey;
use criterion::{criterion_group, criterion_main, Criterion};
use std::time::{Duration, Instant};

/// A 1,000-row absolute upsert batch (600 words, 400 bigrams).
fn batch(round: u64) -> DirtyBatch {
    DirtyBatch {
        vocab: (0..600)
            .map(|i| VocabRow {
                word: format!("word{i}"),
                freq: 2 + round,
                last_seen: 1_700_000_000 + round as i64,
            })
            .collect(),
        bigrams: (0..400)
            .map(|i| BigramRow {
                w1: format!("word{i}"),
                w2: format!("next{i}"),
                count: 2 + round,
            })
            .collect(),
        ..DirtyBatch::default()
    }
}

fn write_1000(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let (mut db, _, _) = attach(dir.path(), &StoreKey::generate()).unwrap();
    let mut round = 0u64;
    c.bench_function("db_write_1000_rows", |b| {
        b.iter(|| {
            round += 1;
            db.write(&batch(round)).unwrap()
        })
    });

    // Explicit median/worst over 200 timed writes (gate: worst < 50 ms).
    let mut times: Vec<Duration> = (0..200)
        .map(|_| {
            round += 1;
            let b = batch(round);
            let t = Instant::now();
            db.write(&b).unwrap();
            t.elapsed()
        })
        .collect();
    times.sort();
    println!(
        "write_1000 median={:?} worst={:?}",
        times[times.len() / 2],
        times[times.len() - 1]
    );
}

criterion_group!(benches, write_1000);
criterion_main!(benches);
