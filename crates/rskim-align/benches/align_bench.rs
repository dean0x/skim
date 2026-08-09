//! Benchmark stubs for rskim-align.
//!
//! Full benchmarks are added in a later phase when the complete pipeline
//! (canonical ordering + cache_control marker injection) is implemented.
//! These stubs ensure the bench target compiles cleanly.

use criterion::{criterion_group, criterion_main, Criterion};
use rskim_align::align;
use rskim_llm::Provider;

static ANTHROPIC_BODY: &[u8] = br#"{
    "model": "claude-3-haiku-20240307",
    "max_tokens": 1024,
    "messages": [
        {"role": "user", "content": "Hello, world!"}
    ],
    "tools": [
        {"name": "get_weather", "description": "Get the weather", "input_schema": {"type": "object", "properties": {"location": {"type": "string"}}}},
        {"name": "calculate", "description": "Do math", "input_schema": {"type": "object", "properties": {"expression": {"type": "string"}}}}
    ]
}"#;

fn bench_align_anthropic(c: &mut Criterion) {
    c.bench_function("align_anthropic_canonical", |b| {
        b.iter(|| {
            let _ = align(
                criterion::black_box(ANTHROPIC_BODY),
                Provider::Anthropic,
                "bench-req-001",
            );
        });
    });
}

criterion_group!(benches, bench_align_anthropic);
criterion_main!(benches);
