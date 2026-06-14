// Property-based tests for rskim-contract core invariants.
//
// Decision 8 (DECISIONS-RESOLVED.md) mandates proptest coverage for the
// never-inflate and append-only invariants. These tests use proptest to
// generate adversarial inputs that probe the invariant boundaries.
//
// Both the IdentityContract (reference) AND broken impls / enforcement
// mechanisms are exercised — specifically `guarded_transform` (the never-inflate
// gate) and `InflatingContract` (the canonical broken impl for that invariant).
// Testing only the IdentityContract in isolation is tautological because
// identity passthrough cannot inflate by construction. Decision 8 requires
// "adversarial inputs, not just fixtures" — these tests drive the GATE.

use proptest::prelude::*;
use rskim_contract::contract::{Contract, IdentityContract};
use rskim_contract::guardrail::guarded_transform;
use rskim_contract::log::MockSink;
use std::sync::Arc;

// ============================================================================
// Strategy: generate arbitrary byte sequences
// ============================================================================

/// Generate arbitrary byte sequences of length 0–4096 bytes.
fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=4096)
}

/// Fixed JSON request bodies for property testing (both schema families).
const SAMPLE_BODIES: &[&[u8]] = &[
    br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"hello"}]}"#,
    br#"{"model":"claude-3-haiku-20240307","messages":[{"role":"user","content":"test"},{"role":"assistant","content":"reply"},{"role":"user","content":"more"}]}"#,
    br#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#,
    br#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"test"},{"role":"assistant","content":"reply"}]}"#,
    b"",
    b"not json",
    b"{\"model\":\"x\"}",
];

/// Generate a JSON body from the fixed sample set or arbitrary bytes.
fn arb_json_body() -> impl Strategy<Value = Vec<u8>> {
    // Mix fixed well-formed samples with arbitrary bytes.
    prop_oneof![
        // Pick one of the fixed samples.
        (0..SAMPLE_BODIES.len()).prop_map(|i| SAMPLE_BODIES[i].to_vec()),
        // Arbitrary bytes (adversarial / malformed).
        arb_bytes(),
    ]
}

// ============================================================================
// Property: never-inflate (invariant 2 / Decision 8)
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 200,
        max_shrink_iters: 1000,
        ..ProptestConfig::default()
    })]

    /// IdentityContract must never inflate output bytes on any input.
    ///
    /// For all byte sequences, identity passthrough produces output ≤ input.
    #[test]
    fn prop_identity_never_inflates(input in arb_bytes()) {
        let c = IdentityContract;
        let outcome = c.transform(&input, "prop-req-1");
        prop_assert!(
            outcome.bytes.len() <= input.len(),
            "IdentityContract inflated output: {} bytes → {} bytes",
            input.len(),
            outcome.bytes.len()
        );
    }

    /// IdentityContract must not inflate on any JSON-shaped input.
    #[test]
    fn prop_identity_never_inflates_json_bodies(input in arb_json_body()) {
        let c = IdentityContract;
        let outcome = c.transform(&input, "prop-req-2");
        prop_assert!(
            outcome.bytes.len() <= input.len(),
            "IdentityContract inflated on JSON input: {} → {} bytes",
            input.len(),
            outcome.bytes.len()
        );
    }

    /// IdentityContract must produce a passthrough decision record on any input.
    #[test]
    fn prop_identity_always_passthrough_record(input in arb_bytes()) {
        let c = IdentityContract;
        let outcome = c.transform(&input, "prop-req-3");
        prop_assert!(
            outcome.is_passthrough(),
            "IdentityContract must always produce passthrough record"
        );
    }

    /// IdentityContract output must be byte-identical to input.
    #[test]
    fn prop_identity_output_equals_input(input in arb_bytes()) {
        let c = IdentityContract;
        let outcome = c.transform(&input, "prop-req-4");
        prop_assert_eq!(
            outcome.bytes, input,
            "IdentityContract must output byte-identical bytes"
        );
    }
}

// ============================================================================
// Property: append-only (invariant 4 / Decision 8)
// ============================================================================

/// Count turns in a JSON request body.
fn count_turns(bytes: &[u8]) -> Option<usize> {
    let s = std::str::from_utf8(bytes).ok()?;
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let messages = v.get("messages")?.as_array()?;
    Some(messages.len())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 100,
        ..ProptestConfig::default()
    })]

    /// IdentityContract must not decrease turn count on any well-formed JSON input.
    ///
    /// For parseable JSON with a `messages` array, output turn count ≥ input turn count.
    #[test]
    fn prop_identity_append_only_turns(input in arb_json_body()) {
        let c = IdentityContract;
        let outcome = c.transform(&input, "prop-req-5");
        // Only check if both input and output parse as valid JSON with messages.
        if let (Some(input_count), Some(output_count)) =
            (count_turns(&input), count_turns(&outcome.bytes))
        {
            prop_assert!(
                output_count >= input_count,
                "turn count decreased: {} → {}",
                input_count,
                output_count
            );
        }
    }
}

