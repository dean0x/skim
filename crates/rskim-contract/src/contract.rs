//! Core contract trait and `Outcome` success type.
//!
//! # Design: fail-open as a success variant
//!
//! The transform path has **no error variant**. Every code path — including
//! parse failure, depth-exceeded nesting, sink-full, and budget overflow —
//! terminates with `Outcome { bytes, decision }`. When modification is not
//! possible, `bytes` equals the original input bytes and `decision` is a
//! passthrough record. This is the L3 enforcement of the "fail-open" doctrine:
//! a proxy that silently loses messages is worse than one that passes them
//! through unmodified.
//!
//! ```rust
//! use rskim_contract::contract::{Contract, Outcome};
//! use rskim_contract::log::{DecisionRecord, DecisionSink, SinkFull};
//!
//! struct Identity;
//!
//! impl Contract for Identity {
//!     fn component_name(&self) -> &'static str { "identity" }
//!
//!     fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
//!         Outcome::passthrough(input.to_vec(), request_id, self.component_name())
//!     }
//! }
//!
//! let identity = Identity;
//! let body = b"{\"model\":\"claude-3-5-sonnet-20241022\",\"messages\":[]}";
//! let outcome = identity.transform(body, "req-001");
//! assert_eq!(outcome.bytes, body);
//! ```
//!
//! # No error variant on `transform`
//!
//! `Result` appears only at construction boundaries (e.g., parsing a config) and
//! in harness assertion APIs (where a deliberately broken impl is the expected
//! failure case). It does NOT appear on `Contract::transform`.
//!
//! # Default-deny capability surface
//!
//! The `Contract` trait exposes no API capable of:
//! - Returning an error in place of passthrough
//! - Growing output bytes (invariant 2)
//! - Deleting, inserting, or reordering turns (invariant 4)
//! - Mutating any byte outside the live zone (invariant 3)
//!
//! These capabilities are available only through the typed waiver traits in
//! [`crate::waiver`], which encode the narrowed rules directly in their
//! method signatures.

use crate::log::DecisionRecord;

/// The outcome of an L3 contract transform.
///
/// `Outcome` has **no error variant** — the transform path is infallible.
/// When modification is not possible (parse failure, invariant violation,
/// sink-full, budget overflow), `bytes` equals the original input and
/// `decision` is a passthrough record.
///
/// # Invariants
///
/// The harness verifies:
/// - `bytes.len() <= input.len()` for every non-waivered transform (invariant 2)
/// - `bytes == input` for every passthrough (invariant 1 fail-open)
/// - `decision` is always present (invariant 8 logged-never-silent)
#[derive(Debug, Clone)]
#[must_use]
pub struct Outcome {
    /// Output bytes. Equal to the original input bytes for passthrough outcomes.
    pub bytes: Vec<u8>,
    /// The decision record for this transform unit.
    pub decision: DecisionRecord,
}

impl Outcome {
    /// Construct a passthrough outcome: output bytes equal input bytes.
    ///
    /// Use this when the transform cannot or should not modify the input.
    /// This is the correct fail-open response to any error condition.
    ///
    /// # Arguments
    ///
    /// - `input` — the original input bytes, returned unchanged
    /// - `request_id` — caller-assigned request identifier (never generated here)
    /// - `component` — name of the component producing this outcome
    pub fn passthrough(input: Vec<u8>, request_id: &str, component: &'static str) -> Self {
        let bytes_in = input.len();
        Self {
            bytes: input,
            decision: DecisionRecord::passthrough(request_id, component, bytes_in),
        }
    }

    /// Construct a modified outcome: output bytes differ from input bytes.
    ///
    /// The caller is responsible for ensuring `output.len() <= input_len`
    /// (invariant 2 never-inflate). The harness enforces this.
    ///
    /// # Arguments
    ///
    /// - `output` — the modified bytes
    /// - `input_len` — original input byte count, recorded in the decision log
    /// - `request_id` — caller-assigned request identifier
    /// - `component` — name of the component producing this outcome
    pub fn modified(
        output: Vec<u8>,
        input_len: usize,
        request_id: &str,
        component: &'static str,
    ) -> Self {
        let bytes_out = output.len();
        Self {
            bytes: output,
            decision: DecisionRecord::modified(request_id, component, input_len, bytes_out),
        }
    }

