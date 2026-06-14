//! Performance linearity gate (AC14).
//!
//! Asserts that parse+classify+serialize time scales linearly with body size —
//! specifically: time(1MB) <= 15x time(100KB) AND time(10MB) <= 15x time(1MB).
//!
//! Per ADR-003, absolute timing gates (e.g. "must complete in <1ms") are forbidden
//! because they are CI-runner-noise-dominated. This gate uses RELATIVE ratios measured
//! in a single run on the same machine under the same load, which are stable across
//! hardware. The 15x bound gives a 1.5x noise margin over the theoretical 10x for
//! perfectly linear scaling.
//!
//! # Memory constant k
//!
//! Peak allocation during parse is bounded analytically by k ≈ 3.5 × body_size
//! (input buffer 1×, serde_json::Value intermediate ≤1.5×, typed model ≤1×). A
//! counting-allocator regression gate would require a custom global allocator in an
//! isolated binary; that infrastructure is a tracked follow-up (#309 Wave-1 perf gate).
//! The analytical k = 3.5 bound is documented in lib.rs as the design intent.
//!
//! # Test isolation
//!
//! These tests are deliberately single-threaded (no parallel subtests) and use
//! `Instant::now()` around a HOT loop of `REPS` iterations to smooth out OS
//! scheduling jitter. The ratio gate is only meaningful when body-size effects dominate
//! over startup/GC noise.

// Test code legitimately uses expect/unwrap for failure reporting.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::panic
)]

use rskim_llm::{classify_body, parse, serialize};
use std::time::Instant;

// Number of repeated iterations to amortise scheduling jitter.
// 5 reps × 3 sizes is fast enough for a normal `cargo test` run (100-200ms total
// in debug mode) while providing enough samples to smooth single-run noise.
const REPS: u32 = 5;

/// Build a tool-result-heavy Anthropic body of approximately `target_bytes` bytes.
/// Matches the build_body() function in benches/parse_bench.rs so the linearity
/// test exercises the same workload as the Criterion benchmark.
fn build_body(target_bytes: usize) -> Vec<u8> {
    let block_payload = "X".repeat(2000);
    let block_json_overhead = 150;
    let blocks_needed = (target_bytes / (2000 + block_json_overhead)).max(1);

    let mut messages = Vec::new();
    for i in 0..blocks_needed {
        let msg = format!(
            r#"{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"call_{i}","content":"{payload}"}},{{"type":"tool_use","id":"call_{i}","name":"tool_{i}","input":{{"query":"test"}}}}]}}"#,
            payload = block_payload,
        );
        messages.push(msg);
    }

    format!(
        r#"{{"model":"claude-3-5-sonnet-20241022","messages":[{}],"max_tokens":4096}}"#,
        messages.join(",")
    )
    .into_bytes()
}

/// Time `REPS` parse+classify+serialize cycles on `input`, returning total nanos.
fn time_cycle(input: &[u8]) -> u128 {
    let start = Instant::now();
    for _ in 0..REPS {
        let body = parse(input).expect("parse failed");
        let _cls = classify_body(&body);
        let _out = serialize(&body).expect("serialize failed");
    }
    start.elapsed().as_nanos()
}

#[test]
fn ac14_relative_linearity_gate() {
    // Build bodies at three scales
    let body_100kb = build_body(100 * 1024);
    let body_1mb = build_body(1024 * 1024);
    let body_10mb = build_body(10 * 1024 * 1024);

    // Warm up the allocator and instruction cache with the smallest body so
    // the first measurement isn't dominated by cold-start effects.
    let _ = time_cycle(&body_100kb);

    let nanos_100kb = time_cycle(&body_100kb);
    let nanos_1mb = time_cycle(&body_1mb);
    let nanos_10mb = time_cycle(&body_10mb);

    // Compute per-rep average nanos to normalise against REPS.
    let avg_100kb = nanos_100kb as f64 / REPS as f64;
    let avg_1mb = nanos_1mb as f64 / REPS as f64;
    let avg_10mb = nanos_10mb as f64 / REPS as f64;

    let ratio_1mb_vs_100kb = avg_1mb / avg_100kb;
    let ratio_10mb_vs_1mb = avg_10mb / avg_1mb;

    // Log the ratios so the CI log shows the measured values (ADR-003: record
    // the ratios alongside the gate constants).
    eprintln!(
        "[AC14] parse+classify+serialize linearity ratios:\
         \n  100KB avg: {avg_100kb:.0} ns\
         \n  1MB   avg: {avg_1mb:.0} ns  (ratio vs 100KB: {ratio_1mb_vs_100kb:.1}x)\
         \n  10MB  avg: {avg_10mb:.0} ns (ratio vs 1MB:   {ratio_10mb_vs_1mb:.1}x)\
         \n  Gate: both ratios must be <= 15x (1.5x margin over linear 10x)"
    );

    const MAX_RATIO: f64 = 15.0;
    assert!(
        ratio_1mb_vs_100kb <= MAX_RATIO,
        "time(1MB) / time(100KB) = {ratio_1mb_vs_100kb:.1}x exceeds gate of {MAX_RATIO}x — \
         scaling is super-linear. Check for O(n²) loops or per-block allocations. \
         (avg 100KB={avg_100kb:.0}ns, avg 1MB={avg_1mb:.0}ns)"
    );
    assert!(
        ratio_10mb_vs_1mb <= MAX_RATIO,
        "time(10MB) / time(1MB) = {ratio_10mb_vs_1mb:.1}x exceeds gate of {MAX_RATIO}x — \
         scaling is super-linear. (avg 1MB={avg_1mb:.0}ns, avg 10MB={avg_10mb:.0}ns)"
    );
}
