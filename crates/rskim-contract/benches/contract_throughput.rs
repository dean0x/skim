// AC21 relative-regression criterion bench for rskim-contract.
//
// Per ADR-003: no absolute ms gate. AC21's gate is the code-path/dependency
// assertion (`ac21_default_path_is_byte_length_only` in guardrail.rs, which
// proves the default path never reaches canonicalization / re-serialization);
// this bench is the AC21 "SHOULD" relative-regression tripwire on a >100KB body.
//
// It is run locally as a relative guard:
//   cargo bench -p rskim-contract --bench contract_throughput -- --save-baseline <name>
//   cargo bench -p rskim-contract --bench contract_throughput -- --baseline <name>
// CI wiring for this bench (a committed `.bench-baselines/` baseline + a ci.yml
// step mirroring the rskim-tokens `token_count` bench) is not yet in place — it
// is a follow-up, not a claimed-existing gate. AC20's CI gate is the conformance
// harness + clippy determinism step, not this bench.
//
// What is measured:
// - `guarded_transform` (the default, non-waivered transform path) on a >100KB body.
//   This is the "one structural parse" path that AC21 mandates coverage for.
// - `IdentityContract::transform` on the same body (the zero-overhead passthrough).
// - `parse_request` on the same body (the structural parse step alone).
//
// Per guardrail.rs AC21 cost model, the default path performs exactly:
// - One byte-length comparison (byte_gate)
// - One non-blocking try_send (O(1) channel push)
// - One Outcome construction (zero-copy for passthrough)
// It must NOT reach: canonical, serde_json::to_vec, serde_json::from_str.
// The bench surface here confirms the practical cost of the path on large inputs.

use criterion::{Criterion, criterion_group, criterion_main};
use rskim_contract::contract::{Contract, IdentityContract};
use rskim_contract::guardrail::guarded_transform;
use rskim_contract::log::MockSink;
use rskim_contract::request::parse_request;
use std::sync::Arc;

/// Generate a representative >100KB Anthropic request body.
///
/// Produces a JSON body with a long user message to exceed 100 KB.
/// The structure is realistic: `model`, `max_tokens`, `messages[]`.
fn generate_large_anthropic_body(target_bytes: usize) -> Vec<u8> {
    // Start with the envelope.
    let prefix = br#"{"model":"claude-3-5-sonnet-20241022","max_tokens":4096,"messages":[{"role":"user","content":""#;
    let suffix = br#""}]}"#;
    // Fill the content field with repeated text to exceed target_bytes.
    let overhead = prefix.len() + suffix.len();
    let content_len = target_bytes.saturating_sub(overhead).max(1);
    let content: String = "Hello, this is a benchmark payload for rskim-contract. "
        .chars()
        .cycle()
        .take(content_len)
        .collect();
    let mut body = Vec::with_capacity(target_bytes + 16);
    body.extend_from_slice(prefix);
    body.extend_from_slice(content.as_bytes());
    body.extend_from_slice(suffix);
    body
}

fn bench_default_transform_path(c: &mut Criterion) {
    let body = generate_large_anthropic_body(110_000); // >100KB
    let sink = Arc::new(MockSink::new());
    let identity = IdentityContract;

    let mut group = c.benchmark_group("contract_default_path_100kb");

    // Bench 1: IdentityContract::transform — the zero-overhead passthrough.
    // This is the baseline; guarded_transform on passthrough adds one channel push.
    group.bench_function("identity_transform", |b| {
        b.iter(|| {
            let outcome = identity.transform(&body, "bench-req");
            criterion::black_box(outcome.bytes.len())
        });
    });

    // Bench 2: guarded_transform — the full default-path guardrail.
    // Candidate == input bytes so the gate accepts and dispatches the record.
    // Uses `iter_batched` to drain the MockSink between batches so the sink's
    // internal Vec stays bounded and doesn't skew later-iteration timings with
    // reallocation noise (per review: unbounded sink growth is a measurement-
    // hygiene issue on this relative-regression tripwire).
    group.bench_function("guarded_transform_passthrough_candidate", |b| {
        b.iter_batched(
            || (body.clone(), body.clone()), // setup: clone per-batch
            |(input_clone, candidate)| {
                let outcome =
                    guarded_transform(input_clone, candidate, "bench-req", "bench", &*sink);
                // Drain so the Vec doesn't grow unboundedly across iterations.
                sink.drain();
                criterion::black_box(outcome.bytes.len())
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // Bench 3: parse_request on the large body.
    // AC21 permits "one structural parse"; this measures its cost on >100KB.
    group.bench_function("parse_request_100kb", |b| {
        b.iter(|| {
            let result = parse_request(&body);
            criterion::black_box(result.is_some())
        });
    });

    group.finish();
}

criterion_group!(benches, bench_default_transform_path);
criterion_main!(benches);