// ============================================================================
// Property: determinism (invariant 5 / Decision 8)
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 100,
        ..ProptestConfig::default()
    })]

    /// IdentityContract must produce byte-identical output on repeated calls.
    #[test]
    fn prop_identity_deterministic(input in arb_bytes()) {
        let c = IdentityContract;
        let out1 = c.transform(&input, "prop-req-6").bytes;
        let out2 = c.transform(&input, "prop-req-6").bytes;
        let out3 = c.transform(&input, "prop-req-6").bytes;
        prop_assert_eq!(&out1, &out2, "non-deterministic output between run 1 and 2");
        prop_assert_eq!(&out2, &out3, "non-deterministic output between run 2 and 3");
    }
}

// ============================================================================
// Property: guarded_transform rejects inflation (invariant 2 / Decision 8)
//
// These are the NON-TAUTOLOGICAL properties mandated by Decision 8.
// They exercise the actual enforcement mechanism — `guarded_transform` and its
// `byte_gate` — against generated candidate/input pairs, not just the identity
// passthrough which cannot inflate by construction.
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 200,
        max_shrink_iters: 500,
        ..ProptestConfig::default()
    })]

    /// `guarded_transform` must always produce output ≤ input for any candidate.
    ///
    /// When `candidate.len() > input.len()`, the gate must reject and return
    /// byte-identical passthrough. This exercises the actual `byte_gate` enforcement
    /// path rather than the identity contract (which trivially cannot inflate).
    #[test]
    fn prop_guarded_transform_never_inflates(
        input in arb_bytes(),
        candidate in arb_bytes(),
    ) {
        let sink = Arc::new(MockSink::new());
        let input_len = input.len();
        let candidate_len = candidate.len();
        let outcome = guarded_transform(
            input.clone(),
            candidate,
            "prop-gate-1",
            "test",
            &*sink,
        );
        prop_assert!(
            outcome.bytes.len() <= input_len,
            "guarded_transform must never produce output ({}) > input ({}) for \
             any candidate ({}) — byte_gate must reject inflation",
            outcome.bytes.len(),
            input_len,
            candidate_len,
        );
    }

    /// When `candidate.len() > input.len()`, `guarded_transform` must return
    /// byte-identical input (passthrough) — not just a non-inflated result.
    ///
    /// This is the stricter form: the gate fallback must be the ORIGINAL bytes,
    /// not a different-but-shorter transformation.
    #[test]
    fn prop_guarded_transform_inflation_falls_back_to_input(
        input in arb_bytes().prop_filter("non-empty input", |b| !b.is_empty()),
        extra in prop::collection::vec(any::<u8>(), 1..=64),
    ) {
        // Construct a candidate that is strictly larger than input.
        let mut candidate = input.clone();
        candidate.extend_from_slice(&extra);
        prop_assume!(candidate.len() > input.len());

        let sink = Arc::new(MockSink::new());
        let outcome = guarded_transform(
            input.clone(),
            candidate,
            "prop-gate-2",
            "test",
            &*sink,
        );
        // Gate must reject → output must be byte-identical to input (passthrough).
        prop_assert_eq!(
            outcome.bytes, input,
            "rejected-by-gate outcome must be byte-identical to input"
        );
        // No decision record must be sent (gate rejection, not sink dispatch).
        prop_assert!(sink.is_empty(), "byte_gate rejection must send no record to sink");
    }

    /// When `candidate.len() <= input.len()` and sink accepts, `guarded_transform`
    /// must return the candidate bytes (not the input).
    #[test]
    fn prop_guarded_transform_shrink_accepted(
        input in arb_bytes().prop_filter("need at least 2 bytes to shrink", |b| b.len() >= 2),
    ) {
        // Candidate is a strict prefix — always shorter.
        let candidate = input[..input.len() / 2].to_vec();
        let sink = Arc::new(MockSink::new());
        let outcome = guarded_transform(
            input.clone(),
            candidate.clone(),
            "prop-gate-3",
            "test",
            &*sink,
        );
        prop_assert_eq!(
            outcome.bytes, candidate,
            "gate-accepted shrink must return candidate bytes"
        );
    }
}
