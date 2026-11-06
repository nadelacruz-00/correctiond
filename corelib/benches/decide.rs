use corelib::dictionary::{parse_lexicon, Dictionary, LangId, LangSet};
use corelib::layout::LayoutId;
use corelib::scoring::{decide, LangCtx, PersonalLookup};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::time::Instant;

struct NoPersonal;
impl PersonalLookup for NoPersonal {
    fn personal_freq(&self, _: &str) -> u64 {
        0
    }
    fn is_blacklisted(&self, _: &str, _: &str) -> bool {
        false
    }
    fn bigram_count(&self, _: &str, _: &str) -> u64 {
        0
    }
    fn slip_count(&self, _: LayoutId, _: char, _: char) -> u64 {
        0
    }
}

const FIXTURES: [&str; 5] = ["bulid", "recieve", "teh", "wierd", "definately"];

fn bench(c: &mut Criterion) {
    let text =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../data/en_words.txt"))
            .expect("data/en_words.txt");
    let dict = Dictionary::from_lexicons(vec![(LangId(0), parse_lexicon(&text))]);
    let ctx = LangCtx {
        allowed: LangSet::all(1),
        margin: 0.3,
    };
    let p = NoPersonal;

    // Worst observed single call over many repetitions.
    let mut worst = std::time::Duration::ZERO;
    for _ in 0..2000 {
        for w in FIXTURES {
            let t = Instant::now();
            black_box(decide(
                black_box(w),
                None,
                LayoutId::Qwerty,
                &ctx,
                &dict,
                &p,
            ));
            worst = worst.max(t.elapsed());
        }
    }
    println!("decide worst observed: {worst:?}");

    c.bench_function("decide_5_typos", |b| {
        b.iter(|| {
            for w in FIXTURES {
                black_box(decide(
                    black_box(w),
                    None,
                    LayoutId::Qwerty,
                    &ctx,
                    &dict,
                    &p,
                ));
            }
        })
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
