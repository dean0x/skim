//! Typed waiver mechanism for the two sanctioned exceptions to default-deny.
//!
//! # Default-deny design
//!
//! The core [`crate::contract::Contract`] trait has no surface that can grow bytes
//! or touch the hot zone. Absence of a waiver trait *is* the deny.
//!
//! Two sanctioned exceptions exist, each modeled as a **narrowed trait** whose
//! method signature encodes the narrowed rule directly:
//!
//! 1. [`MetadataReorderWithMarkers`] — metadata-only reorder + bounded
//!    `cache_control` marker injection (Decision 7 / #306)
//! 2. [`SameSlotShrink`] — same-array-slot byte-shrinking turn edit (#307)
//!
//! # Why narrowed traits over capability tokens
//!
//! A narrowed trait makes the carve-out self-documenting and harness-checkable.
//! The method signature already encodes the invariant; a free-floating capability
//! token would push the rule into runtime branching where it is easy to forget.
//!
//! # MARKER_BYTES constant
//!
//! `MARKER_BYTES = 37` is the byte length of the minimal JSON `cache_control`
//! insertion: `,\"cache_control\":{\"type\":\"ephemeral\"}`. This is pinned by
//! #306's compact serialization spec, not invented. The cap `4 × MARKER_BYTES`
//! comes from the maximum of 4 injected markers per request (#306 design).
//!
//! Arithmetic is done in `u64` with `checked_add` / `saturating_add` to prevent
//! overflow on 32-bit targets or with adversarial huge `input.len()` (PF-004).

/// Byte length of one `cache_control` marker injection.
///
/// Derived from the minimal JSON form of `,"cache_control":{"type":"ephemeral"}`.
/// Verified by `debug_assert_eq!(marker.len(), MARKER_BYTES)` in `apply_reorder`
/// and the `marker_bytes_constant_is_correct` unit test — the string is 37 bytes.
///
/// **Plan/AC15 supersession note (ADR-003):** the 301 plan, risk table, and
/// DECISIONS-RESOLVED.md Decision 7 cited `MARKER_BYTES = 38` as an estimate.
/// The verified compact-serialization length is 37 bytes; 37 supersedes 38 per
/// ADR-003 (numeric criteria must trace to a measured basis, not an invented figure).
/// The code is correct; the plan figure was off by one.
pub const MARKER_BYTES: usize = 37;

/// Maximum number of markers injected per request by the `#306` component.
pub const MAX_MARKERS: usize = 4;

/// The upper bound on output bytes for waivered marker injection.
///
/// `len(output) ≤ len(input) + MAX_MARKERS × MARKER_BYTES`
///
/// Computed in `u64` to avoid overflow; the result is compared against
/// the actual `u64` output length (PF-004).
///
/// Returns `None` if `input_len + 4 × 37` would overflow `u64` (should never
/// happen on any real input, but safe-by-default).
pub fn marker_injection_cap(input_len: usize) -> Option<u64> {
    let input_u64 = input_len as u64;
    let addition = (MAX_MARKERS as u64).checked_mul(MARKER_BYTES as u64)?; // 4 × 37 = 148; cannot overflow u64
    input_u64.checked_add(addition)
}

/// Waiver trait for #306: metadata-only reorder + bounded `cache_control` marker injection.
///
/// # Invariant encoded in the trait
///
/// The return value of `apply_reorder` MUST satisfy:
/// `output.len() as u64 ≤ marker_injection_cap(input.len())`
///
/// In addition:
/// - Markers MUST be injected only at block-form positions in the `content[]` array,
///   never inside the `messages` array structure itself.
/// - Only `metadata` fields may be reordered; no message content may be modified.
/// - The output is verified by the harness before acceptance (AC15).
///
/// # Default deny
///
/// A component that only implements [`crate::contract::Contract`] (without this
/// waiver trait) cannot inject markers. The type system enforces this.
///
/// # Block-form position constraint
///
/// "Block-form position" means a position inside a `content: [...]` array where
/// content blocks appear as `{"type": "text", ...}` objects. Marker injection at
/// message level or at scalar `"content": "string"` positions is prohibited.
pub trait MetadataReorderWithMarkers: crate::contract::Contract {
    /// Apply a metadata-only reorder with bounded marker injection.
    ///
    /// # Arguments
    ///
    /// - `input` — original request bytes
    /// - `request_id` — caller-assigned request identifier
    ///
    /// # Returns
    ///
    /// Output bytes where:
    /// - `output.len() as u64 ≤ marker_injection_cap(input.len())`
    /// - Markers appear only at block-form content positions
    /// - Only metadata fields are reordered
    ///
    /// Returns `None` to trigger passthrough (fail-open), e.g., on parse failure.
    fn apply_reorder(&self, input: &[u8], request_id: &str) -> Option<Vec<u8>>;

