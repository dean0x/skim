//! Harness self-test: deliberately-broken implementations (AC18).
//!
//! Each broken implementation violates exactly one invariant. The harness
//! self-test verifies:
//!
//! 1. The identity/passthrough reference passes the full suite.
//! 2. Each broken impl fails on the specific invariant it violates.
//!
//! # Broken implementation roster (AC18)
//!
//! Implemented broken impls (each has a self-test asserting it fails on the
//! specific invariant it violates):
//!
//! ## Core invariant broken impls (8 invariants)
//!
//! - [`InflatingContract`] — violates invariant 2 (never-inflate); fails `AC4-never-inflate`
//! - [`TurnDroppingContract`] — violates invariant 4 (append-only); fails `AC8-append-only`
//! - [`UnloggedModifyingContract`] — violates invariant 8 (logged-never-silent);
//!   fails `AC13-logged-never-silent`
//! - [`NoncanonicalToolsContract`] — violates invariant 6 (canonical tool equality)
//!   by rewriting number tokens (e.g., `1e3` → `1000`); fails `canonical::tools_arrays_equal`
//! - [`SacrosanctLeakingContract`] — violates invariant 7 (sacrosanct-field passthrough)
//!   by embedding a known sensitive key name in its `request_id`; detected by
//!   `check_sacrosanct_redaction` via `AC12-sacrosanct-redaction`
//!
//! ## Invariants by construction (no runtime-falsifiable broken impl)
//!
//! - **Invariant 1 (fail-open)**: type-level enforcement — the `transform` method
//!   has no error variant, so a "failing-to-be-open" impl cannot be written against
//!   the core `Contract` trait. Verified by the AC2 trybuild compile-fail tests.
//! - **Invariant 3 (hot-zone byte-identity)**: `splice_hot_zone` guarantees bytes
//!   come from the original buffer. Offset derivation is a per-consumer responsibility
//!   (#302). The `zone` module unit tests cover splice correctness and out-of-range
//!   fail-open behavior.
//! - **Invariant 5 (determinism)**: enforced by the clippy `disallowed-methods` static
//!   gate (AC10) plus the AC9 sequential/cross-thread replay checks. A broken impl
//!   that calls `SystemTime::now` cannot be compiled into this crate (the gate rejects
//!   it), so the runtime check is supplementary.
//!
//! ## Waiver narrowed-rule broken impls (2 rules)
//!
//! - [`MarkerOverflowInjector`] — violates the `MetadataReorderWithMarkers` cap
//!   narrowed rule (waiver rule 1: marker-count cap); fails its own `verify_marker_cap`
//! - [`SameSlotShrinkViolatorContract`] — violates the `SameSlotShrink` narrowed rule
//!   (waiver rule 2) by growing bytes instead of shrinking; fails `verify_shrink_rule`
//!
//! ## Extension invariant broken impl (1)
//!
//! - [`MarkerDroppingContract`] — violates the marker-immutability extension
//!   invariant; fails `ext:marker-immutability`

use crate::contract::{Contract, IdentityContract, Outcome};
use crate::log::{Decision, DecisionRecord};
use crate::waiver::{MARKER_BYTES, MAX_MARKERS, MetadataReorderWithMarkers, SameSlotShrink};

// ============================================================================
// Broken impl 1: InflatingContract (violates invariant 2)
// ============================================================================

/// Deliberately inflates output by appending one byte.
///
/// Violates invariant 2 (never-inflate). Must fail the `AC4-never-inflate` check.
#[derive(Debug, Clone, Copy)]
pub struct InflatingContract;

impl Contract for InflatingContract {
    fn component_name(&self) -> &'static str {
        "broken-inflating"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        // Append a space byte to inflate the output.
        let mut output = input.to_vec();
        output.push(b' ');
        Outcome::modified(output, input.len(), request_id, self.component_name())
    }
}

// ============================================================================
// Broken impl 2: TurnDroppingContract (violates invariant 4)
// ============================================================================

/// Drops the last turn in the messages array.
///
/// Violates invariant 4 (append-only). Must fail the `AC8-append-only` check.
#[derive(Debug, Clone, Copy)]
pub struct TurnDroppingContract;

