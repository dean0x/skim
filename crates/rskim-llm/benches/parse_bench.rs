//! Criterion benchmark for rskim-llm parse+classify+serialize pipeline.
//!
//! # Relative linearity gate (ADR-003/AC14)
//!
//! The absolute <=1ms/100KB figure is RECORDED as a baseline, NOT enforced as a gate
//! (1ms is within CI-runner noise per ADR-003). The enforced gates are:
//!
//! - time(1MB) <= 15x time(100KB)
//! - time(10MB) <= 15x time(1MB)
//!
//! These are asserted in the integration test suite (not here) using stored Criterion
//! estimates from the benches output directory. This file records the absolute times.
//!
//! # Memory constant k
//!
//! See `tests/memory_alloc.rs` for the isolated counting-allocator test that verifies
//! peak allocation <= k × body_size.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
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
                // not a graceful error path.
                #[allow(clippy::unwrap_used)]
                let body = parse(input).unwrap();
                let _classifications = classify_body(&body);
                #[allow(clippy::unwrap_used)]
                let _serialized = serialize(&body).unwrap();
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_parse_classify_serialize);
criterion_main!(benches);
