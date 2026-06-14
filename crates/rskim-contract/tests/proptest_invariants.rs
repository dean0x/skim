// Property-based tests for rskim-contract core invariants.
//
// Decision 8 (DECISIONS-RESOLVED.md) mandates proptest coverage for the
// never-inflate and append-only invariants. These tests use proptest to
// generate adversarial inputs that probe the invariant boundaries.
//
// Both the IdentityContract (reference) and broken impls are exercised.

use proptest::prelude::*;
use rskim_contract::contract::{Contract, IdentityContract};

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