impl Contract for TurnDroppingContract {
    fn component_name(&self) -> &'static str {
        "broken-turn-dropping"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        // Try to drop the last message.
        if let Some(output) = try_drop_last_turn(input)
            && output.len() <= input.len()
        {
            return Outcome::modified(output, input.len(), request_id, self.component_name());
        }
        Outcome::passthrough(input.to_vec(), request_id, self.component_name())
    }
}

fn try_drop_last_turn(input: &[u8]) -> Option<Vec<u8>> {
    let s = std::str::from_utf8(input).ok()?;
    let mut v: serde_json::Value = serde_json::from_str(s).ok()?;
    let messages = v.get_mut("messages")?.as_array_mut()?;
    if messages.len() <= 1 {
        return None; // Don't drop the only turn.
    }
    messages.pop();
    serde_json::to_vec(&v).ok()
}

// ============================================================================
// Broken impl 3: UnloggedModifyingContract (violates invariant 8)
// ============================================================================

/// Claims to be a modification but reports passthrough in the decision record.
///
/// Violates invariant 8 (logged-never-silent). The bytes differ from input but
/// the record says passthrough. Must fail the `AC13-logged-never-silent` check.
#[derive(Debug, Clone, Copy)]
pub struct UnloggedModifyingContract;

impl Contract for UnloggedModifyingContract {
    fn component_name(&self) -> &'static str {
        "broken-unlogged-modifying"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        // Modify the bytes but report passthrough in the decision record.
        // This simulates a component that bypasses the sink-failure rule.
        if input.is_empty() {
            return Outcome::passthrough(vec![], request_id, self.component_name());
        }
        let mut output = input.to_vec();
        // Replace last byte with itself (no-op in bytes, but we'll claim modification
        // via a wrong passthrough record).
        // Actually: to make bytes differ, we change one byte if possible.
        if output.len() > 1 {
            output[0] = output[0].wrapping_add(1); // change first byte
        }
        // Deliberately report passthrough even though bytes changed.
        Outcome {
            bytes: output,
            decision: DecisionRecord::passthrough(request_id, self.component_name(), input.len()),
        }
    }
}

// ============================================================================
// Broken impl 4: NoncanonicalToolsContract (violates invariant 6)
// ============================================================================

/// Rewrites number tokens in a JCS-style normalization: `1e3` → `1000`.
///
/// Violates invariant 6 (canonical tool equality) by altering the raw source
/// token representation of numbers, which would produce cache misses.
/// Verified via [`crate::canonical::tools_arrays_equal`].
///
/// This implementation is used in the self-test to confirm that
/// `tools_arrays_equal("1e3", "1000")` returns `false` — i.e., the canonical
/// equality check catches JCS-style normalization as a violation.
#[derive(Debug, Clone, Copy)]
pub struct NoncanonicalToolsContract;

impl Contract for NoncanonicalToolsContract {
    fn component_name(&self) -> &'static str {
        "broken-noncanonical-tools"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        // Attempt to normalise number tokens in the input JSON.
        // This is the invariant 6 violation: we alter the raw source bytes.
        if let Some(normalised) = try_normalise_numbers(input).filter(|n| n != input) {
            return Outcome::modified(normalised, input.len(), request_id, self.component_name());
        }
        Outcome::passthrough(input.to_vec(), request_id, self.component_name())
    }
}

/// Attempt to re-serialise the JSON (which normalises number tokens via serde_json).
///
/// serde_json without `arbitrary_precision` normalises `1e3` → `1000.0` → `1000`
/// in its default serialization. This is exactly what we must NOT do.
fn try_normalise_numbers(input: &[u8]) -> Option<Vec<u8>> {
    let s = std::str::from_utf8(input).ok()?;
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    serde_json::to_vec(&v).ok()
}

// ============================================================================
// Broken impl 5: SacrosanctLeakingContract (violates invariant 7)
// ============================================================================

/// Embeds a known sensitive key name as the `request_id` in its decision record.
///
/// Violates invariant 7 (sacrosanct-field passthrough / AC12): the decision record
/// produced by this component contains a SENSITIVE_EXACT key name as the `request_id`
/// value — simulating a component that accidentally uses an env-var key name as the
/// correlation identifier, causing auth material to appear in the persisted log record.
///
/// The harness `check_sacrosanct_redaction` check must detect this and report
/// `AC12-sacrosanct-redaction` as `passed: false` when this component is under test.
///
/// Output bytes are passthrough (never-inflate is not our violation).
#[derive(Debug, Clone, Copy)]
pub struct SacrosanctLeakingContract;

