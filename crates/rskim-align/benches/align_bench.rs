//! Criterion benchmarks for rskim-align — worst-case fixture (AC17).
//!
//! # AC17 — Worst-case latency gate
//!
//! CI asserts p99 < 5 ms on three groups:
//! - `align_anthropic_64tools` — 64 tools, nested schema depth ≥8
//! - `align_anthropic_512kb` — 512 KB multi-turn body
//! - `align_openai_64tools` — same 64 tools, OpenAI format
//!
//! # No direct `Instant::now`
//!
//! Bench user code must NOT call `Instant::now` directly (enforced by the
//! `crates/rskim-align/src/` clippy.toml gate). Criterion handles all timing
//! internally. This file lives in `benches/` and is excluded from the CI
//! source-level grep (`grep -rnE ... crates/rskim-align/src`).

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rskim_align::align;
use rskim_llm::Provider;

// ============================================================================
// Fixture helpers
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
// Benchmark: 64 tools with nested schema depth 8
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

criterion_group!(
    benches,
    bench_align_anthropic_64tools,
    bench_align_anthropic_512kb,
    bench_align_openai_64tools,
    bench_align_anthropic_2tools_baseline,
);
criterion_main!(benches);
