//! Criterion benchmarks for rskim-align — worst-case fixture (AC17).
// Benchmark setup code may panic on invalid fixtures — that is the intended
// behavior.  Suppress the crate-level `expect_used` / `unwrap_used` denies here
// since they are not appropriate for criterion harness code.
#![allow(clippy::unwrap_used, clippy::expect_used)]
//!
//! # AC17 — Worst-case latency gate
//!
//! CI asserts p99 < 5 ms on three groups:
//! - `align_anthropic_64tools` — 64 tools, nested schema depth ≥8
//! - `align_anthropic_512kb` — 512 KB multi-turn body
//! - `align_openai_64tools` — same 64 tools, OpenAI format
//!
//! # Stage micro-benchmarks (TEMPORARY INSTRUMENTATION — not wired into CI)
//!
//! Groups `stage_64tools` and `stage_2tools` break the `align()` pipeline into
//! isolated stages so each stage's share of the total ~7.5 ms can be attributed
//! independently. These benchmarks are NOT part of the CI gate — they exist only
//! to identify the dominant hotspot and answer whether the O(n log n)
//! `tools_arrays_set_equal` implementation warrants keeping, reverting, or bounding.
//!
//! Pipeline stages measured:
//!   A  — `locate_top_level_spans` on raw input (serde_json flat map walk)
//!   B  — `sort_tools_array` on the extracted tools value span
//!         (64 × per-element: parse_object_pairs + recursive canonicalize_value ×8 depth)
//!   C  — `canonical_envelope_with_spans` end-to-end (includes B + system + envelope)
//!   D  — `locate_top_level_spans` on canonical output (AD-CA-7 independent re-parse gate)
//!   E  — `tools_arrays_set_equal(original, canonical)` (O(n log n) reorder gate)
//!         (parse_raw_node ×2 arrays + canonical_bytes_of_node ×128 + sort ×2)
//!   F  — Two SHA-256 digests (input + canonical output)
//!   G  — `detect_volatile` (regex scan over input)
//!   H  — `count_client_markers` (cache_control string scan)
//!   I  — `plan_injection` + `inject_tools_marker` (breakpoint planning + splice)
//!
//! # No direct `Instant::now`
//!
//! Bench user code must NOT call `Instant::now` directly (enforced by the
//! `crates/rskim-align/src/` clippy.toml gate). Criterion handles all timing
//! internally. This file lives in `benches/` and is excluded from the CI
//! source-level grep (`grep -rnE ... crates/rskim-align/src`).

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rskim_align::align;
use rskim_align::breakpoint::{count_client_markers, inject_tools_marker, plan_injection};
use rskim_align::canonical_emit::{ToolArrayKind, canonical_envelope_with_spans, sort_tools_array};
use rskim_align::span::locate_top_level_spans;
use rskim_align::stats::sha256;
use rskim_align::volatile::detect_volatile;
use rskim_contract::canonical::tools_arrays_set_equal;
use rskim_llm::Provider;

// ============================================================================
// Fixture helpers (shared with AC17 benches)
// ============================================================================

/// Build a single tool definition with a nested schema at a given depth.
///
/// `depth` controls how many layers of nested `properties` are added.
/// A `depth` of 8 meets the AC17 requirement of "nested schema depth ≥8".
fn build_tool_with_nested_schema(name: &str, depth: usize) -> String {
    // Build the innermost leaf.
    let leaf = r#"{"type":"string"}"#.to_owned();
    // Wrap with nested properties layers.
    let mut schema = leaf;
    for d in 0..depth {
        schema = format!(
            r#"{{"type":"object","properties":{{"nested_{d}":{schema}}},"required":["nested_{d}"]}}"#
        );
    }
    format!(
        r#"{{"name":"{name}","description":"Tool {name} with nested schema depth {depth}","input_schema":{schema}}}"#
    )
}