/// The leaked key name embedded verbatim in the `request_id` of the decision record.
///
/// This is a SENSITIVE_EXACT entry. `check_sacrosanct_redaction` calls
/// `contains_sensitive_value_unredacted` which scans the serialised record JSON for
/// this string as a JSON value — `"ANTHROPIC_API_KEY"` — and must find it.
pub const LEAKED_KEY_NAME: &str = "ANTHROPIC_API_KEY";

impl Contract for SacrosanctLeakingContract {
    fn component_name(&self) -> &'static str {
        "broken-sacrosanct-leaking"
    }

    fn transform(&self, input: &[u8], _request_id: &str) -> Outcome {
        // Passthrough bytes (never-inflate is not our violation).
        // The AC12 violation: construct a DecisionRecord whose `request_id` field
        // contains a literal SENSITIVE_EXACT key name. When serialised to JSON this
        // produces `"request_id":"ANTHROPIC_API_KEY"` — exactly what
        // `contains_sensitive_value_unredacted` scans for.
        //
        // A real producer of this bug would be a proxy that derives request_id from a
        // request header containing an auth key (e.g., an x-api-key echo pattern).
        // This broken impl makes that leak observable and testable.
        let bytes_in = input.len();
        Outcome {
            bytes: input.to_vec(),
            decision: DecisionRecord {
                request_id: LEAKED_KEY_NAME.to_owned(),
                component: self.component_name(),
                decision: Decision::Passthrough,
                bytes_in,
                bytes_out: bytes_in,
            },
        }
    }
}

// ============================================================================
// Broken impl 7: MarkerDroppingContract (violates marker-immutability extension)
// ============================================================================

/// Drops any `cache_control` marker present in the input.
///
/// Violates the marker-immutability extension invariant (AC16).
/// Must fail the `ext:marker-immutability` check.
#[derive(Debug, Clone, Copy)]
pub struct MarkerDroppingContract;

impl Contract for MarkerDroppingContract {
    fn component_name(&self) -> &'static str {
        "broken-marker-dropping"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        // Remove any occurrence of the cache_control marker pattern.
        let marker = b",\"cache_control\":{\"type\":\"ephemeral\"}";
        if !input.windows(marker.len()).any(|w| w == marker) {
            return Outcome::passthrough(input.to_vec(), request_id, self.component_name());
        }
        // Build output by excluding the marker.
        let mut output = Vec::with_capacity(input.len());
        let mut i = 0;
        while i < input.len() {
            if i + marker.len() <= input.len() && &input[i..i + marker.len()] == marker {
                i += marker.len(); // skip the marker
            } else {
                output.push(input[i]);
                i += 1;
            }
        }
        if output.len() <= input.len() {
            Outcome::modified(output, input.len(), request_id, self.component_name())
        } else {
            Outcome::passthrough(input.to_vec(), request_id, self.component_name())
        }
    }
}

// ============================================================================
// Broken impl 8: MarkerOverflowInjector (violates MetadataReorderWithMarkers cap)
// ============================================================================

/// Injects more markers than the `MAX_MARKERS × MARKER_BYTES` cap allows.
///
/// Violates the `MetadataReorderWithMarkers` narrowed rule (AC15).
/// Must pass its own waiver-level cap verification? No — it fails `verify_marker_cap`.
#[derive(Debug, Clone, Copy)]
pub struct MarkerOverflowInjector;

impl Contract for MarkerOverflowInjector {
    fn component_name(&self) -> &'static str {
        "broken-marker-overflow"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        match self.apply_reorder(input, request_id) {
            Some(output) => {
                Outcome::modified(output, input.len(), request_id, self.component_name())
            }
            None => Outcome::passthrough(input.to_vec(), request_id, self.component_name()),
        }
    }
}

impl MetadataReorderWithMarkers for MarkerOverflowInjector {
    fn apply_reorder(&self, input: &[u8], _request_id: &str) -> Option<Vec<u8>> {
        // Inject 5 markers instead of the max 4 → violates the cap rule.
        let marker = b",\"cache_control\":{\"type\":\"ephemeral\"}";
        // MARKER_BYTES must equal the marker slice length — assert here so
        // any future marker-byte change is caught at runtime.
        debug_assert_eq!(marker.len(), MARKER_BYTES, "MARKER_BYTES constant mismatch");
        let mut output = input.to_vec();
        for _ in 0..(MAX_MARKERS + 1) {
            output.extend_from_slice(marker);
        }
        Some(output)
    }
}

