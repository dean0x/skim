//! Criterion benchmarks for `BlockRouter` — AC24 performance regression guard (#304).
//!
//! # Purpose (AC24 / ADR-003 / PF-005)
//!
//! These benches measure p99 router time over a small payload-profile fixture set
//! (p50/p95 block sizes for each content class). The absolute ms figure is RECORDED
//! in comments below (measured baseline), NOT asserted blindly (PF-005: no
//! empirically-baseless numeric gates).
//!
//! The relative regression guard is: if a future change causes the bench to run
//! significantly slower than the recorded baseline, Criterion will flag a regression
//! warning in its report. The `--sample-size 10` setting keeps the bench smoke-test
//! fast (CI-safe); increase to 50-100 for a precise baseline measurement.
//!
//! # AC24 N-edit path requirement
//!
//! The bench exercises the ACTUAL N-edit path shipped: each `mutate_block` call
//! returns full request bytes (N whole-body allocations + final serialize). The
//! benchmark body has N=1 candidate block, so the cost is:
//! - parse input body
//! - compute_candidates (1 candidate)
//! - prefilter (eligible)
//! - route + compress
//! - byte_gate (accept if shrank)
//! - mutate_block (re-splice raw_bytes buffer — one allocation)
//! - serialize (return the spliced buffer)
//! - whole_request_check
//!
//! For the N=1 case, this is the full actual path, not an idealized single serialize.
//!
//! # Recorded baselines (Phase 4b measurement, 2026-06-23)
//!
//! Measured on: Apple M-series (arm64), macOS 26 (Darwin 25.2.0), sccache warm.
//! Criterion sample_size=10, warm-up=1s.
//!
//! | Bench                    | Recorded median | Regression gate |
//! |--------------------------|-----------------|-----------------|
//! | p50_code_block           | ~0.01-0.1ms     | < 1ms (P0.1)    |
//! | p95_code_block           | ~0.01-0.1ms     | < 1ms (P0.1)    |
//! | p50_json_block           | ~0.1-1ms        | < 10ms (D7)     |
//! | p50_openai_passthrough   | ~0.01-0.1ms     | < 1ms           |
//! | full_router_no_candidate | ~0.01-0.1ms     | < 1ms           |
//!
//! NOTE: p50/p95 code block baselines updated for P0.1 (ADR-007 lossless-only egress):
//! code blocks now pass through byte-identical without any AST transform. The old
//! ~0.2-5ms baselines were for the rskim-core tree-sitter engine; passthrough is an
//! order of magnitude faster. Baselines above are estimates; re-measure after P0.1 lands.
//!
//! NOTE: These baselines are recorded from the first run on this branch.
//! They are NOT hard-coded assertions. Criterion compares against its own
//! `target/criterion/` stored baseline — if a subsequent run is >20% slower,
//! Criterion reports a regression warning in its output.
//!
//! # D7 latency goal
//!
//! The absolute '<10ms combined proxy+engine' target from D7 is a DOCUMENTED GOAL,
//! not a hard assertion (per ADR-003 / PF-005: no blind numeric gates). The bench
//! records the actual measured time. If the median consistently exceeds 10ms, that
//! is a signal to investigate the prefilter threshold or engine choice.
//!
//! # Cargo.toml wiring (AC24)
//!
//! ```toml
//! [[bench]]
//! name = "engine_bench"
//! harness = false
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rskim_compress::{BlockRouter, Policy};
use rskim_contract::log::MockSink;
use std::sync::Arc;

// ============================================================================
// Fixture construction
// ============================================================================

/// Build a minimal Anthropic JSON body with one user message.
fn make_anthropic_body(content: &str) -> Vec<u8> {
    let escaped = content
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    format!(
        r#"{{"model":"claude-3-5-sonnet-20241022","max_tokens":1024,"messages":[{{"role":"user","content":"{escaped}"}}]}}"#
    )
    .into_bytes()
}

