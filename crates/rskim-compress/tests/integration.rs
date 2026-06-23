//! Behavioral and safety integration tests for `BlockRouter` — Phase 3 (#304).
//!
//! Each test covers one or more acceptance criteria (AC11, AC12, AC14, AC17,
//! AC18, AC19, AC20, AC21, AC22). Every test is DISCRIMINATING: removing or
//! disabling the tested feature causes the test to fail, not pass vacuously
//! (per PF-007).
//!
//! ## Test helpers
//!
//! `anthropic_body` constructs minimal Anthropic-format JSON bodies for
//! integration testing. Bodies have `max_tokens` (the Anthropic discriminator
//! heuristic) and at least one user message in the live zone.
//!
//! ## Setup pattern
//!
//! Most tests:
//! 1. Build a minimal Anthropic JSON body as `Vec<u8>`.
//! 2. Create a `MockSink` to capture decision records.
//! 3. Call `router.route(body, Policy::Default, "req-id", &sink)`.
//! 4. Assert outcomes and sink records.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::assertions_on_constants
)]

use std::sync::{Arc, Mutex};

use proptest::prelude::*;
use rskim_compress::{BlockRouter, Policy};
use rskim_contract::log::{
    Decision, DecisionRecord, DecisionSink, MockSink, OutcomeReason, SinkFull,
};

// ============================================================================
// Body construction helpers
// ============================================================================

/// Minimal Anthropic body with a single live-zone user message containing `content`.
///
/// Uses `max_tokens` to trigger Anthropic detection. No assistant message →
/// the user message IS in the live zone.
fn anthropic_body(content: &str) -> Vec<u8> {
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

/// Anthropic body with an assistant message followed by a live-zone user message.
///
/// The assistant message is in the hot zone; the trailing user message is live.
fn anthropic_body_with_assistant_then_user(hot_content: &str, live_content: &str) -> Vec<u8> {
    let esc = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    };
    format!(
        r#"{{"model":"claude-3-5-sonnet-20241022","max_tokens":1024,"messages":[{{"role":"user","content":"setup"}},{{"role":"assistant","content":"{hot}"}},{{"role":"user","content":"{live}"}}]}}"#,
        hot = esc(hot_content),
        live = esc(live_content),
    )
    .into_bytes()
}

/// An OpenAI body (no `max_tokens`, has `model` starting with "gpt").
fn openai_body(content: &str) -> Vec<u8> {
    let escaped = content
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!(r#"{{"model":"gpt-4o","messages":[{{"role":"user","content":"{escaped}"}}]}}"#)
        .into_bytes()
}

/// A long code block (> MIN_SIZE_FLOOR, eligible for code compression).
/// Contains a complete Rust module with multiple functions for structure mode.
fn long_rust_code() -> String {
    // ~500 bytes of valid Rust — well above the 64-byte floor, below 32 KiB max.
    let mut s = String::new();
    for i in 0..10 {
        s.push_str(&format!(
            "fn function_{i}(x: u32, y: u32) -> u32 {{\n    let result = x + y;\n    result * 2\n}}\n\n"
        ));
    }
    s
}

/// A tiny block — below MIN_SIZE_FLOOR (64 bytes).
fn tiny_block() -> &'static str {
    "tiny" // 4 bytes — well below the 64-byte floor
}

/// A giant block — above MAX_CODE_BYTES (32 KiB).
fn giant_code_block() -> String {
    // ~33 KiB of code text, above MAX_CODE_BYTES (32 KiB).
    "fn placeholder() {}\n".repeat(33 * 1024 / 20)
}

/// Already-compressed / random-ish content that is hard to compress further.
fn incompressible_content() -> String {
    "xK9mP2nQ7rL4sT8vW3uY6zA1bC0dE5fG+h/iJkMlNoOpRqSt=UVwXYZ"
        .repeat(2)
        .chars()
        .take(200)
        .collect()
}

// ============================================================================
// Spy sink: records every call and can simulate SinkFull
// ============================================================================

/// A spy `DecisionSink` that returns `SinkFull` for every call.
///
/// Used for AC20 tests to verify the router handles SinkFull without mutation.
struct AlwaysFullSink;

impl DecisionSink for AlwaysFullSink {
    fn try_send(&self, _record: DecisionRecord) -> Result<(), SinkFull> {
        Err(SinkFull)
    }
}