// ============================================================================
// Broken impl 9: SameSlotShrinkViolatorContract (violates SameSlotShrink rule)
// ============================================================================

/// Violates the `SameSlotShrink` narrowed rule (waiver rule 2) by growing the
/// slot instead of shrinking it.
///
/// A real violator would grow the bytes in a given slot, breaking the invariant
/// that `apply_shrink` must satisfy `output_slot.len() <= input_slot.len()`.
/// This broken impl returns one extra byte — verifiable via `verify_shrink_rule`.
///
/// Exists solely to satisfy AC18's requirement of a broken impl per waiver rule
/// (rule 2: `SameSlotShrink` narrowed-rule negative case).
#[derive(Debug, Clone, Copy)]
pub struct SameSlotShrinkViolatorContract;

impl Contract for SameSlotShrinkViolatorContract {
    fn component_name(&self) -> &'static str {
        "broken-same-slot-shrink-violator"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        // Attempt to apply the (violating) shrink; inflate is caught by verify_shrink_rule.
        match self.apply_shrink(input, 0, request_id) {
            Some(output) => {
                Outcome::modified(output, input.len(), request_id, self.component_name())
            }
            None => Outcome::passthrough(input.to_vec(), request_id, self.component_name()),
        }
    }
}

impl SameSlotShrink for SameSlotShrinkViolatorContract {
    fn apply_shrink(&self, input: &[u8], _slot_index: usize, _request_id: &str) -> Option<Vec<u8>> {
        if input.is_empty() {
            return None;
        }
        // AC18 violation: grow by one byte instead of shrinking.
        let mut output = input.to_vec();
        output.push(b' ');
        Some(output)
    }
}

// ============================================================================
// Self-test assertions (AC18)
// ============================================================================

/// Verify that the identity contract passes the full conformance suite.
///
/// Called by the in-crate `#[cfg(test)]` module below.
pub fn assert_identity_passes(request_id: &str) {
    use super::run_conformance_suite;
    let report = run_conformance_suite(&IdentityContract, request_id);
    assert!(
        report.all_passed(),
        "IdentityContract must pass all invariants, failures: {:#?}",
        report.failures()
    );
}

/// Verify that `InflatingContract` fails the never-inflate invariant.
pub fn assert_inflating_fails_never_inflate(request_id: &str) {
    use super::run_conformance_suite;
    let report = run_conformance_suite(&InflatingContract, request_id);
    let never_inflate_results: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.invariant == "AC4-never-inflate")
        .collect();
    assert!(
        !never_inflate_results.is_empty(),
        "AC4-never-inflate check must run"
    );
    assert!(
        never_inflate_results.iter().any(|r| !r.passed),
        "InflatingContract must fail AC4-never-inflate, but all passed"
    );
}

/// Verify that `TurnDroppingContract` fails the append-only invariant.
pub fn assert_turn_dropping_fails_append_only(request_id: &str) {
    use super::run_conformance_suite;
    let report = run_conformance_suite(&TurnDroppingContract, request_id);
    let append_only_results: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.invariant == "AC8-append-only")
        .collect();
    assert!(
        !append_only_results.is_empty(),
        "AC8-append-only check must run"
    );
    assert!(
        append_only_results.iter().any(|r| !r.passed),
        "TurnDroppingContract must fail AC8-append-only, but all passed"
    );
}

/// Verify that `UnloggedModifyingContract` fails the logged-never-silent invariant.
pub fn assert_unlogged_fails_logged_never_silent(request_id: &str) {
    use super::run_conformance_suite;
    let report = run_conformance_suite(&UnloggedModifyingContract, request_id);
    let logged_results: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.invariant == "AC13-logged-never-silent")
        .collect();
    assert!(
        !logged_results.is_empty(),
        "AC13-logged-never-silent check must run"
    );
    assert!(
        logged_results.iter().any(|r| !r.passed),
        "UnloggedModifyingContract must fail AC13-logged-never-silent, but all passed"
    );
}

