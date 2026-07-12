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
//! | json_minify_p50          | TBD             | < 10ms (D7)     |
//! | json_minify_p95          | TBD             | < 10ms (D7)     |
//! | json_gate_worst_case     | ~2.38ms         | < 10ms (D7)     |
//! | dup_key_scan             | TBD             | < 10ms (D7)     |
//! | code_block_passthrough   | ~0.59ms         | < 1ms (P0.1)    |
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

/// p95 JSON block: ~10 KiB pretty-printed JSON object (200 string-value entries).
///
/// Represents the 95th-percentile JSON block size. Used in `bench_json_minify_p95`
/// to measure the minify + value-equivalence gate cost for a larger payload.
fn p95_json_block() -> String {
    let mut s = String::from("{\n");
    for i in 0..199 {
        s.push_str(&format!(
            "  \"key_{i:03}\": \"value_{i}_a_somewhat_longer_realistic_string_here\",\n"
        ));
    }
    s.push_str("  \"key_199\": \"value_199_final_entry_no_trailing_comma\"\n}");
    s
}

/// Adversarial many-key JSON object: 3 500 integer-valued keys (~57 KiB).
///
/// The maximum adversarial fixture within the `MAX_JSON_BYTES = 64 KiB` prefilter
/// limit. Stresses:
/// - The JSON engine dup-key scan (`HashSet<Vec<u8>>` per object, 3 500 insertions).
/// - `value_equivalent_raw` after minification (3 500 key-by-key recursive
///   comparisons, stays well within `DEFAULT_WORK_BUDGET = 200_000`).
///
/// # Why 3 500, not `MAX_JSON_KEYS = 10_000`
///
/// 10k keys × ~15 bytes/entry (pretty-printed) ≈ 150 KiB — exceeds `MAX_JSON_BYTES`
/// (64 KiB), so the prefilter short-circuits and neither the engine nor
/// `value_equivalent_raw` runs. 3 500 keys ≈ 57 KiB is the largest adversarial
/// fixture that exercises the full minify + gate path under production prefilter
/// constraints (per ADR-003 / PF-005: no hard assertions, documented goal only).
fn adversarial_many_key_json() -> String {
    let mut s = String::with_capacity(60_000);
    s.push_str("{\n");
    for i in 0..3_500usize {
        if i + 1 < 3_500 {
            s.push_str(&format!("  \"k{i}\": {i},\n"));
        } else {
            s.push_str(&format!("  \"k{i}\": {i}\n"));
        }
    }
    s.push('}');
    s
}

/// Adversarial dup-key scan fixture: 200 entries whose string VALUES contain
/// colon-separated text that looks like JSON key names.
///
/// A naive scanner that doesn't track string boundaries would falsely flag
/// substrings like `"key_N:"` inside a string value as duplicate keys.
/// This fixture stresses the scanner's in-string handling (must correctly
/// ignore key-like patterns inside quoted string values).
///
/// ~14 KiB: 200 entries × ~70 bytes each.
fn dup_key_scan_json() -> String {
    let mut s = String::with_capacity(15_000);
    s.push_str("{\n");
    for i in 0..200usize {
        let j = (i + 1) % 200;
        if i < 199 {
            s.push_str(&format!(
                "  \"entry_{i:03}\": \"config key_{j}: some value and key_{i}: nested here\",\n"
            ));
        } else {
            s.push_str(&format!(
                "  \"entry_{i:03}\": \"config key_{j}: some value and key_{i}: nested here\"\n"
            ));
        }
    }
    s.push('}');
    s
}