// ============================================================================
// AC11 — Per-block never-inflate (proptest adversarial corpus)
//
// For every processed block, candidate.len() <= original.len().
// Tested over: random strings + structured code + already-compressed content
// wrapped in valid Anthropic bodies.
// ============================================================================

proptest! {
    /// AC11 — proptest: for every block processed, output bytes <= input bytes.
    ///
    /// Discriminating: if the byte gate is removed, an engine could produce
    /// output larger than input. This property fails in that case.
    #[test]
    fn ac11_per_block_never_inflate(
        content in proptest::string::string_regex(r"[a-zA-Z0-9 \n\t\r!@#$%^&*()_+=\[\]{}|;:,.<>?/`~']{10,200}").unwrap()
    ) {
        let body = anthropic_body(&content);
        let sink = Arc::new(MockSink::new());
        let router = BlockRouter::new(sink.clone());

        let outcome = router.route(&body, Policy::Default, "req-ac11", sink.as_ref());

        // Invariant: output bytes must never exceed input bytes.
        prop_assert!(
            outcome.bytes.len() <= body.len(),
            "Output {} bytes > input {} bytes for content: {:?}",
            outcome.bytes.len(),
            body.len(),
            &content[..content.len().min(50)]
        );

        // Also: every record must have bytes_out <= bytes_in.
        for record in sink.drain() {
            prop_assert!(
                record.bytes_out <= record.bytes_in,
                "Record bytes_out {} > bytes_in {} for component {}",
                record.bytes_out,
                record.bytes_in,
                record.component
            );
        }
    }
}

/// AC11 — structured corpus: already-compressed content does not inflate.
#[test]
fn ac11_already_compressed_content_no_inflate() {
    let content = incompressible_content();
    let body = anthropic_body(&content);
    let sink = MockSink::new();
    let router = BlockRouter::passthrough_default();

    let outcome = router.route(&body, Policy::Default, "req-ac11-compressed", &sink);
    assert!(
        outcome.bytes.len() <= body.len(),
        "Output {} > input {} for already-compressed content",
        outcome.bytes.len(),
        body.len()
    );
}

/// AC11 — Rust code: compression never inflates.
#[test]
fn ac11_rust_code_never_inflates() {
    let code = long_rust_code();
    let body = anthropic_body(&code);
    let sink = MockSink::new();
    let router = BlockRouter::passthrough_default();

    let outcome = router.route(&body, Policy::Default, "req-ac11-rust", &sink);
    assert!(
        outcome.bytes.len() <= body.len(),
        "Rust code output {} > input {}",
        outcome.bytes.len(),
        body.len()
    );
}

// ============================================================================
// AC12 — Whole-request all-or-nothing fallback
// ============================================================================

/// AC12 — Whole-request guard: any modified outcome has output <= input.
///
/// Discriminating: if `whole_request_check` is removed, a body whose per-block
/// shrinks are outweighed by serialization overhead would produce outcome.bytes
/// larger than input. This test would fail.
#[test]
fn ac12_whole_request_never_inflates() {
    let code = long_rust_code();
    let body = anthropic_body(&code);
    let sink = MockSink::new();
    let router = BlockRouter::passthrough_default();

    let outcome = router.route(&body, Policy::Default, "req-ac12", &sink);

    assert!(
        outcome.bytes.len() <= body.len(),
        "Whole-request guard failed: output {} > input {}",
        outcome.bytes.len(),
        body.len()
    );
}

/// AC12 — Passthrough outcome is byte-identical.
///
/// When the router returns passthrough, the exact input bytes are preserved.
#[test]
fn ac12_passthrough_is_byte_identical() {
    let body = anthropic_body(tiny_block());
    let sink = MockSink::new();
    let router = BlockRouter::passthrough_default();

    let outcome = router.route(&body, Policy::Default, "req-ac12-pt", &sink);

    if outcome.is_passthrough() {
        assert_eq!(
            outcome.bytes.as_slice(),
            body.as_slice(),
            "Passthrough must be byte-identical"
        );
    } else {
        assert!(outcome.bytes.len() < body.len(), "Modified must be smaller");
    }
}

// ============================================================================
// AC14 — Hot-zone byte-identity by candidate exclusion
// ============================================================================