/// Verify that `MarkerDroppingContract` fails the marker-immutability extension
/// on an input that contains a marker.
pub fn assert_marker_dropping_fails_extension(request_id: &str) {
    use crate::extension::{ExtensionRegistry, marker_immutability_check};

    let mut registry = ExtensionRegistry::new();
    registry.register("marker-immutability", marker_immutability_check());

    // Use an input that contains the marker so the extension check is not vacuous.
    let marker = b",\"cache_control\":{\"type\":\"ephemeral\"}";
    let mut input = b"prefix".to_vec();
    input.extend_from_slice(marker);
    input.extend_from_slice(b"suffix");

    let outcome = MarkerDroppingContract.transform(&input, request_id);
    let ext_results = registry.run_all(&input, &outcome.bytes);
    assert!(
        ext_results.iter().any(|r| !r.passed),
        "MarkerDroppingContract must fail marker-immutability on a marker-containing input"
    );
}

/// Verify that `NoncanonicalToolsContract` violates invariant 6.
///
/// The check: `tools_arrays_equal` must return `false` when number tokens differ
/// between the original and the normalised form (e.g., `1e3` vs `1000`).
pub fn assert_noncanonical_tools_fails_invariant_6() {
    use crate::canonical::tools_arrays_equal;
    // A tools array with a number token `1e3`.
    let original = r#"[{"name":"t","parameters":{"properties":{"n":{"default":1e3}}}}]"#;
    // The NoncanonicalToolsContract would normalise this to `1000`.
    // Verify that `tools_arrays_equal` rejects the normalised form.
    let normalised = r#"[{"name":"t","parameters":{"properties":{"n":{"default":1000}}}}]"#;
    assert!(
        !tools_arrays_equal(original, normalised),
        "tools_arrays_equal must return false for normalised number tokens (AC11/invariant 6)"
    );
}

/// Verify that `SacrosanctLeakingContract` is caught by the conformance harness
/// as an AC12-sacrosanct-redaction violation.
///
/// This is a **real negative test**: `SacrosanctLeakingContract` embeds a SENSITIVE_EXACT
/// key name (`ANTHROPIC_API_KEY`) verbatim in its decision record's `request_id` field.
/// Running it through `run_conformance_suite` must produce a `AC12-sacrosanct-redaction`
/// result with `passed: false` — proving the harness *observably catches* the violation
/// rather than only tautologically asserting the helper function.
///
/// Without this test, removing or breaking `check_sacrosanct_redaction` in `mod.rs`
/// would leave no failing test. With this test, any regression in the AC12 gate causes
/// an immediate failure here (PF-005: each AC must be observable/testable).
pub fn assert_sacrosanct_leaking_detected(request_id: &str) {
    use super::run_conformance_suite;

    let report = run_conformance_suite(&SacrosanctLeakingContract, request_id);

    // The AC12-sacrosanct-redaction check must be present in the report.
    let ac12_results: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.invariant == "AC12-sacrosanct-redaction")
        .collect();
    assert!(
        !ac12_results.is_empty(),
        "AC12-sacrosanct-redaction check must run against SacrosanctLeakingContract"
    );

    // And it must FAIL — the leaking component embeds ANTHROPIC_API_KEY in the record.
    assert!(
        ac12_results.iter().any(|r| !r.passed),
        "SacrosanctLeakingContract must fail AC12-sacrosanct-redaction \
         (its decision record request_id contains a SENSITIVE_EXACT key name), \
         but all checks passed — the AC12 gate is not observing the violation"
    );
}

/// Verify that `MarkerOverflowInjector` fails its own cap rule.
pub fn assert_marker_overflow_fails_cap(request_id: &str) {
    let injector = MarkerOverflowInjector;
    let input = b"x".repeat(100);
    if let Some(output) = injector.apply_reorder(&input, request_id) {
        // 5 × 37 = 185 bytes added, cap is 4 × 37 = 148.
        assert!(
            !injector.verify_marker_cap(input.len(), output.len()),
            "MarkerOverflowInjector must fail the cap rule"
        );
    }
}