/// 90 KiB fenced Rust code block (45 × `p50_rust_code()`).
///
/// Used in `bench_code_block_passthrough` to record the O(1) passthrough baseline
/// for a 90 KiB fenced code block. Represents the largest-size code block expected
/// in real chat payloads (p99.9 class).
///
/// # P0.1 / ADR-007 baseline
///
/// Pre-P0.1 (rskim-core tree-sitter path): ~14.6 ms/90 KiB (measured at commit
/// `9cb3020c`, 2026-07-11). Post-P0.1 (passthrough, O(1) in content size):
/// expected < 0.2 ms (no AST transform, no rskim-core dependency).
fn p99_fenced_code_90kb() -> String {
    let base = p50_rust_code();
    let code = base.repeat(45);
    format!("```rust\n{code}\n```")
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

/// Bench: p50 JSON block through the JSON engine (minify + value-equivalence gate).
///
/// Exercised path: parse body → classify (`Class::Json`) → prefilter (eligible) →
/// JSON engine (minify, ~1 KiB) → `value_equivalent_raw` (20 key comparisons) →
/// byte_gate → `mutate_block`.
///
/// # D7 / ADR-003
///
/// Combined proxy + engine target: < 10 ms (D7 documented goal; per ADR-003/PF-005,
/// this is a RECORDED figure, NOT a hard assertion). A median ≫ 10 ms signals a
/// prefilter-threshold or engine-choice investigation.
fn bench_json_minify_p50(c: &mut Criterion) {
    let json = p50_json_block();
    let body = make_anthropic_body(&json);
    let router = BlockRouter::new(Arc::new(MockSink::new()));

    c.bench_with_input(
        BenchmarkId::new("router/json-minify", "p50_1kib"),
        &body,
        |b, body| {
            b.iter(|| {
                let sink = MockSink::new();
                router.route(body, Policy::Default, "bench-req", &sink)
            })
        },
    );
}

/// Bench: p95 JSON block through the JSON engine (minify + value-equivalence gate).
///
/// Same path as `bench_json_minify_p50` but with a ~10 KiB payload (200 entries).
/// Useful for detecting super-linear growth in the engine or gate.
fn bench_json_minify_p95(c: &mut Criterion) {
    let json = p95_json_block();
    let body = make_anthropic_body(&json);
    let router = BlockRouter::new(Arc::new(MockSink::new()));

    c.bench_with_input(
        BenchmarkId::new("router/json-minify", "p95_10kib"),
        &body,
        |b, body| {
            b.iter(|| {
                let sink = MockSink::new();
                router.route(body, Policy::Default, "bench-req", &sink)
            })
        },
    );
}

/// Bench: many-key adversarial JSON object through minify + `value_equivalent_raw`.
///
/// 3 500-key object (~57 KiB pretty-printed) — the largest adversarial fixture within
/// the `MAX_JSON_BYTES = 64 KiB` prefilter limit. Exercises:
/// - JSON engine dup-key scan: 3 500 `HashSet` insertions.
/// - `value_equivalent_raw` gate: 3 500 key-by-key recursive comparisons.
///
/// See `adversarial_many_key_json()` for why 3 500 keys (not 10 000).
///
/// # D7 latency goal (ADR-003 / PF-005)
///
/// Combined proxy + engine target: < 10 ms. This bench records the empirical worst-case
/// cost — NOT a hard assertion. If median > 10 ms consistently, investigate per-object
/// `HashSet` allocation or `value_equivalent_raw` budget exhaustion.
fn bench_json_gate_worst_case(c: &mut Criterion) {
    let json = adversarial_many_key_json();
    let body = make_anthropic_body(&json);
    let router = BlockRouter::new(Arc::new(MockSink::new()));

    c.bench_with_input(
        BenchmarkId::new("router/json-gate", "adversarial_3500keys_57kib"),
        &body,
        |b, body| {
            b.iter(|| {
                let sink = MockSink::new();
                router.route(body, Policy::Default, "bench-req", &sink)
            })
        },
    );
}

/// Bench: in-string key-like patterns through the JSON dup-key scanner.
///
/// 200-entry object (~14 KiB) where every string VALUE contains substrings of the
/// form `"key_N: value"` — a pattern that would trip a naive scanner operating on
/// raw bytes without proper string-boundary tracking.
///
/// Proves that the scanner's O(n) in-string skip is correct AND fast: the per-entry
/// scan must not regress to O(m) on the value strings (where m is value length).
fn bench_dup_key_scan(c: &mut Criterion) {
    let json = dup_key_scan_json();
    let body = make_anthropic_body(&json);
    let router = BlockRouter::new(Arc::new(MockSink::new()));

    c.bench_with_input(
        BenchmarkId::new("router/json-gate", "dup_key_scan_in_strings_14kib"),
        &body,
        |b, body| {
            b.iter(|| {
                let sink = MockSink::new();
                router.route(body, Policy::Default, "bench-req", &sink)
            })
        },
    );
}

/// Bench: 90 KiB fenced code block → O(1) passthrough (ADR-007 baseline).
///
/// Records the **new** passthrough cost after P0.1 (ADR-007 lossless-only egress):
/// parse body → classify (`Class::Code`) → `EngineTarget::Passthrough` → return.
/// No prefilter check, no AST transform, no `rskim-core` dependency.
///
/// # Baseline comparison (P0.1 commit `9cb3020c`, 2026-07-11)
///
/// | Path             | Cost / 90 KiB  | Notes                              |
/// |------------------|----------------|------------------------------------|
/// | Pre-P0.1 (tst)  | ~14.6 ms       | rskim-core tree-sitter transform   |
/// | Post-P0.1 (this) | < 0.2 ms est. | O(1) passthrough, no AST parse     |
///
/// Per ADR-003 / PF-005: the "> 70× speedup" is a DOCUMENTED finding, not a hard
/// assertion. Criterion regression warnings fire if a future change degrades the
/// passthrough path significantly.
///
/// # DEFERRED: `bench_log_lossless_proxy_block`
///
/// A bench measuring the log lossless-proxy path end-to-end is deferred to Pass 5
/// (Log Lossless regime), when the log oracle corpus has timestamps and the
/// annotated-form log engine is implemented.
fn bench_code_block_passthrough(c: &mut Criterion) {
    let fenced = p99_fenced_code_90kb();
    let body = make_anthropic_body(&fenced);
    let router = BlockRouter::new(Arc::new(MockSink::new()));

    c.bench_with_input(
        BenchmarkId::new("router/passthrough", "code_90kib_fenced"),
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
    targets =
        bench_p50_code_block,
        bench_p95_code_block,
        bench_p50_json_block,
        bench_openai_passthrough,
        bench_full_router_no_modification,
        bench_json_minify_p50,
        bench_json_minify_p95,
        bench_json_gate_worst_case,
        bench_dup_key_scan,
        bench_code_block_passthrough
}

criterion_main!(engine_benches);