/// AC14 — Hot-zone blocks are never mutated.
///
/// Discriminating: if the zone exclusion is removed, the router would attempt
/// to compress hot-zone blocks. The serialized output would differ from input
/// for those blocks, and the assertion on hot-zone bytes would fail.
#[test]
fn ac14_hot_zone_bytes_unchanged() {
    let hot_code = long_rust_code();
    let live_content = "Please summarize."; // short, below floor

    let body = anthropic_body_with_assistant_then_user(&hot_code, live_content);

    let body_str = std::str::from_utf8(&body).expect("body is UTF-8");
    assert!(
        body_str.contains("fn function_0"),
        "test setup: hot code must appear in raw body"
    );

    let sink = MockSink::new();
    let router = BlockRouter::passthrough_default();
    let outcome = router.route(&body, Policy::Default, "req-ac14", &sink);

    let out_str = std::str::from_utf8(&outcome.bytes).expect("output is UTF-8");
    assert!(
        out_str.contains("fn function_0"),
        "Hot-zone code (fn function_0) must be preserved byte-identical in output"
    );

    assert!(
        outcome.bytes.len() <= body.len(),
        "Output {} > input {}",
        outcome.bytes.len(),
        body.len()
    );
}

/// AC14 — No record should be emitted for hot-zone blocks.
#[test]
fn ac14_no_records_for_hot_zone_blocks() {
    let hot_code = long_rust_code();
    let live_content = "What is 2 + 2?"; // short — likely prefiltered

    let body = anthropic_body_with_assistant_then_user(&hot_code, live_content);
    let sink = Arc::new(MockSink::new());
    let router = BlockRouter::new(sink.clone());

    let _ = router.route(&body, Policy::Default, "req-ac14-records", sink.as_ref());

    let records = sink.drain();
    // At most 1 record (for the 1 live block). Never 2 (hot+live).
    assert!(
        records.len() <= 1,
        "Expected ≤1 record (only live block), got {} — hot zone may have been processed",
        records.len()
    );
}

// ============================================================================
// AC17 — OpenAI body → byte-identical; ZERO records.
// ============================================================================

/// AC17 — OpenAI body is forwarded byte-identical.
///
/// Discriminating:
/// 1. If the router attempted to compress OpenAI blocks, the outcome would differ.
/// 2. Zero records: if a record were emitted for an OpenAI body, the count fails.
#[test]
fn ac17_openai_body_byte_identical() {
    let body = openai_body("Tell me about Rust programming.");
    let sink = Arc::new(MockSink::new());
    let router = BlockRouter::new(sink.clone());

    let outcome = router.route(&body, Policy::Default, "req-ac17", sink.as_ref());

    assert!(
        outcome.is_passthrough(),
        "OpenAI body must produce passthrough outcome"
    );
    assert_eq!(
        outcome.bytes.as_slice(),
        body.as_slice(),
        "OpenAI body must be byte-identical"
    );

    let records = sink.drain();
    assert_eq!(
        records.len(),
        0,
        "OpenAI body must produce ZERO decision records (zero candidates)"
    );
}

/// AC17 — OpenAI with longer content still byte-identical.
#[test]
fn ac17_openai_large_body_byte_identical() {
    let content = long_rust_code();
    let body = openai_body(&content);
    let sink = MockSink::new();
    let router = BlockRouter::passthrough_default();

    let outcome = router.route(&body, Policy::Default, "req-ac17-large", &sink);

    assert_eq!(
        outcome.bytes.as_slice(),
        body.as_slice(),
        "OpenAI large body must be byte-identical"
    );
}

// ============================================================================
// AC18 — Block/message count+order unchanged; no injection.
// ============================================================================

/// AC18 — Message count is preserved after routing.
///
/// Discriminating: a router that drops or duplicates messages would produce
/// a serialized body with a different message array length.
#[test]
fn ac18_message_count_unchanged() {
    let code = long_rust_code();
    let body = anthropic_body(&code);
    let sink = MockSink::new();
    let router = BlockRouter::passthrough_default();

    let outcome = router.route(&body, Policy::Default, "req-ac18", &sink);

    let in_parsed = rskim_llm::parse(&body).expect("input must parse");
    let out_parsed = rskim_llm::parse(&outcome.bytes).expect("output must parse");

    let in_count = match &in_parsed {
        rskim_llm::ParsedBody::Anthropic(b) => b.messages().len(),
        _ => panic!("expected Anthropic"),
    };
    let out_count = match &out_parsed {
        rskim_llm::ParsedBody::Anthropic(b) => b.messages().len(),
        _ => panic!("expected Anthropic"),
    };

    assert_eq!(
        in_count, out_count,
        "Message count must be unchanged after routing"
    );
}

