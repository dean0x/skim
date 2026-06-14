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
//! - [`InflatingContract`] — violates invariant 2 (never-inflate); fails `AC4-never-inflate`
//! - [`TurnDroppingContract`] — violates invariant 4 (append-only); fails `AC8-append-only`
//! - [`UnloggedModifyingContract`] — violates invariant 8 (logged-never-silent);
//!   fails `AC13-logged-never-silent`
//! - [`MarkerOverflowInjector`] — violates the `MetadataReorderWithMarkers` cap
//!   narrowed rule (waiver rule 1); fails its own `verify_marker_cap`
//! - [`MarkerDroppingContract`] — violates the marker-immutability extension
//!   invariant; fails `ext:marker-immutability`
//!
//! Invariants 1 (fail-open), 3 (hot-zone byte-identity), and 5 (determinism) are
//! enforced *by construction* rather than by a runtime-falsifiable broken impl:
//! fail-open is type-level (the `transform` method has no error variant, so a
//! "failing-open" impl cannot be written); hot-zone byte-identity is guaranteed
//! by splice from the original buffer (`zone::splice_hot_zone`), and offset
//! derivation is a per-consumer responsibility (#302); and determinism is
//! enforced by the clippy `disallowed-methods` static gate plus the replay check.
//! Negative coverage for invariants 6 (canonical tool equality) and 7 (sacrosanct
//! fields), and for the second waiver rule (non-block-form marker positions),
//! lands with the consumer that owns the precise byte-offset model (#302/#306);
//! see the `canonical` and `request` module unit tests for the positive-side
//! checks available at this layer.

use crate::contract::{Contract, IdentityContract, Outcome};
use crate::log::DecisionRecord;
use crate::waiver::{MARKER_BYTES, MAX_MARKERS, MetadataReorderWithMarkers};

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
// Broken impl 4: MarkerDroppingContract (violates marker-immutability extension)
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
// Broken impl 5: MarkerOverflowInjector (violates MetadataReorderWithMarkers cap)
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
// Self-test assertions (AC18)
// ============================================================================

/// Verify that the identity contract passes the full conformance suite.
///
/// Called by the integration test in `tests/conformance.rs`.
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
