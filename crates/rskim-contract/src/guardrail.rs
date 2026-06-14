//! Never-inflate guardrail: per-transform-unit byte-length gate.
//!
//! # L3 vs L2 guardrail distinction
//!
//! This guardrail is deliberately different from the L2 guardrail in
//! `crates/rskim/src/output/guardrail.rs`:
//!
//! - **No tokenizer** in the accept/reject path (invariant 2 / AC5).
//!   The L2 guardrail has a token slow-path for the case where bytes grow
//!   but tokens shrink; L3 has no such path.
//! - **No tiny-payload exemption** (invariant 2 / AC4).
//!   The L2 guardrail skips files < 256 bytes; L3 applies to all sizes.
//! - **Per-transform-unit** (one content block in one message), with a
//!   whole-request defense-in-depth check.
//!
//! Migration of L2 to share this implementation is tracked in #325.
//!
//! # Sink-failure rule (invariant 8)
//!
//! If [`crate::log::DecisionSink::try_send`] returns `SinkFull`, the transform
//! unit MUST emit byte-faithful passthrough instead of the (uninstrumented)
//! modification. The decision gate and sink dispatch are separated here so that
//! the sink-failure rule is the only ordering dependency.
//!
//! # Static determinism gate
//!
//! This module must not call `std::time::SystemTime::now`, `Instant::now`, or
//! any `rand`/`getrandom` entry point. Enforcement is via the clippy
//! `disallowed-methods` configuration in `clippy.toml` at the crate root.

use crate::contract::Outcome;
use crate::log::{DecisionRecord, DecisionSink, SinkFull};

/// Outcome of the never-inflate byte gate.
///
/// The gate is a pure byte-length comparison; it has no knowledge of tokens
/// or content structure. This type records whether the candidate was accepted
/// or rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteGateVerdict {
    /// Candidate bytes are ≤ input bytes. The modification is permitted
    /// (subject to the sink check).
    Accepted,
    /// Candidate bytes are > input bytes. The modification is rejected;
    /// caller should fall back to passthrough.
    Rejected,
}

/// Apply the never-inflate byte gate to a candidate modification.
///
/// Returns [`ByteGateVerdict::Accepted`] iff `candidate_len <= input_len`.
/// No tokenizer is consulted; the gate is a single comparison.
///
/// # Arguments
///
/// - `input_len` — byte length of the original input
/// - `candidate_len` — byte length of the proposed modification
///
/// # Examples
///
/// ```rust
/// use rskim_contract::guardrail::{byte_gate, ByteGateVerdict};
///
/// assert_eq!(byte_gate(100, 90), ByteGateVerdict::Accepted);
/// assert_eq!(byte_gate(100, 100), ByteGateVerdict::Accepted);
/// assert_eq!(byte_gate(100, 101), ByteGateVerdict::Rejected);
/// assert_eq!(byte_gate(0, 0), ByteGateVerdict::Accepted);
/// ```
#[inline]
pub fn byte_gate(input_len: usize, candidate_len: usize) -> ByteGateVerdict {
    if candidate_len <= input_len {
        ByteGateVerdict::Accepted
    } else {
        ByteGateVerdict::Rejected
    }
}

/// Apply the guarded transform pipeline with sink-failure rule.
///
/// This function enforces the full L3 transform contract:
///
/// 1. Run `byte_gate(input.len(), candidate.len())`.
/// 2. If rejected → return passthrough `Outcome` (no record sent).
/// 3. If accepted → attempt `sink.try_send(modification_record)`.
/// 4. If `try_send` returns `SinkFull` → return passthrough `Outcome`.
/// 5. If `try_send` returns `Ok` → return the modification `Outcome`.
///
/// This is the only function in the codebase that interleaves the gate check
/// and the sink dispatch, making the sink-failure rule observable at one site.
///
/// # Arguments
///
/// - `input` — original input bytes
/// - `candidate` — proposed output bytes (may be the same as input on trivial paths)
/// - `request_id` — caller-assigned request identifier
/// - `component` — stable component name for the decision record
/// - `sink` — injected decision sink
///
/// # Returns
///
/// Always returns an `Outcome` (no error variant). Passthrough occurs when:
/// - `candidate.len() > input.len()` (gate rejected)
/// - `sink.try_send` returns `SinkFull` (sink-failure rule)
pub fn guarded_transform(
    input: Vec<u8>,
    candidate: Vec<u8>,
    request_id: &str,
    component: &'static str,
    sink: &dyn DecisionSink,
) -> Outcome {
    let input_len = input.len();
    let candidate_len = candidate.len();

    // Step 1: byte gate (invariant 2, no tokenizer).
    if byte_gate(input_len, candidate_len) == ByteGateVerdict::Rejected {
        // Gate rejected: emit passthrough (no decision record for rejected attempts).
        return Outcome::passthrough(input, request_id, component);
    }

    // Step 2: attempt to record the decision before emitting the modification.
    // Invariant 8: a modification whose record was not accepted MUST NOT be emitted.
    let record = DecisionRecord::modified(request_id, component, input_len, candidate_len);
    match sink.try_send(record) {
        Ok(()) => {
            // Record accepted; emit the modification.
            Outcome::modified(candidate, input_len, request_id, component)
        }
        Err(SinkFull) => {
            // Sink full: emit passthrough rather than an unlogged modification.
            Outcome::passthrough(input, request_id, component)
        }
    }
}