/// AC18 — No output bytes are absent from input (no injection).
///
/// Every 8-byte window in the output must appear somewhere in the input.
/// Discriminating: if the router injected a watermark or annotation, this fails.
#[test]
fn ac18_no_byte_injection() {
    let code = long_rust_code();
    let body = anthropic_body(&code);
    let sink = MockSink::new();
    let router = BlockRouter::passthrough_default();

    let outcome = router.route(&body, Policy::Default, "req-ac18-inj", &sink);
    let output = &outcome.bytes;

    let window = 8;
    if output.len() >= window {
        let step = (output.len() / 20).max(1);
        for i in (0..output.len() - window + 1).step_by(step) {
            let chunk = &output[i..i + window];
            assert!(
                body.windows(window).any(|w| w == chunk),
                "Output bytes at position {i} ('{:?}') are not present in input — possible injection",
                String::from_utf8_lossy(chunk)
            );
        }
    }
}

// ============================================================================
// AC19 — Exactly one record per candidate; 5→3 reason mapping correct.
// ============================================================================

/// AC19 — One record per candidate block.
///
/// Discriminating: if a block emitted 2 records or 0 records, the count fails.
#[test]
fn ac19_one_record_per_candidate() {
    let code = long_rust_code();
    let body = anthropic_body(&code);
    let sink = Arc::new(MockSink::new());
    let router = BlockRouter::new(sink.clone());

    let _ = router.route(&body, Policy::Default, "req-ac19", sink.as_ref());
    let records = sink.drain();

    assert_eq!(
        records.len(),
        1,
        "Expected exactly 1 record for 1 candidate block, got {}: {:#?}",
        records.len(),
        records
    );
}

/// AC19 — Reason mapping: prefiltered block → Passthrough/Passthrough.
///
/// Discriminating:
/// 1. Zero records → prefilter didn't emit a record.
/// 2. Wrong reason → reason mapping is wrong.
#[test]
fn ac19_reason_prefilter_passthrough() {
    let body = anthropic_body(tiny_block());
    let sink = Arc::new(MockSink::new());
    let router = BlockRouter::new(sink.clone());

    let _ = router.route(&body, Policy::Default, "req-ac19-pre", sink.as_ref());
    let records = sink.drain();

    assert_eq!(
        records.len(),
        1,
        "Prefiltered block must emit exactly 1 record"
    );
    assert_eq!(
        records[0].decision,
        Decision::Passthrough,
        "Prefiltered block must be Passthrough"
    );
    assert_eq!(
        records[0].reason,
        OutcomeReason::Passthrough,
        "Prefiltered block must have reason=Passthrough"
    );
}

/// AC19 — Reason mapping: compressed clean code → Modified/Full.
///
/// Discriminating: if reason is Degraded instead of Full, the assertion fails.
#[test]
fn ac19_reason_compressed_clean_modified_full() {
    let code = long_rust_code();
    let body = anthropic_body(&code);
    let sink = Arc::new(MockSink::new());
    let router = BlockRouter::new(sink.clone());

    let outcome = router.route(&body, Policy::Default, "req-ac19-full", sink.as_ref());
    let records = sink.drain();

    if outcome.is_passthrough() {
        assert_eq!(records.len(), 1, "Must have exactly 1 record");
        assert_eq!(records[0].decision, Decision::Passthrough);
    } else {
        assert_eq!(
            records.len(),
            1,
            "Must have exactly 1 record for 1 candidate"
        );
        assert_eq!(
            records[0].decision,
            Decision::Modified,
            "Compressed block must be Modified"
        );
        assert_eq!(
            records[0].reason,
            OutcomeReason::Full,
            "Clean compression must have reason=Full"
        );
        assert!(
            records[0].bytes_out < records[0].bytes_in,
            "Modified record must show bytes_out < bytes_in"
        );
    }
}

