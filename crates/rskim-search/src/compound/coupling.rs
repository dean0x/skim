//! Structural-coupling signal scaffold (#200, phase deferred to #336).
//!
//! # Status
//!
//! This module is a **compile-ready scaffold** with a neutral (0.0) implementation
//! and a default weight of 0.0 (see [`crate::compound::weights::WEIGHT6_STRUCTURAL_COUPLING`]).
//!
//! The full implementation is deferred to follow-up ticket **#336** because:
//! 1. AC8 explicitly allows phasing ("may be phased separately").
//! 2. No corpus-grounded lift measurement has been performed yet (ADR-003).
//! 3. Extracting shared type references / trait impls requires deeper AST
//!    traversal than the existing import-graph and proximity signals.
//!
//! # AC8 deferral (explicit)
//!
//! AC8(b) applies: the API returns a documented neutral value (0.0) and is
//! gated to default weight 0.0 in `CompositeWeights6`.  Follow-up ticket #336
//! tracks the full implementation with a benchmark-grounded weight.
//!
//! # Future implementation notes (for #336)
//!
//! The structural-coupling score measures how strongly two files are coupled
//! via shared type references or trait implementations:
//! - Two files that both `impl Foo` → coupled.
//! - Two files that share a common generic parameter type → coupled.
//! - Score range: `[0.0, 1.0]` (Jaccard of shared type-reference sets).
//!
//! The implementation will reuse AST structural metrics from `AstIndexReader`
//! (already available in Wave 4a) to extract type-reference sets per file.

use crate::types::FileId;

// ============================================================================
// Public API (scaffold)
// ============================================================================

/// Structural coupling score between two files.
///
/// # Current behaviour (scaffold)
///
/// Always returns `0.0`.  This is intentional: the signal is gated to
/// `weight = 0.0` in [`crate::compound::weights::CompositeWeights6`] until
/// a benchmark-grounded promotion is performed in ticket #336.
///
/// # Future behaviour (post-#336)
///
/// Will return a `[0.0, 1.0]` Jaccard score measuring the overlap of the
/// two files' shared type-reference / trait-implementation sets.  The
/// implementation in #336 must use `total_cmp` for any f64 comparisons and
/// widen all structural metric counters via `u32::from` / `f64::from` before
/// arithmetic (PF-004).
///
/// # Arguments
///
/// * `_source` — The source `FileId`.
/// * `_target` — The target `FileId`.
///
/// # Returns
///
/// `0.0` (neutral score — no coupling assumed in the scaffold phase).
#[must_use]
pub fn structural_coupling_score(_source: FileId, _target: FileId) -> f64 {
    // Scaffold: always 0.0 until #336 implements the full signal.
    // See module-level doc comment for the deferral rationale.
    0.0
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "coupling_tests.rs"]
mod tests;