    /// Verify the waiver rule for the output produced by `apply_reorder`.
    ///
    /// Returns `true` if `output` satisfies the marker injection cap.
    /// The harness calls this to verify the narrowed rule.
    fn verify_marker_cap(&self, input_len: usize, output_len: usize) -> bool {
        let output_u64 = output_len as u64;
        match marker_injection_cap(input_len) {
            Some(cap) => output_u64 <= cap,
            // Overflow → fail-open (treat as cap exceeded → return false → passthrough).
            None => false,
        }
    }
}

/// Waiver trait for #307: same-array-slot byte-shrinking turn edit.
///
/// # Invariant encoded in the trait
///
/// The return value of `apply_shrink` for a given slot MUST satisfy:
/// `output_slot_bytes.len() ≤ input_slot_bytes.len()`
///
/// In addition:
/// - Only the content of the specified slot may change.
/// - The slot index must remain the same (no reorder, no delete).
/// - The output is verified by the harness before acceptance (AC15).
///
/// # Default deny
///
/// A component that only implements [`crate::contract::Contract`] (without this
/// waiver trait) cannot perform shrinking edits.
pub trait SameSlotShrink: crate::contract::Contract {
    /// Apply a byte-shrinking edit to a single message slot.
    ///
    /// # Arguments
    ///
    /// - `input` — original request bytes
    /// - `slot_index` — zero-based index of the turn to shrink (within messages array)
    /// - `request_id` — caller-assigned request identifier
    ///
    /// # Returns
    ///
    /// Modified request bytes where the specified slot is shorter than the original,
    /// or `None` to trigger passthrough.
    fn apply_shrink(&self, input: &[u8], slot_index: usize, request_id: &str) -> Option<Vec<u8>>;

    /// Verify the same-slot-shrink rule: output ≤ input.
    ///
    /// Returns `true` if `output_len ≤ input_len`.
    fn verify_shrink_rule(&self, input_len: usize, output_len: usize) -> bool {
        output_len <= input_len
    }
}

// ============================================================================
// Mock waivered components (for AC15 harness tests)
// ============================================================================
//
// These mock types are gated behind `#[cfg(any(test, feature = "harness"))]` so
// they are absent from production builds (release binaries, downstream #306/#307
// that do not enable the harness feature). Without this gate the types would
// appear in the public API surface of every consumer of rskim-contract and be
// compiled into all release binaries — a minor SRP/layering leak. The gate
// makes test fixtures stay in test infrastructure.

/// Mock marker-injection component that passes its narrowed rule.
///
/// Injects a single mock marker (exactly MARKER_BYTES bytes) into a copy of the
/// input. Used by AC15 to verify the waiver passes its narrowed rule while failing
/// the strict baseline (which requires output.len() ≤ input.len()).
///
/// Only available under `#[cfg(any(test, feature = "harness"))]`.
#[cfg(any(test, feature = "harness"))]
#[derive(Debug, Clone, Copy)]
pub struct MockMarkerInjector;

#[cfg(any(test, feature = "harness"))]
impl crate::contract::Contract for MockMarkerInjector {
    fn component_name(&self) -> &'static str {
        "mock-marker-injector"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> crate::contract::Outcome {
        // Attempt to inject one mock marker.
        match self.apply_reorder(input, request_id) {
            Some(output) => {
                // Verify the waiver rule before emitting.
                if !self.verify_marker_cap(input.len(), output.len()) {
                    return crate::contract::Outcome::passthrough(
                        input.to_vec(),
                        request_id,
                        self.component_name(),
                    );
                }
                // The output is larger than input (by marker bytes), so the
                // strict never-inflate gate would reject it. This is intentional:
                // the waivered path bypasses the strict gate.
                crate::contract::Outcome::modified(
                    output,
                    input.len(),
                    request_id,
                    self.component_name(),
                )
            }
            None => crate::contract::Outcome::passthrough(
                input.to_vec(),
                request_id,
                self.component_name(),
            ),
        }
    }
}

#[cfg(any(test, feature = "harness"))]
impl MetadataReorderWithMarkers for MockMarkerInjector {
    fn apply_reorder(&self, input: &[u8], _request_id: &str) -> Option<Vec<u8>> {
        // Inject one mock marker (MARKER_BYTES bytes) to simulate #306 injection.
        let marker = b",\"cache_control\":{\"type\":\"ephemeral\"}";
        debug_assert_eq!(marker.len(), MARKER_BYTES);
        let mut output = input.to_vec();
        output.extend_from_slice(marker);
        Some(output)
    }
}

