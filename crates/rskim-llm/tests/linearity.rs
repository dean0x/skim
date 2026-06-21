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
//! # Flakiness mitigation
//!
//! Each body size is measured `SAMPLES` independent times (each sample averaging
//! `REPS` iterations to amortise micro-jitter).  The MINIMUM sample is used to
//! compute the ratio, which eliminates the effect of a single scheduler or GC hitch
//! inflating one measurement past the gate threshold.  The minimum-of-N strategy
//! is a standard approach to timing microbenchmarks (Kalibera & Jones 2013).
//!
//! # Memory constant k
//!
//! Peak allocation during parse is bounded analytically by k ≈ 2 × body_size
//! (input buffer 1×, typed model ≤1×). The previous k ≈ 3.5× that included a
//! throwaway serde_json::Value intermediate was eliminated in the single-parse
//! refactor (provider detection now uses a shallow IgnoredAny scan). A
//! counting-allocator regression gate would require a custom global allocator in an
//! isolated binary; that infrastructure is a tracked follow-up (#309 Wave-1 perf gate).
//! The analytical k ≈ 2× bound is documented in lib.rs as the design intent.
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

/// Number of iterations per sample to amortise intra-measurement jitter.
const REPS: u32 = 5;

/// Number of independent samples taken per body size.
/// The MINIMUM sample is used for the ratio to eliminate single-hitch outliers.
/// 7 samples is a standard count for microbenchmarks (Kalibera & Jones 2013):
/// enough to discard both cold and transient-hot outliers.
const SAMPLES: u32 = 7;

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
///
/// Returns a single aggregate measurement (sum of REPS iterations). Caller takes
/// the minimum across SAMPLES independent calls to eliminate outliers.
// AC14 perf test legitimately reads wall-clock time. The crate clippy.toml bans
// `Instant::now` to keep ambient state out of production `src/` (AC10/AC12 determinism
// gate); the gate's own note explicitly sanctions timing in tests. clippy.toml cannot
// path-scope to `src/`, so a targeted allow keeps the gate strict everywhere except
// this measurement helper.
#[allow(clippy::disallowed_methods)]
fn time_cycle(input: &[u8]) -> u128 {
    let start = Instant::now();
    for _ in 0..REPS {
        let body = parse(input).expect("parse failed");
        let _cls = classify_body(&body);
        let _out = serialize(&body).expect("serialize failed");
    }
    start.elapsed().as_nanos()
}

/// Take `SAMPLES` independent measurements and return the minimum.
///
/// The minimum-of-N strategy eliminates single-hitch outliers (scheduler preemption,
/// GC, page faults) that inflate a single sample past the ratio gate.
/// Used instead of the mean because the minimum is the most stable estimate of the
/// "true" cost on an uncontended system (Agesen 1995; Kalibera & Jones 2013).
fn min_time_cycle(input: &[u8]) -> u128 {
    (0..SAMPLES)
        .map(|_| time_cycle(input))
        .min()
        .expect("SAMPLES > 0")
}

/// Run condition: this test is `#[ignore]`d by default because a ratio-based timing
/// gate in the standard `cargo test` path can flake on loaded/shared CI runners
/// (if the 100KB body is cache-resident while the 10MB body hits memory bandwidth,
/// the ratio can transiently exceed 15x even with correct linear code).
///
/// To run: `cargo test -p rskim-llm ac14_relative_linearity_gate -- --ignored`
///
/// In CI this runs as a dedicated perf job (see #309 / Wave-1 perf-gate follow-up)
/// rather than in the default `cargo test` matrix.
///
/// # Memory-k bound follow-up
///
/// Peak allocation during parse is analytically bounded at k ≈ 2 × body_size
/// (input buffer 1×, typed model ≤1×) after the single-parse refactor that
/// eliminated the throwaway serde_json::Value intermediate.
/// Wiring this as an enforced counting-allocator test (AC14) is tracked in a
/// dedicated follow-up (tracked under #309 per ADR-004).  The
/// isolated counting-allocator binary must not share the global allocator with
/// parallel test threads.
#[test]
#[ignore = "timing ratio gate — run with --ignored in a dedicated perf job to avoid CI flakiness; \
            see #309 for the tracking issue"]
fn ac14_relative_linearity_gate() {
    // Build bodies at three scales
    let body_100kb = build_body(100 * 1024);
    let body_1mb = build_body(1024 * 1024);
    let body_10mb = build_body(10 * 1024 * 1024);

    // Warm up the allocator and instruction cache with the smallest body so
    // the first measurement isn't dominated by cold-start effects.
    let _ = min_time_cycle(&body_100kb);

    // Take the minimum of SAMPLES independent measurements per body size.
    // This eliminates single-hitch outliers from the ratio calculation.
    let min_100kb = min_time_cycle(&body_100kb);
    let min_1mb = min_time_cycle(&body_1mb);
    let min_10mb = min_time_cycle(&body_10mb);

    // Compute per-rep average nanos to normalise against REPS.
    let avg_100kb = min_100kb as f64 / REPS as f64;
    let avg_1mb = min_1mb as f64 / REPS as f64;
    let avg_10mb = min_10mb as f64 / REPS as f64;

    let ratio_1mb_vs_100kb = avg_1mb / avg_100kb;
    let ratio_10mb_vs_1mb = avg_10mb / avg_1mb;

    // Log the ratios so the CI log shows the measured values (ADR-003: record
    // the ratios alongside the gate constants).
    eprintln!(
        "[AC14] parse+classify+serialize linearity ratios (min of {SAMPLES} samples):\
         \n  100KB min-avg: {avg_100kb:.0} ns\
         \n  1MB   min-avg: {avg_1mb:.0} ns  (ratio vs 100KB: {ratio_1mb_vs_100kb:.1}x)\
         \n  10MB  min-avg: {avg_10mb:.0} ns (ratio vs 1MB:   {ratio_10mb_vs_1mb:.1}x)\
         \n  Gate: both ratios must be <= 15x (1.5x margin over linear 10x)"
    );

    const MAX_RATIO: f64 = 15.0;
    assert!(
        ratio_1mb_vs_100kb <= MAX_RATIO,
        "time(1MB) / time(100KB) = {ratio_1mb_vs_100kb:.1}x exceeds gate of {MAX_RATIO}x — \
         scaling is super-linear. Check for O(n²) loops or per-block allocations. \
         (min-avg 100KB={avg_100kb:.0}ns, min-avg 1MB={avg_1mb:.0}ns)"
    );
    assert!(
        ratio_10mb_vs_1mb <= MAX_RATIO,
        "time(10MB) / time(1MB) = {ratio_10mb_vs_1mb:.1}x exceeds gate of {MAX_RATIO}x — \
         scaling is super-linear. (min-avg 1MB={avg_1mb:.0}ns, min-avg 10MB={avg_10mb:.0}ns)"
    );
}