/// Build an Anthropic request body with N tools, each at the given schema depth.
fn build_anthropic_n_tools(n: usize, depth: usize) -> Vec<u8> {
    let tools: Vec<String> = (0..n)
        .map(|i| build_tool_with_nested_schema(&format!("tool_{i:03}"), depth))
        .collect();
    let tools_json = tools.join(",");
    format!(
        r#"{{"max_tokens":4096,"messages":[{{"content":"What tools are available?","role":"user"}}],"model":"claude-3-5-sonnet-20241022","system":[{{"text":"You are a helpful assistant with many tools.","type":"text"}}],"tools":[{tools_json}]}}"#
    )
    .into_bytes()
}

/// Build a 512 KB multi-turn Anthropic body.
///
/// Uses 16 turns × 32 KB average message pairs to reach approximately 512 KB.
/// The body contains a `tools` array with 4 tools (for marker injection).
fn build_anthropic_512kb_body() -> Vec<u8> {
    // 32 turns × 2 msgs × ~8 KB each = ~512 KB total body.
    let large_content = "B".repeat(8192);
    let mut msgs: Vec<String> = Vec::new();
    for _ in 0..32 {
        msgs.push(format!(r#"{{"content":"{large_content}","role":"user"}}"#,));
        msgs.push(format!(
            r#"{{"content":"{large_content}","role":"assistant"}}"#,
        ));
    }
    let messages = msgs.join(",");
    // 4 tools with depth-8 nested schema.
    let tools: Vec<String> = (0..4)
        .map(|i| build_tool_with_nested_schema(&format!("bench_tool_{i}"), 8))
        .collect();
    let tools_json = tools.join(",");
    format!(
        r#"{{"max_tokens":4096,"messages":[{messages}],"model":"claude-3-5-sonnet-20241022","system":[{{"text":"You are a helpful assistant.","type":"text"}}],"tools":[{tools_json}]}}"#
    )
    .into_bytes()
}

/// Build an OpenAI request body with N tools, each at the given schema depth.
fn build_openai_n_tools(n: usize, depth: usize) -> Vec<u8> {
    let tools: Vec<String> = (0..n)
        .map(|i| {
            // For OpenAI, wrap the reconstructed leaf schema in function.parameters.
            let tool_name = format!("tool_{i:03}");
            let leaf_schema = if depth == 0 {
                r#"{"type":"object","properties":{}}"#.to_owned()
            } else {
                let leaf = r#"{"type":"string"}"#.to_owned();
                let mut schema = leaf;
                for d in 0..depth {
                    schema = format!(
                        r#"{{"type":"object","properties":{{"nested_{d}":{schema}}},"required":["nested_{d}"]}}"#
                    );
                }
                schema
            };
            format!(
                r#"{{"function":{{"description":"Tool {tool_name}","name":"{tool_name}","parameters":{leaf_schema}}},"type":"function"}}"#
            )
        })
        .collect();
    let tools_json = tools.join(",");
    format!(
        r#"{{"messages":[{{"content":"What tools are available?","role":"user"}}],"model":"gpt-4o","tools":[{tools_json}]}}"#
    )
    .into_bytes()
}

// ============================================================================
// Benchmark: 64 tools with nested schema depth 8 (AC17 primary gate)
// ============================================================================

/// AC17 — Worst-case fixture: 64 tools × nested depth 8 (Anthropic).
///
/// This is the primary p99 measurement for the CI gate (< 5 ms).
fn bench_align_anthropic_64tools(c: &mut Criterion) {
    let body = build_anthropic_n_tools(64, 8);
    let body_len = body.len();

    c.bench_with_input(
        BenchmarkId::new("align_anthropic_64tools", format!("{body_len}B")),
        &body,
        |b, body| {
            b.iter(|| {
                let _ = align(
                    criterion::black_box(body),
                    Provider::Anthropic,
                    "bench-64tools-001",
                );
            });
        },
    );
}

// ============================================================================
// Benchmark: 512 KB multi-turn body
// ============================================================================

/// AC17 — 512 KB multi-turn body with 4 tools (Anthropic).
///
/// Tests alignment latency on the large-body path (message content dominates).
fn bench_align_anthropic_512kb(c: &mut Criterion) {
    let body = build_anthropic_512kb_body();
    let body_len = body.len();

    c.bench_with_input(
        BenchmarkId::new("align_anthropic_512kb", format!("{body_len}B")),
        &body,
        |b, body| {
            b.iter(|| {
                let _ = align(
                    criterion::black_box(body),
                    Provider::Anthropic,
                    "bench-512kb-001",
                );
            });
        },
    );
}

// ============================================================================
// Benchmark: OpenAI 64 tools
// ============================================================================

/// AC17 — OpenAI 64 tools × nested depth 8 (no cache_control injection).
///
/// Tests the OpenAI path: canonical key-sort + element reorder, no marker injection.
fn bench_align_openai_64tools(c: &mut Criterion) {
    let body = build_openai_n_tools(64, 8);
    let body_len = body.len();

    c.bench_with_input(
        BenchmarkId::new("align_openai_64tools", format!("{body_len}B")),
        &body,
        |b, body| {
            b.iter(|| {
                let _ = align(
                    criterion::black_box(body),
                    Provider::OpenAi,
                    "bench-openai-64tools-001",
                );
            });
        },
    );
}

// ============================================================================
// Benchmark: original 2-tool baseline (regression guard)
// ============================================================================

static ANTHROPIC_BODY_2TOOLS: &[u8] = br#"{
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

fn bench_align_anthropic_2tools_baseline(c: &mut Criterion) {
    c.bench_function("align_anthropic_canonical_2tools_baseline", |b| {
        b.iter(|| {
            let _ = align(
                criterion::black_box(ANTHROPIC_BODY_2TOOLS),
                Provider::Anthropic,
                "bench-baseline-001",
            );
        });
    });
}

// ============================================================================
// TEMPORARY INSTRUMENTATION: per-stage benchmarks (NOT wired into CI)
//
// These benchmarks decompose the align() pipeline into isolated stages to
// attribute the ~7.5 ms total across specific operations. See module-level
// docs for the stage labelling scheme.
//
// To run only these: `cargo bench -p rskim-align -- stage_`
// ============================================================================

/// Run all pipeline-stage sub-benchmarks for the 64-tool fixture.
///
/// Each sub-benchmark isolates one stage of `try_align_full` and measures it
/// independently. The sum of stages A–I will be less than the full `align()`
/// call because `align()` also includes overhead not covered by individual
/// stages (e.g. match arms, intermediate &str checks, Vec::with_capacity
/// decision points).
fn bench_stages_anthropic_64tools(c: &mut Criterion) {
    // ── Pre-compute shared state (not part of any measurement) ──────────────

    let body = build_anthropic_n_tools(64, 8);
    let body_str: &str = std::str::from_utf8(&body).unwrap();

    // Parsed input spans (used by Stage C and Stage H)
    let input_spans =
        locate_top_level_spans(body_str).expect("64-tool fixture must have valid top-level spans");

    // Extract the raw tools value span (used by Stage B and Stage E)
    let tools_raw: String = input_spans["tools"]
        .extract(body_str)
        .expect("64-tool fixture must have a tools key")
        .to_owned();

    // Canonical form of the body (used by Stage D, Stage E, Stage F)
    let (canonical_bytes, canonical_spans) =
        canonical_envelope_with_spans(body_str, &input_spans, Provider::Anthropic)
            .expect("64-tool fixture must canonicalize without error");
    let canonical_str: String =
        String::from_utf8(canonical_bytes.clone()).expect("canonical bytes must be valid UTF-8");

    // Extract the canonical tools value span (used by Stage E)
    let canonical_tools_raw: String = canonical_spans["tools"]
        .extract(&canonical_str)
        .expect("canonical output must have a tools key")
        .to_owned();

    // Client marker count for the injection stages
    let client_count = count_client_markers(body_str, &input_spans);

    // ── Stage benchmarks ─────────────────────────────────────────────────────

    let mut group = c.benchmark_group("stage_64tools");

    // Stage A: locate_top_level_spans on raw input
    // What: serde_json flat-map parse of the top-level object; records byte spans
    // for each top-level key (5 keys in the 64-tool fixture).
    group.bench_function("A_locate_input_spans", |b| {
        b.iter(|| locate_top_level_spans(criterion::black_box(body_str)))
    });

    // Stage B: sort_tools_array on the extracted tools value span
    // What: parse_array_elements (64 elements) + for each element:
    //   parse_object_pairs + recursive canonicalize_value at depth 8
    //   (9 serde_json parse calls per element × 64 = ~576 total).
    // This is the primary canonicalization hot path.
    group.bench_function("B_sort_tools_array", |b| {
        b.iter(|| {
            sort_tools_array(
                criterion::black_box(tools_raw.as_str()),
                ToolArrayKind::AnthropicTools,
            )
        })
    });

    // Stage C: canonical_envelope_with_spans (full canonicalization including Stage B)
    // What: sorts envelope keys + dispatches to sort_tools_array (tools),
    // canonicalize_value (system), verbatim copy (messages, others). Includes
    // the AD-CA-7 messages self-verify. Stage C - Stage B ≈ envelope overhead.
    group.bench_function("C_canonical_envelope", |b| {
        b.iter(|| {
            canonical_envelope_with_spans(
                criterion::black_box(body_str),
                &input_spans,
                Provider::Anthropic,
            )
        })
    });

    // Stage D: locate_top_level_spans on canonical output (AD-CA-7 independent re-parse gate)
    // What: same as Stage A but on the canonical (post-sort, post-compaction) body.
    // This is the "independent re-parse" envelope path check from AD-CA-7.
    group.bench_function("D_locate_canonical_spans", |b| {
        b.iter(|| locate_top_level_spans(criterion::black_box(canonical_str.as_str())))
    });

    // Stage E: tools_arrays_set_equal(original, canonical) — the O(n log n) reorder gate
    // What: parse_raw_node on BOTH arrays (original 64-tool + canonical 64-tool),
    // building full RawNode trees; canonical_bytes_of_node for 128 elements;
    // sort of 64 Vec<u8> keys × 2; pairwise comparison.
    // This is the AC29 ADR-007 multiset gate for the element reorder.
    group.bench_function("E_tools_set_equal", |b| {
        b.iter(|| {
            tools_arrays_set_equal(
                criterion::black_box(tools_raw.as_str()),
                criterion::black_box(canonical_tools_raw.as_str()),
            )
        })
    });

    // Stage F: two SHA-256 digests (AlignStats::success calls sha256 twice)
    // What: SHA-256(input ~42KB) + SHA-256(canonical ~42KB).
    group.bench_function("F_sha256_both", |b| {
        b.iter(|| {
            let h1 = sha256(criterion::black_box(&body));
            let h2 = sha256(criterion::black_box(&canonical_bytes));
            criterion::black_box((h1, h2))
        })
    });

    // Stage G: volatile detection (warn/stats only; does not steer placement)
    // What: regex pattern scan over the entire input string.
    group.bench_function("G_volatile_detect", |b| {
        b.iter(|| detect_volatile(criterion::black_box(body_str)))
    });

    // Stage H: count_client_markers (pre-injection budget accounting)
    // What: counts "cache_control" occurrences within the tools/system/messages spans.
    group.bench_function("H_count_client_markers", |b| {
        b.iter(|| count_client_markers(criterion::black_box(body_str), &input_spans))
    });

    // Stage I: plan_injection + inject_tools_marker (breakpoint planning + splice)
    // What: determines eligible positions (V1: last tool object + last system block),
    // then injects the marker into the canonical tools array. Excludes the
    // verify_injection call (that is part of apply_injection in lib.rs).
    group.bench_function("I_plan_and_inject_tools", |b| {
        let canonical_tools_bytes = canonical_tools_raw.as_bytes();
        let canonical_system_bytes: Option<Vec<u8>> = canonical_spans
            .get("system")
            .and_then(|s| s.extract(&canonical_str))
            .map(|v| v.as_bytes().to_vec());
        b.iter(|| {
            let plan = plan_injection(
                criterion::black_box(Some(canonical_tools_bytes)),
                criterion::black_box(canonical_system_bytes.as_deref()),
                client_count,
            );
            if plan.skim_count > 0 {
                let _ = inject_tools_marker(criterion::black_box(canonical_tools_bytes));
            }
        })
    });

    group.finish();
}

/// Run all pipeline-stage sub-benchmarks for the 2-tool baseline fixture.
///
/// Used alongside `stage_64tools` to quantify the per-tool scaling factor
/// for each stage independently.
fn bench_stages_anthropic_2tools(c: &mut Criterion) {
    // ── Pre-compute shared state ──────────────────────────────────────────────

    let body_str: &str = std::str::from_utf8(ANTHROPIC_BODY_2TOOLS).unwrap();
    let body_bytes: &[u8] = ANTHROPIC_BODY_2TOOLS;

    let input_spans =
        locate_top_level_spans(body_str).expect("2-tool fixture must have valid top-level spans");

    let tools_raw: String = input_spans["tools"]
        .extract(body_str)
        .expect("2-tool fixture must have a tools key")
        .to_owned();

    let (canonical_bytes, canonical_spans) =
        canonical_envelope_with_spans(body_str, &input_spans, Provider::Anthropic)
            .expect("2-tool fixture must canonicalize without error");
    let canonical_str: String =
        String::from_utf8(canonical_bytes.clone()).expect("canonical bytes must be valid UTF-8");

    let canonical_tools_raw: String = canonical_spans["tools"]
        .extract(&canonical_str)
        .expect("canonical output must have a tools key")
        .to_owned();

    let client_count = count_client_markers(body_str, &input_spans);

    // ── Stage benchmarks ─────────────────────────────────────────────────────

    let mut group = c.benchmark_group("stage_2tools");

    group.bench_function("A_locate_input_spans", |b| {
        b.iter(|| locate_top_level_spans(criterion::black_box(body_str)))
    });

    group.bench_function("B_sort_tools_array", |b| {
        b.iter(|| {
            sort_tools_array(
                criterion::black_box(tools_raw.as_str()),
                ToolArrayKind::AnthropicTools,
            )
        })
    });

    group.bench_function("C_canonical_envelope", |b| {
        b.iter(|| {
            canonical_envelope_with_spans(
                criterion::black_box(body_str),
                &input_spans,
                Provider::Anthropic,
            )
        })
    });

    group.bench_function("D_locate_canonical_spans", |b| {
        b.iter(|| locate_top_level_spans(criterion::black_box(canonical_str.as_str())))
    });

    group.bench_function("E_tools_set_equal", |b| {
        b.iter(|| {
            tools_arrays_set_equal(
                criterion::black_box(tools_raw.as_str()),
                criterion::black_box(canonical_tools_raw.as_str()),
            )
        })
    });

    group.bench_function("F_sha256_both", |b| {
        b.iter(|| {
            let h1 = sha256(criterion::black_box(body_bytes));
            let h2 = sha256(criterion::black_box(&canonical_bytes));
            criterion::black_box((h1, h2))
        })
    });

    group.bench_function("G_volatile_detect", |b| {
        b.iter(|| detect_volatile(criterion::black_box(body_str)))
    });

    group.bench_function("H_count_client_markers", |b| {
        b.iter(|| count_client_markers(criterion::black_box(body_str), &input_spans))
    });

    group.bench_function("I_plan_and_inject_tools", |b| {
        let canonical_tools_bytes = canonical_tools_raw.as_bytes();
        let canonical_system_bytes: Option<Vec<u8>> = canonical_spans
            .get("system")
            .and_then(|s| s.extract(&canonical_str))
            .map(|v| v.as_bytes().to_vec());
        b.iter(|| {
            let plan = plan_injection(
                criterion::black_box(Some(canonical_tools_bytes)),
                criterion::black_box(canonical_system_bytes.as_deref()),
                client_count,
            );
            if plan.skim_count > 0 {
                let _ = inject_tools_marker(criterion::black_box(canonical_tools_bytes));
            }
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_align_anthropic_64tools,
    bench_align_anthropic_512kb,
    bench_align_openai_64tools,
    bench_align_anthropic_2tools_baseline,
);

// TEMPORARY INSTRUMENTATION: stage benchmarks for hotspot attribution.
// Not part of the CI gate. Run with: cargo bench -p rskim-align -- stage_
criterion_group!(
    stages,
    bench_stages_anthropic_64tools,
    bench_stages_anthropic_2tools,
);

criterion_main!(benches, stages);