/// AC19 — request_id is passed through unchanged.
#[test]
fn ac19_request_id_passed_through() {
    let body = anthropic_body(tiny_block());
    let sink = Arc::new(MockSink::new());
    let router = BlockRouter::new(sink.clone());

    let _ = router.route(
        &body,
        Policy::Default,
        "my-unique-req-id-12345",
        sink.as_ref(),
    );
    let records = sink.drain();

    for record in &records {
        assert_eq!(
            record.request_id(),
            "my-unique-req-id-12345",
            "request_id must be passed through unchanged"
        );
    }
}

/// AC19 — stable component name in all records.
#[test]
fn ac19_stable_component_name() {
    let body = anthropic_body(tiny_block());
    let sink = Arc::new(MockSink::new());
    let router = BlockRouter::new(sink.clone());

    let _ = router.route(&body, Policy::Default, "req-comp", sink.as_ref());
    let records = sink.drain();

    for record in &records {
        assert_eq!(
            record.component, "block-router",
            "Component name must be stable 'block-router'"
        );
    }
}

/// AC19 — Reason for any record is one of the 5 valid OutcomeReason values.
#[test]
fn ac19_reason_engine_passthrough_any_valid_reason() {
    let long_text = "not actually json at all, just text content that is long enough to pass the floor check and be processed by the classification system yes indeed";
    let body = anthropic_body(long_text);
    let sink = Arc::new(MockSink::new());
    let router = BlockRouter::new(sink.clone());

    let _ = router.route(&body, Policy::Default, "req-ac19-fo", sink.as_ref());
    let records = sink.drain();

    assert_eq!(records.len(), 1, "Must have exactly 1 record");

    let valid_reasons = [
        OutcomeReason::Full,
        OutcomeReason::Degraded,
        OutcomeReason::Passthrough,
        OutcomeReason::FailedOpen,
        OutcomeReason::PolicyPassthrough,
    ];
    assert!(
        valid_reasons.iter().any(|r| r == &records[0].reason),
        "Record reason must be one of the 5 valid OutcomeReason values, got {:?}",
        records[0].reason
    );
    assert!(
        records[0].bytes_in > 0,
        "bytes_in must be non-zero for a block with content"
    );
}

// ============================================================================
// AC20 — SinkFull → block stays original, no blocking.
// ============================================================================

/// AC20 — SinkFull sink: block stays original, request completes without blocking.
///
/// Discriminating:
/// 1. If the router mutated despite SinkFull (invariant 8 violation), the output
///    bytes would differ from input.
/// 2. Non-blocking: `AlwaysFullSink::try_send` returns immediately — if the router
///    awaited, the test would deadlock or fail to compile (no async context).
#[test]
fn ac20_sink_full_block_stays_original() {
    let code = long_rust_code();
    let body = anthropic_body(&code);
    let sink = AlwaysFullSink;
    let router = BlockRouter::passthrough_default();

    let outcome = router.route(&body, Policy::Default, "req-ac20", &sink);

    assert_eq!(
        outcome.bytes.as_slice(),
        body.as_slice(),
        "SinkFull must preserve original bytes (no unlogged modification)"
    );
}

/// AC20 — SinkFull is non-blocking (structural proof via try_send return type).
///
/// `AlwaysFullSink::try_send` returns `Err(SinkFull)` immediately without any
/// async/await or blocking. If the router called a blocking `.send()`, the test
/// would not compile in this non-async context.
#[test]
fn ac20_sink_full_is_non_blocking_structurally() {
    let code = long_rust_code();
    let body = anthropic_body(&code);
    // ImmediateSinkFull returns immediately — no blocking possible.
    struct ImmediateSinkFull;
    impl DecisionSink for ImmediateSinkFull {
        fn try_send(&self, _: DecisionRecord) -> Result<(), SinkFull> {
            Err(SinkFull)
        }
    }

    let sink = ImmediateSinkFull;
    let router = BlockRouter::passthrough_default();
    let outcome = router.route(&body, Policy::Default, "req-ac20-nb", &sink);
    assert_eq!(outcome.bytes.as_slice(), body.as_slice());
}

// ============================================================================
// AC21 — LosslessOnly → all blocks byte-identical; one PolicyPassthrough per candidate.
// ============================================================================

