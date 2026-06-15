//! Criterion benchmark for rskim-llm parse+classify+serialize pipeline.
//!
//! # Relative linearity baseline (ADR-003/AC14)
//!
//! This benchmark RECORDS the absolute parse+classify+serialize times across
//! 100KB / 1MB / 10MB tool-result-heavy bodies. Per ADR-003, the absolute
//! <=1ms/100KB figure is a baseline, NOT a pass/fail gate (1ms is within
//! CI-runner noise).
//!
//! The relative-linearity gate (time(1MB) <= 15x time(100KB) and
//! time(10MB) <= 15x time(1MB)) described in AC14 is enforced as an in-run
//! assertion in `tests/linearity.rs` (`ac14_relative_linearity_gate`). This
//! benchmark provides the measured absolute baseline that gate is grounded
//! against.
//!
//! The counting-allocator memory k-bound (peak allocation <= k × body_size) is
//! NOT yet wired up as an enforced assertion — there is no isolated
//! counting-allocator test binary in this crate today (a global allocator must
//! not be shared with parallel tests). The k ≈ 3.5 bound is documented
//! analytically in `lib.rs`; wiring it as a regression gate is a follow-up
//! (see #309 / Wave-1 perf-gate follow-up).

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use rskim_llm::{classify_body, parse, serialize};

/// Build a tool-result-heavy Anthropic body of approximately `target_bytes` bytes.
///
/// Each tool result uses a ~2KB text payload to simulate real-world usage where
/// tool results dominate the body size.
fn build_body(target_bytes: usize) -> Vec<u8> {
    // Each block is roughly 2KB of text payload
    let block_payload = "X".repeat(2000);
    let mut messages = Vec::new();

    // Estimate blocks needed
    let block_json_overhead = 150; // per tool_result block
    let blocks_needed = (target_bytes / (2000 + block_json_overhead)).max(1);

    // Build tool use + tool result pairs
    for i in 0..blocks_needed {
        // User message with tool result
        let tool_result = format!(
            r#"{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"call_{i}","content":"{payload}"}},{{"type":"tool_use","id":"call_{i}","name":"tool_{i}","input":{{"query":"test"}}}}]}}"#,
            payload = block_payload,
        );
        messages.push(tool_result);
    }

    let body = format!(
        r#"{{"model":"claude-3-5-sonnet-20241022","messages":[{}],"max_tokens":4096}}"#,
        messages.join(",")
    );
    body.into_bytes()
}

fn bench_parse_classify_serialize(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_classify_serialize");

    for &size_label in &[
        ("100KB", 100 * 1024),
        ("1MB", 1024 * 1024),
        ("10MB", 10 * 1024 * 1024),
    ] {
        let (label, size) = size_label;
        let input = build_body(size);

        group.bench_with_input(BenchmarkId::new("anthropic", label), &input, |b, input| {
            b.iter(|| {
                // Unwrap is acceptable in benchmarks — a parse failure is a bug,
                // not a graceful error path.  black_box prevents LTO/codegen-units=1
                // from dead-code-eliminating the measured pipeline (matches the
                // sibling crates rskim-tokens and rskim-core bench pattern).
                #[allow(clippy::unwrap_used)]
                let body = parse(black_box(input)).unwrap();
                black_box(classify_body(&body));
                #[allow(clippy::unwrap_used)]
                black_box(serialize(&body).unwrap());
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_parse_classify_serialize);
criterion_main!(benches);
