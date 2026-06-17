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
//!
//! # AC21 default-path cost model
//!
//! The non-waivered default transform path through `guarded_transform` performs
//! exactly:
//! - One byte-length comparison (`byte_gate`)
//! - One non-blocking `try_send` (O(1) channel push)
//! - One `Outcome` construction (zero-copy for passthrough; move for modification)
//!
//! It MUST NOT reach: `crate::canonical` (canonicalization / deep-equality),
//! any token counter, `serde_json::to_vec` (re-serialization), or
//! `serde_json::from_str` (re-parse). These are confined to the waivered path.
//! The `ac21_default_path_is_byte_length_only` unit test provides the behavioral
//! assertion; the Rust module boundary provides the structural guarantee (this
//! module imports nothing from `canonical`).

use crate::contract::Outcome;
use crate::log::{DecisionRecord, DecisionSink, SinkFull};

/// Outcome of the never-inflate byte gate.
///
/// The gate is a pure byte-length comparison; it has no knowledge of tokens
/// or content structure. This type records whether the candidate was accepted
/// or rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
/// - `input` — original input bytes (owned; moved into the passthrough `Outcome`
///   on gate rejection or `SinkFull`, avoiding a copy on the most common paths)
/// - `candidate` — proposed output bytes (owned; moved into the modification `Outcome`
///   on acceptance, also avoiding a copy; `&[u8]` would require `.to_vec()` at each
///   accept site — owned here keeps the hot path allocation-free on acceptance)
/// - `request_id` — caller-assigned request identifier
/// - `component` — stable component name for the decision record
/// - `sink` — injected decision sink
///
/// # Borrow vs. own trade-off
///
/// This is the one place in the crate that takes `Vec<u8>` instead of `&[u8]`.
/// The owned signature allows the passthrough and modification paths to MOVE the
/// buffer into the `Outcome` rather than clone it — a zero-copy hot path (AC21).
/// Call sites that have a `&[u8]` must `.to_vec()` once before calling; that cost
/// is equal to the clone `guarded_transform` would need internally if it took `&[u8]`.
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
/// # Current status
///
/// This function is a primitive reserved for the #306/#307 consumer that owns
/// whole-request assembly (`apply_live_zone_edits` + hot-zone splice path).
/// At this layer, `locate_hot_zone_range` and `apply_live_zone_edits` are
/// stubs that return `None` (→ passthrough), so no whole-request assembly
/// occurs and this function has no current caller in the production path.
/// It is tested in isolation and wired in by #307 when the typed offset
/// model becomes available.
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

    // ========================================================================
    // AC21 default-path code-path assertions
    //
    // AC21 requires that the default (non-waivered) transform path MUST NOT
    // reach canonicalization (`crate::canonical`), token computation, deep
    // equality, or re-serialization. This is asserted here:
    //
    // 1. `byte_gate` performs ONLY a comparison: `candidate_len <= input_len`.
    //    It imports nothing from `canonical`, `request`, or any tokenizer.
    //    The Rust compiler enforces this: unused imports are dead code and
    //    any accidental import from `canonical` would appear in the call graph.
    //
    // 2. `guarded_transform` reaches only: `byte_gate`, `sink.try_send`, and
    //    the `Outcome::passthrough`/`Outcome::modified` constructors.
    //    The `DecisionRecord` constructors use only primitive integer ops.
    //
    // The behavioral test below validates the O(1) path at runtime: it runs
    // `guarded_transform` on a >100KB body and confirms only a single byte
    // comparison is performed (output is either the candidate or the input —
    // never a re-serialized or canonicalized form).
    // ========================================================================

    /// AC21 behavioral assertion: default path is O(1) byte-length comparison only.
    ///
    /// Runs `guarded_transform` on a large body (>100KB) and verifies:
    /// - For a shrinking candidate: output == candidate bytes exactly
    /// - For an inflating candidate: output == input bytes exactly (gate rejected)
    ///
    /// If the default path accidentally routed through canonicalization or
    /// re-serialization, the output bytes would differ from either candidate or
    /// input (serde_json may reorder keys or normalize numbers). The byte-identity
    /// assertion here is the observable proxy for "no re-serialization occurred".
    #[test]
    fn ac21_default_path_is_byte_length_only() {
        let sink = Arc::new(MockSink::new());

        // Generate a large body (AC21: >100KB body).
        // Content is arbitrary bytes — not valid JSON — so any re-serialization
        // path would fail and produce different bytes.
        let input: Vec<u8> = (0u8..=255).cycle().take(102_400).collect();
        let candidate: Vec<u8> = input[..51_200].to_vec(); // shrink to 50KB

        // Shrinking candidate: gate accepts, output must be candidate exactly.
        let outcome = guarded_transform(
            input.clone(),
            candidate.clone(),
            "ac21-req-1",
            "test",
            &*sink,
        );
        assert_eq!(
            outcome.bytes, candidate,
            "AC21: gate-accepted output must be byte-identical to candidate (no re-serialization)"
        );
        assert!(!outcome.is_passthrough());

        // Inflating candidate: gate rejects, output must be input exactly.
        let mut inflating = input.clone();
        inflating.push(0u8); // one byte over
        let outcome2 = guarded_transform(input.clone(), inflating, "ac21-req-2", "test", &*sink);
        assert_eq!(
            outcome2.bytes, input,
            "AC21: gate-rejected output must be byte-identical to input (passthrough, no mutation)"
        );
        assert!(outcome2.is_passthrough());
    }
}