/// Mock same-slot-shrink component that passes its narrowed rule.
///
/// Truncates the input by one byte (if possible) to simulate #307 shrinking.
///
/// Only available under `#[cfg(any(test, feature = "harness"))]`.
#[cfg(any(test, feature = "harness"))]
#[derive(Debug, Clone, Copy)]
pub struct MockSameSlotShrinker;

#[cfg(any(test, feature = "harness"))]
impl crate::contract::Contract for MockSameSlotShrinker {
    fn component_name(&self) -> &'static str {
        "mock-same-slot-shrinker"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> crate::contract::Outcome {
        match self.apply_shrink(input, 0, request_id) {
            Some(output) => crate::contract::Outcome::modified(
                output,
                input.len(),
                request_id,
                self.component_name(),
            ),
            None => crate::contract::Outcome::passthrough(
                input.to_vec(),
                request_id,
                self.component_name(),
            ),
        }
    }
}

#[cfg(any(test, feature = "harness"))]
impl SameSlotShrink for MockSameSlotShrinker {
    fn apply_shrink(&self, input: &[u8], _slot_index: usize, _request_id: &str) -> Option<Vec<u8>> {
        if input.is_empty() {
            return None; // Cannot shrink empty input.
        }
        Some(input[..input.len() - 1].to_vec())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::contract::Contract;

    #[test]
    fn marker_injection_cap_nominal() {
        let cap = marker_injection_cap(1000).expect("must not overflow");
        // 1000 + 4 * 37 = 1000 + 148 = 1148
        assert_eq!(cap, 1148);
    }

    #[test]
    fn marker_injection_cap_zero_input() {
        let cap = marker_injection_cap(0).expect("must not overflow");
        assert_eq!(cap, 148); // 4 × 37
    }

    #[test]
    fn marker_injection_cap_large_input_no_overflow() {
        // usize::MAX as u64 would be 18446744073709551615 on 64-bit;
        // + 148 overflows u64 → checked_add returns None.
        // On 64-bit: u64::MAX + 148 overflows.
        // Test that we handle the extreme case without panic.
        let large = (u64::MAX - 100) as usize;
        // This should return None because large_u64 + 148 > u64::MAX.
        let result = marker_injection_cap(large);
        assert!(result.is_none());
    }

    #[test]
    fn mock_marker_injector_passes_cap_rule() {
        let injector = MockMarkerInjector;
        let input = b"x".repeat(100);
        let output = injector
            .apply_reorder(&input, "req-1")
            .expect("must produce output");
        assert!(injector.verify_marker_cap(input.len(), output.len()));
        // Output is input + 1 marker = 100 + 37 = 137, cap = 100 + 148 = 248.
        assert_eq!(output.len(), 137);
    }

    #[test]
    fn mock_marker_injector_fails_strict_never_inflate() {
        // The strict gate (byte_gate) rejects inflation — the waivered output is larger.
        let injector = MockMarkerInjector;
        let input = b"x".repeat(100);
        let output = injector
            .apply_reorder(&input, "req-1")
            .expect("must produce output");
        // output.len() (137) > input.len() (100) → strict gate would reject.
        assert!(
            output.len() > input.len(),
            "mock injector must produce larger output than strict gate allows"
        );
    }

    #[test]
    fn mock_same_slot_shrinker_passes_shrink_rule() {
        let shrinker = MockSameSlotShrinker;
        let input = b"hello world";
        let output = shrinker
            .apply_shrink(input, 0, "req-1")
            .expect("must produce output");
        assert!(shrinker.verify_shrink_rule(input.len(), output.len()));
        assert_eq!(output.len(), input.len() - 1);
    }

    #[test]
    fn mock_same_slot_shrinker_empty_input_returns_none() {
        let shrinker = MockSameSlotShrinker;
        let result = shrinker.apply_shrink(b"", 0, "req-1");
        assert!(result.is_none());
    }

    #[test]
    fn mock_same_slot_shrinker_fails_strict_identity() {
        // The reference (identity) contract requires output == input for passthrough.
        // The shrinker produces output != input → fails strict identity check.
        let shrinker = MockSameSlotShrinker;
        let input = b"hello world";
        let outcome = shrinker.transform(input, "req-1");
        // Not passthrough (modification) and output is shorter.
        assert!(!outcome.is_passthrough());
        assert!(outcome.bytes.len() < input.len());
    }

    #[test]
    fn marker_bytes_constant_is_correct() {
        // The MARKER_BYTES constant is the byte length of the JSON marker string.
        let marker = b",\"cache_control\":{\"type\":\"ephemeral\"}";
        assert_eq!(marker.len(), MARKER_BYTES);
    }
}