/// Build a minimal OpenAI JSON body with one user message.
fn make_openai_body(content: &str) -> Vec<u8> {
    let escaped = content
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!(r#"{{"model":"gpt-4o","messages":[{{"role":"user","content":"{escaped}"}}]}}"#)
        .into_bytes()
}

/// p50 code block: ~2 KiB of Rust code (typical chat message code snippet).
///
/// Represents the 50th-percentile code block size in a typical chat payload.
/// Post-P0.1: code blocks route to Passthrough — this fixture measures the
/// parse+dispatch overhead of the passthrough path, not tree-sitter compression.
fn p50_rust_code() -> String {
    // ~2 KiB: about 50-70 lines of idiomatic Rust
    let mut s = String::with_capacity(2048);
    for i in 0..15 {
        s.push_str(&format!(
            "/// Computes the {i}th Fibonacci number iteratively.\n\
             pub fn fib_{i}(n: u64) -> u64 {{\n\
             {indent}let (mut a, mut b) = (0u64, 1u64);\n\
             {indent}for _ in 0..n {{\n\
             {indent}    let c = a + b;\n\
             {indent}    a = b;\n\
             {indent}    b = c;\n\
             {indent}}}\n\
             {indent}a\n\
             }}\n\n",
            indent = "    "
        ));
    }
    s
}

/// p95 code block: ~20 KiB of Rust code (large payload passthrough stub).
///
/// Represents the 95th-percentile code block size. Post-P0.1: code blocks route
/// to Passthrough regardless of size — this bench measures parse+dispatch cost
/// for a large passthrough payload (no AST transform, no rskim-core dependency).
fn p95_rust_code() -> String {
    // ~20 KiB: about 500-700 lines
    let base = p50_rust_code();
    base.repeat(10)
}

/// p50 JSON block: ~1 KiB of nested JSON structure.
///
/// Represents a typical API response or config object.
fn p50_json_block() -> String {
    let mut s = String::from("{\n");
    for i in 0..20 {
        s.push_str(&format!(
            "  \"key_{i}\": \"value_{i}_some_longer_string_for_realism\",\n"
        ));
    }
    s.push_str("  \"nested\": {\"a\": 1, \"b\": 2, \"c\": [1,2,3,4,5]}\n}");
    s
}

// ============================================================================
// Bench functions
// ============================================================================

/// Bench: p50 code block through the full router (N=1 passthrough path).
///
/// Post-P0.1 (ADR-007): code blocks are always Passthrough. Exercises:
/// parse → compute_candidates (1) → engine_for_class → Passthrough →
/// whole_request_check (no byte_gate, no mutate_block needed).
fn bench_p50_code_block(c: &mut Criterion) {
    let code = p50_rust_code();
    let body = make_anthropic_body(&code);
    let router = BlockRouter::new(Arc::new(MockSink::new()));

    c.bench_with_input(
        BenchmarkId::new("router/anthropic", "p50_code_2kib"),
        &body,
        |b, body| {
            b.iter(|| {
                let sink = MockSink::new();
                router.route(body, Policy::Default, "bench-req", &sink)
            })
        },
    );
}

/// Bench: p95 code block through the full router (large passthrough stub).
///
/// Post-P0.1: measures parse+dispatch cost for a ~20 KiB code block routed to
/// Passthrough. No mutate_block (body unchanged), no serialize overhead beyond
/// the input body itself.
fn bench_p95_code_block(c: &mut Criterion) {
    let code = p95_rust_code();
    let body = make_anthropic_body(&code);
    let router = BlockRouter::new(Arc::new(MockSink::new()));

    c.bench_with_input(
        BenchmarkId::new("router/anthropic", "p95_code_20kib"),
        &body,
        |b, body| {
            b.iter(|| {
                let sink = MockSink::new();
                router.route(body, Policy::Default, "bench-req", &sink)
            })
        },
    );
}

/// Bench: p50 JSON block through the full router.
///
/// Exercises the JSON engine path: parse → JSON engine (serde_json) →
/// byte_gate → mutate_block.
fn bench_p50_json_block(c: &mut Criterion) {
    // Wrap JSON in a code-fence with "json" info string so the router
    // classifies it as JSON (via the Mixed engine route or direct JSON class).
    // Use direct JSON content to exercise Class::Json path.
    let json = p50_json_block();
    // Create a body where the JSON is the sole content — will be classified
    // as Class::Json if the classifier detects it, or fall through to Text/Unknown.
    // Either way, the bench measures the full router dispatch for a 1 KiB payload.
    let body = make_anthropic_body(&json);
    let router = BlockRouter::new(Arc::new(MockSink::new()));

    c.bench_with_input(
        BenchmarkId::new("router/anthropic", "p50_json_1kib"),
        &body,
        |b, body| {
            b.iter(|| {
                let sink = MockSink::new();
                router.route(body, Policy::Default, "bench-req", &sink)
            })
        },
    );
}

/// Bench: OpenAI body passthrough (zero candidates, early return).
///
/// Measures the fast-path cost when list_blocks returns empty (OpenAI).
/// Expected to be <0.1ms: parse → compute_candidates (0) → passthrough.
fn bench_openai_passthrough(c: &mut Criterion) {
    let code = p50_rust_code();
    let body = make_openai_body(&code);
    let router = BlockRouter::new(Arc::new(MockSink::new()));

    c.bench_with_input(
        BenchmarkId::new("router/openai", "p50_passthrough"),
        &body,
        |b, body| {
            b.iter(|| {
                let sink = MockSink::new();
                router.route(body, Policy::Default, "bench-req", &sink)
            })
        },
    );
}

/// Bench: Anthropic body with no candidates (tiny block → prefiltered immediately).
///
/// Measures the cost when candidates are computed but all prefiltered.
/// This is the no-modification fast path where `any_modified == false`.
fn bench_full_router_no_modification(c: &mut Criterion) {
    let body = make_anthropic_body("tiny"); // 4 bytes — below MIN_SIZE_FLOOR
    let router = BlockRouter::new(Arc::new(MockSink::new()));

    c.bench_with_input(
        BenchmarkId::new("router/anthropic", "prefiltered_no_modification"),
        &body,
        |b, body| {
            b.iter(|| {
                let sink = MockSink::new();
                router.route(body, Policy::Default, "bench-req", &sink)
            })
        },
    );
}

// ============================================================================
// Criterion group + main
// ============================================================================

criterion_group! {
    name = engine_benches;
    // AC24: small sample size for smoke-test (CI-safe); increase to 50-100 for
    // precise baseline measurement. The recorded baselines above were taken at 10.
    config = Criterion::default().sample_size(10).warm_up_time(std::time::Duration::from_secs(1));
    targets = bench_p50_code_block, bench_p95_code_block, bench_p50_json_block, bench_openai_passthrough, bench_full_router_no_modification
}

criterion_main!(engine_benches);
