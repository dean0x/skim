#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Criterion benchmarks for token counting performance.
//!
//! Measures counting a ~100 KB input after counter initialisation.
//! Tracked baseline: relative regression guard (ADR-003 — no empirically-
//! baseless absolute cap; the 25 ms ceiling in AC12 is a coarse sentinel).

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use rskim_tokens::{Counter, Encoding};

/// Build a deterministic ~100 KB ASCII input for benchmarking.
fn make_100kb_input() -> String {
    // Repeat a realistic code snippet to reach ~100 KB.
    let snippet = "fn process(items: &[Item]) -> Result<Vec<Output>, Error> {\n\
                   items.iter().map(|item| transform(item)).collect()\n\
                   }\n\n";
    let repeat = (100 * 1024) / snippet.len() + 1;
    snippet.repeat(repeat)
}

fn bench_cl100k(c: &mut Criterion) {
    let input = make_100kb_input();
    let counter = Counter::new(Encoding::Cl100k).expect("cl100k init");

    // Warm up by counting once before benchmarking.
    let _ = counter.count(&input);

    c.bench_function("cl100k_100kb", |b| {
        b.iter(|| counter.count(black_box(&input)));
    });
}

fn bench_o200k(c: &mut Criterion) {
    let input = make_100kb_input();
    let counter = Counter::new(Encoding::O200k).expect("o200k init");

    let _ = counter.count(&input);

    c.bench_function("o200k_100kb", |b| {
        b.iter(|| counter.count(black_box(&input)));
    });
}

fn bench_anthropic_offline(c: &mut Criterion) {
    let input = make_100kb_input();
    let counter = Counter::new(Encoding::AnthropicOffline).expect("anthropic_offline init");

    let _ = counter.count(&input);

    c.bench_function("anthropic_offline_100kb", |b| {
        b.iter(|| counter.count(black_box(&input)));
    });
}

fn bench_heuristic(c: &mut Criterion) {
    let input = make_100kb_input();
    let counter = Counter::new(Encoding::Heuristic).expect("heuristic init");

    c.bench_function("heuristic_100kb", |b| {
        b.iter(|| counter.count(black_box(&input)));
    });
}

criterion_group!(
    benches,
    bench_cl100k,
    bench_o200k,
    bench_anthropic_offline,
    bench_heuristic,
);
criterion_main!(benches);