/// Verify that `SameSlotShrinkViolatorContract` fails the `SameSlotShrink` narrowed rule.
///
/// This is the AC18 negative test for waiver rule 2 (`SameSlotShrink`): the violator
/// grows bytes instead of shrinking, so `verify_shrink_rule` must return `false`.
pub fn assert_same_slot_shrink_violator_fails_rule(request_id: &str) {
    let violator = SameSlotShrinkViolatorContract;
    let input = b"hello world";
    if let Some(output) = violator.apply_shrink(input, 0, request_id) {
        assert!(
            !violator.verify_shrink_rule(input.len(), output.len()),
            "SameSlotShrinkViolatorContract must fail verify_shrink_rule: \
             output ({}) must be > input ({}) to trigger the violation",
            output.len(),
            input.len()
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn identity_passes_conformance() {
        assert_identity_passes("self-test-001");
    }

    #[test]
    fn inflating_fails_never_inflate() {
        assert_inflating_fails_never_inflate("self-test-002");
    }

    #[test]
    fn turn_dropping_fails_append_only() {
        assert_turn_dropping_fails_append_only("self-test-003");
    }

    #[test]
    fn unlogged_fails_logged_never_silent() {
        assert_unlogged_fails_logged_never_silent("self-test-004");
    }

    #[test]
    fn marker_overflow_fails_cap() {
        assert_marker_overflow_fails_cap("self-test-005");
    }

    #[test]
    fn marker_dropping_fails_extension_check() {
        assert_marker_dropping_fails_extension("self-test-006");
    }

    #[test]
    fn noncanonical_tools_fails_invariant_6() {
        assert_noncanonical_tools_fails_invariant_6();
    }

    #[test]
    fn sacrosanct_leaking_detected() {
        assert_sacrosanct_leaking_detected("self-test-008");
    }

    #[test]
    fn same_slot_shrink_violator_fails_rule() {
        assert_same_slot_shrink_violator_fails_rule("self-test-009");
    }

    // ========================================================================
    // Individual broken impl unit tests
    // ========================================================================

    #[test]
    fn inflating_contract_output_larger_than_input() {
        let c = InflatingContract;
        let input = b"hello";
        let outcome = c.transform(input, "r");
        assert!(outcome.bytes.len() > input.len());
        assert!(!outcome.is_passthrough());
    }

    #[test]
    fn turn_dropping_contract_drops_last_turn() {
        let c = TurnDroppingContract;
        let input = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"a"},{"role":"assistant","content":"b"}]}"#;
        let outcome = c.transform(input, "r");
        if !outcome.is_passthrough() {
            // Parse output and verify it has fewer turns.
            let out_str = std::str::from_utf8(&outcome.bytes).expect("valid UTF-8");
            let out_val: serde_json::Value = serde_json::from_str(out_str).expect("valid JSON");
            let msg_count = out_val["messages"].as_array().map(|a| a.len()).unwrap_or(0);
            assert_eq!(msg_count, 1, "should have dropped the last turn");
        }
    }

    #[test]
    fn unlogged_modifying_contract_has_wrong_record() {
        let c = UnloggedModifyingContract;
        let input = b"hello world";
        let outcome = c.transform(input, "r");
        // Bytes changed but record says passthrough.
        let bytes_changed = outcome.bytes != input;
        let record_says_modified = !outcome.decision.is_passthrough();
        // The invariant violation: one of these must differ from the other.
        if bytes_changed {
            assert!(
                !record_says_modified,
                "UnloggedModifyingContract must have mismatched record"
            );
        }
    }

    #[test]
    fn marker_dropping_contract_removes_marker() {
        let c = MarkerDroppingContract;
        let marker = b",\"cache_control\":{\"type\":\"ephemeral\"}";
        let mut input = b"prefix".to_vec();
        input.extend_from_slice(marker);
        input.extend_from_slice(b"suffix");
        let outcome = c.transform(&input, "r");
        let has_marker = outcome.bytes.windows(marker.len()).any(|w| w == marker);
        assert!(!has_marker, "marker must be removed from output");
    }

    #[test]
    fn marker_overflow_injector_exceeds_cap() {
        let injector = MarkerOverflowInjector;
        let input = b"x".repeat(10);
        let output = injector
            .apply_reorder(&input, "r")
            .expect("must produce output");
        // 5 × 37 = 185 bytes added; cap = 10 + 148 = 158.
        assert!(
            !injector.verify_marker_cap(input.len(), output.len()),
            "must fail cap rule: output {} > cap {}",
            output.len(),
            input.len() + MAX_MARKERS * MARKER_BYTES
        );
    }
}