/// Whole-request defense-in-depth byte check.
///
/// After all per-unit transforms are applied and assembled into the final
/// output buffer, this check verifies the whole-request invariant:
/// `output.len() <= input.len()`.
///
/// If the whole-request check fails (which should not happen if per-unit
/// gates are correct), returns `Err(output_len)` so the caller can fall
/// back to the original input. This is defense-in-depth, not the primary
/// gate.
///
/// # Arguments
///
/// - `input_len` — byte length of the original whole request
/// - `output_len` — byte length of the assembled whole-request output
pub fn whole_request_check(input_len: usize, output_len: usize) -> Result<(), usize> {
    if output_len <= input_len {
        Ok(())
    } else {
        Err(output_len)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::log::MockSink;
    use std::sync::Arc;

    // ========================================================================
    // byte_gate tests
    // ========================================================================

    #[test]
    fn byte_gate_accepts_shrink() {
        assert_eq!(byte_gate(100, 90), ByteGateVerdict::Accepted);
    }

    #[test]
    fn byte_gate_accepts_equal() {
        assert_eq!(byte_gate(100, 100), ByteGateVerdict::Accepted);
    }

    #[test]
    fn byte_gate_rejects_inflate() {
        assert_eq!(byte_gate(100, 101), ByteGateVerdict::Rejected);
    }

    #[test]
    fn byte_gate_empty_input_accepts_empty_candidate() {
        assert_eq!(byte_gate(0, 0), ByteGateVerdict::Accepted);
    }

    #[test]
    fn byte_gate_empty_input_rejects_nonempty_candidate() {
        assert_eq!(byte_gate(0, 1), ByteGateVerdict::Rejected);
    }

    #[test]
    fn byte_gate_no_tokenizer_reachable() {
        // This test documents the dependency assertion (AC5): byte_gate takes
        // no token-counting parameter and has no import of any tokenizer.
        // The compiler enforces this; this test is the observable record.
        let v = byte_gate(50, 30);
        assert_eq!(v, ByteGateVerdict::Accepted);
    }

    // ========================================================================
    // whole_request_check tests
    // ========================================================================

    #[test]
    fn whole_request_check_accepts_shrink() {
        assert!(whole_request_check(1000, 900).is_ok());
    }

    #[test]
    fn whole_request_check_accepts_equal() {
        assert!(whole_request_check(500, 500).is_ok());
    }

    #[test]
    fn whole_request_check_rejects_inflate() {
        let err = whole_request_check(100, 101).expect_err("must fail");
        assert_eq!(err, 101);
    }

    // ========================================================================
    // guarded_transform tests — AC14 (sink-full → passthrough)
    // ========================================================================

    #[test]
    fn guarded_transform_accepted_modification() {
        let sink = Arc::new(MockSink::new());
        let input = b"hello world extra content".to_vec();
        let candidate = b"hello world".to_vec();
        let outcome = guarded_transform(input.clone(), candidate.clone(), "req-1", "test", &*sink);
        assert_eq!(outcome.bytes, candidate);
        assert!(!outcome.is_passthrough());
        assert_eq!(sink.len(), 1);
        let records = sink.drain();
        assert_eq!(records[0].decision, crate::log::Decision::Modified);
        assert_eq!(records[0].bytes_in, input.len());
        assert_eq!(records[0].bytes_out, candidate.len());
    }

    #[test]
    fn guarded_transform_gate_rejected_no_record() {
        let sink = Arc::new(MockSink::new());
        let input = b"short".to_vec();
        let candidate = b"longer than input bytes".to_vec(); // inflate
        let outcome = guarded_transform(input.clone(), candidate, "req-2", "test", &*sink);
        // Passthrough because candidate > input.
        assert_eq!(outcome.bytes, input);
        assert!(outcome.is_passthrough());
        // No record sent (gate rejected before sink dispatch).
        assert!(sink.is_empty());
    }

    #[test]
    fn guarded_transform_sink_full_falls_back_to_passthrough() {
        let sink = Arc::new(MockSink::new());
        sink.set_full(true);
        let input = b"some long input content here".to_vec();
        let candidate = b"shorter".to_vec(); // would pass gate
        let outcome = guarded_transform(input.clone(), candidate, "req-3", "test", &*sink);
        // Sink was full → passthrough (not the modification).
        assert_eq!(outcome.bytes, input, "must fall back to input on SinkFull");
        assert!(outcome.is_passthrough());
        assert!(sink.is_empty(), "no record accepted");
    }

    #[test]
    fn guarded_transform_equal_length_accepted() {
        let sink = Arc::new(MockSink::new());
        let input = b"hello world!".to_vec();
        let candidate = b"hello world?".to_vec(); // same length
        let outcome = guarded_transform(input.clone(), candidate.clone(), "req-4", "test", &*sink);
        assert_eq!(outcome.bytes, candidate);
        assert!(!outcome.is_passthrough());
        assert_eq!(sink.len(), 1);
    }

    #[test]
    fn guarded_transform_multibyte_utf8_no_panic() {
        // Test with multi-byte UTF-8 content to verify no off-by-one on byte counting.
        let sink = Arc::new(MockSink::new());
        let input = "🦀 rust crab emoji uses 4 bytes".as_bytes().to_vec();
        let candidate = "🦀 rust crab".as_bytes().to_vec(); // shorter
        let in_len = input.len();
        let cand_len = candidate.len();
        assert!(cand_len < in_len);
        let outcome = guarded_transform(input, candidate.clone(), "req-5", "test", &*sink);
        assert_eq!(outcome.bytes, candidate);
        assert!(!outcome.is_passthrough());
    }
}