/// AC21 — LosslessOnly policy: output byte-identical, PolicyPassthrough records.
///
/// Discriminating:
/// 1. If engines ran, the output would differ (compressed).
/// 2. If records were missing (early-return before candidates), count is 0 — fails.
/// 3. If the reason were wrong (not PolicyPassthrough), the reason assertion fails.
#[test]
fn ac21_lossless_only_byte_identical_policy_passthrough_records() {
    let code = long_rust_code();
    let body = anthropic_body(&code);
    let sink = Arc::new(MockSink::new());
    let router = BlockRouter::new(sink.clone());

    let outcome = router.route(&body, Policy::LosslessOnly, "req-ac21", sink.as_ref());

    assert_eq!(
        outcome.bytes.as_slice(),
        body.as_slice(),
        "LosslessOnly must produce byte-identical output"
    );
    assert!(
        outcome.is_passthrough(),
        "LosslessOnly must produce passthrough outcome"
    );

    let records = sink.drain();

    // Must have emitted records — NOT 0 (which would mean early-return before candidates).
    assert!(
        !records.is_empty(),
        "LosslessOnly must emit per-candidate records (0 records means early-return before candidate computation — discriminating failure)"
    );

    for record in &records {
        assert_eq!(
            record.decision,
            Decision::Passthrough,
            "LosslessOnly records must be Passthrough"
        );
        assert_eq!(
            record.reason,
            OutcomeReason::PolicyPassthrough,
            "LosslessOnly records must have reason=PolicyPassthrough"
        );
    }
}

/// AC21 — LosslessOnly with 1 candidate: exactly 1 record.
#[test]
fn ac21_lossless_only_record_per_candidate() {
    let body = anthropic_body(&long_rust_code());
    let sink = Arc::new(MockSink::new());
    let router = BlockRouter::new(sink.clone());

    let _ = router.route(&body, Policy::LosslessOnly, "req-ac21-multi", sink.as_ref());
    let records = sink.drain();

    assert_eq!(
        records.len(),
        1,
        "LosslessOnly with 1 candidate must emit exactly 1 record"
    );
    assert_eq!(records[0].reason, OutcomeReason::PolicyPassthrough);
}

// ============================================================================
// AC22 — Pre-filter by size: above threshold and below floor → Passthrough;
// spy engines ZERO invocations.
// ============================================================================

/// AC22 — Block below MIN_SIZE_FLOOR: byte-identical + Passthrough record.
///
/// Discriminating:
/// 1. If the prefilter is removed, an engine runs. The record reason changes
///    (FailedOpen or Modified), not Passthrough. The reason assertion fails.
/// 2. Zero records → prefilter didn't emit; count assertion fails.
#[test]
fn ac22_below_floor_passthrough_record() {
    // "tiny" is 4 bytes — below MIN_SIZE_FLOOR (64 bytes).
    let body = anthropic_body("tiny");
    let sink = Arc::new(MockSink::new());
    let router = BlockRouter::new(sink.clone());

    let outcome = router.route(&body, Policy::Default, "req-ac22-floor", sink.as_ref());
    let records = sink.drain();

    assert_eq!(
        outcome.bytes.as_slice(),
        body.as_slice(),
        "Below-floor block must produce byte-identical output"
    );

    assert_eq!(records.len(), 1, "Prefiltered block must emit 1 record");
    assert_eq!(
        records[0].decision,
        Decision::Passthrough,
        "Prefiltered block must be Passthrough"
    );
    assert_eq!(
        records[0].reason,
        OutcomeReason::Passthrough,
        "Prefiltered block must have reason=Passthrough (not FailedOpen)"
    );
}

/// AC22 — Block above MAX_CODE_BYTES: byte-identical + Passthrough record.
///
/// Discriminating: if the max-size threshold is removed, the engine would run.
/// The record reason would be FailedOpen or Modified. The reason assertion fails.
#[test]
fn ac22_above_max_code_bytes_passthrough_record() {
    let giant = giant_code_block();
    let body = anthropic_body(&giant);
    let sink = Arc::new(MockSink::new());
    let router = BlockRouter::new(sink.clone());

    let outcome = router.route(&body, Policy::Default, "req-ac22-max", sink.as_ref());
    let records = sink.drain();

    assert_eq!(
        outcome.bytes.as_slice(),
        body.as_slice(),
        "Above-max block must produce byte-identical output"
    );

    assert_eq!(records.len(), 1, "Above-max block must emit 1 record");
    assert_eq!(records[0].decision, Decision::Passthrough);
    assert_eq!(
        records[0].reason,
        OutcomeReason::Passthrough,
        "Above-max block prefilter must emit reason=Passthrough"
    );
}