    /// Returns `true` if this is a passthrough outcome (output bytes equal input bytes).
    pub fn is_passthrough(&self) -> bool {
        self.decision.is_passthrough()
    }
}

/// The core contract trait every L3 component must implement.
///
/// # Design constraints (compile-time enforced)
///
/// The `transform` method signature makes illegal states unrepresentable:
///
/// - **No error variant**: returns `Outcome`, not `Result<Outcome, _>`
/// - **No byte-grow surface**: the API does not accept "extra bytes to inject"
/// - **No turn-delete/reorder surface**: the API does not accept turn indices
/// - **No hot-zone mutation surface**: the API does not accept zone offsets
///
/// Waivered operations (bounded marker injection, same-slot shrink) are
/// available through the narrowed traits in [`crate::waiver`].
///
/// # Implementing Contract
///
/// ```rust
/// use rskim_contract::contract::{Contract, Outcome};
///
/// struct MyTransform;
///
/// impl Contract for MyTransform {
///     fn component_name(&self) -> &'static str {
///         "my-transform"
///     }
///
///     fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
///         // Parse, analyse, and attempt modification.
///         // On ANY error condition: return passthrough.
///         Outcome::passthrough(input.to_vec(), request_id, self.component_name())
///     }
/// }
/// ```
///
/// # Harness registration
///
/// Implementors should register with the conformance harness behind the
/// `harness` feature. See [`crate::harness`] for details and
/// `run_conformance_suite` for the one-call registration API.
pub trait Contract: Send + Sync {
    /// Returns the stable component name for decision records and harness registration.
    ///
    /// Names must be lowercase, hyphen-separated, and unique across components
    /// registered in the same harness run (e.g., `"metadata-reorder"`,
    /// `"content-shrink"`).
    fn component_name(&self) -> &'static str;

    /// Transform the request body, returning an `Outcome` with no error variant.
    ///
    /// # Fail-open contract
    ///
    /// Every call MUST return an `Outcome`. On any error condition (parse
    /// failure, invariant violation, etc.) the implementation MUST return
    /// `Outcome::passthrough(input.to_vec(), request_id, self.component_name())`.
    ///
    /// # Arguments
    ///
    /// - `input` — raw request body bytes (UTF-8 JSON, but may be malformed)
    /// - `request_id` — caller-assigned request identifier; never generated here
    ///   (invariant 5: no entropy in the transform path)
    fn transform(&self, input: &[u8], request_id: &str) -> Outcome;
}

/// Identity/passthrough reference implementation.
///
/// Returns every input unchanged. Passes the full conformance suite because
/// it satisfies all invariants trivially (passthrough is always safe):
/// - Never inflates (bytes_out == bytes_in, invariant 2)
/// - Hot zone is byte-identical (unmodified, invariant 3)
/// - Turn count unchanged (no modification, invariant 4)
/// - Deterministic (no side effects, invariant 5)
/// - One passthrough record per call (invariant 8)
///
/// Used as the harness self-test reference implementation (AC18).
#[derive(Debug, Clone, Copy)]
pub struct IdentityContract;

impl Contract for IdentityContract {
    fn component_name(&self) -> &'static str {
        "identity"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        Outcome::passthrough(input.to_vec(), request_id, self.component_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_passthrough_bytes_equal_input() {
        let input = b"hello world";
        let outcome = Outcome::passthrough(input.to_vec(), "req-1", "test");
        assert_eq!(outcome.bytes, input);
        assert!(outcome.is_passthrough());
    }

    #[test]
    fn outcome_modified_records_byte_counts() {
        let outcome = Outcome::modified(b"short".to_vec(), 100, "req-2", "test");
        assert_eq!(outcome.bytes, b"short");
        assert!(!outcome.is_passthrough());
        assert_eq!(outcome.decision.bytes_in, 100);
        assert_eq!(outcome.decision.bytes_out, 5);
    }

    #[test]
    fn identity_contract_returns_passthrough() {
        let c = IdentityContract;
        let input = b"{\"model\":\"claude-3-5-sonnet-20241022\",\"messages\":[]}";
        let outcome = c.transform(input, "req-3");
        assert_eq!(outcome.bytes, input);
        assert!(outcome.is_passthrough());
        assert_eq!(outcome.decision.component, "identity");
    }

    #[test]
    fn identity_contract_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<IdentityContract>();
    }
}