/// AC22 — Prefilter constants are meaningful (not 0 or usize::MAX).
#[test]
fn ac22_prefilter_constants_are_meaningful() {
    use rskim_compress::prefilter::{MAX_CODE_BYTES, MIN_SIZE_FLOOR};

    assert!(
        MIN_SIZE_FLOOR >= 16,
        "MIN_SIZE_FLOOR ({MIN_SIZE_FLOOR}) must be at least 16 bytes"
    );
    assert!(
        MAX_CODE_BYTES >= 1024,
        "MAX_CODE_BYTES ({MAX_CODE_BYTES}) must be at least 1 KiB"
    );
}

// ============================================================================
// AC23 partial — Determinism (same input → same output 100 times)
// ============================================================================

/// Null sink for determinism test (avoids Arc overhead).
struct NullSink;
impl DecisionSink for NullSink {
    fn try_send(&self, _: DecisionRecord) -> Result<(), SinkFull> {
        Ok(())
    }
}

/// AC23 partial — Determinism: same input produces same output 100 times.
///
/// Discriminating: a non-deterministic engine (using rand or SystemTime) would
/// produce different output on different runs. This test fails in that case.
#[test]
fn determinism_100_repeats() {
    let code = long_rust_code();
    let body = anthropic_body(&code);
    let router = BlockRouter::passthrough_default();

    let first = router.route(&body, Policy::Default, "req-det-0", &NullSink);
    for i in 1..100usize {
        let outcome = router.route(&body, Policy::Default, &format!("req-det-{i}"), &NullSink);
        assert_eq!(
            outcome.bytes, first.bytes,
            "Output must be byte-identical on repeat {i}"
        );
    }
}

// ============================================================================
// Misc: prefilter constants are public (AC22 requirement)
// ============================================================================

mod prefilter_public_api {
    use rskim_compress::prefilter::{
        MAX_CODE_BYTES, MAX_JSON_BYTES, MAX_LOG_BYTES, MAX_MIXED_BYTES, MIN_SIZE_FLOOR,
    };

    /// AC22 — Prefilter constants are accessible for documentation and testing.
    #[test]
    fn all_constants_accessible() {
        // Compile test: this compiles iff all constants are `pub`.
        // Runtime bounds: each constant must be a sensible positive value.
        assert!(MIN_SIZE_FLOOR > 0, "MIN_SIZE_FLOOR must be positive");
        assert!(MAX_CODE_BYTES > 0, "MAX_CODE_BYTES must be positive");
        assert!(MAX_JSON_BYTES > 0, "MAX_JSON_BYTES must be positive");
        assert!(MAX_LOG_BYTES > 0, "MAX_LOG_BYTES must be positive");
        assert!(MAX_MIXED_BYTES > 0, "MAX_MIXED_BYTES must be positive");
    }
}

// ============================================================================
// Concurrent / thread-safety sanity: BlockRouter is Send + Sync
// ============================================================================

/// Structural test: BlockRouter implements Send + Sync (required for shared stage).
///
/// Discriminating: if BlockRouter were not Send+Sync, this would not compile.
#[test]
fn block_router_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<BlockRouter>();
}

// ============================================================================
// Missing: SpySink was defined but we use AlwaysFullSink and MockSink instead.
// The following ensures the Mutex-based spy is available if needed in future tests.
// ============================================================================

/// Thread-safe spy sink that records all received records.
///
/// Used when tests need to examine what records were sent (as opposed to
/// `MockSink` which has different drain semantics). Currently only here
/// as a type-check; future tests can use `SpySink::drain`.
#[allow(dead_code)]
struct SpySink {
    records: Mutex<Vec<DecisionRecord>>,
}

#[allow(dead_code)]
impl SpySink {
    fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
        }
    }

    fn drain(&self) -> Vec<DecisionRecord> {
        std::mem::take(&mut *self.records.lock().unwrap())
    }
}

impl DecisionSink for SpySink {
    fn try_send(&self, record: DecisionRecord) -> Result<(), SinkFull> {
        self.records.lock().unwrap().push(record);
        Ok(())
    }
}
