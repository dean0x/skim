//! Composite RRF weight container for all search signals (#200).
//!
//! # Design
//!
//! `CompositeWeights` (defined in `compound::intersection`) is the **canonical**
//! single weight container shared by #198 and #200.  #200 extended it additively
//! with four new fields (`temporal`, `import_graph`, `dir_proximity`,
//! `structural_coupling`) so no second representation is needed.
//!
//! `CompositeWeights6` is a **type alias** for `CompositeWeights` — it exists
//! for backward source compatibility and documents the six-signal profile.
//! New code should prefer `CompositeWeights` directly.
//!
//! # Weight semantics
//!
//! Each field controls the relative contribution of one signal ranked list to
//! the fused RRF score.  The RRF formula is scale-free — only the RANK of a
//! file in each signal list drives the output — so the absolute magnitudes of
//! the per-signal weights are arbitrary; only their RATIOS matter.
//!
//! # Default profile (six-signal, #200)
//!
//! `CompositeWeights::with_six_signal_defaults()` returns the #200 starting
//! profile.  Extended-signal weights default to `0.0` because no
//! corpus-grounded lift measurement has been performed yet (applies ADR-003).
//! They will be promoted to a non-zero value once a relative-lift benchmark
//! (composite macro-F1 >= jaccard-only baseline F1) confirms positive marginal
//! lift on the same corpus in the same run.
//!
//! | Signal             | Default weight |
//! |--------------------|---------------|
//! | lexical (BM25F)    | 0.5           |
//! | AST structural     | 0.3           |
//! | temporal co-change | 0.2           |
//! | import graph       | 0.0 (gated)   |
//! | dir proximity      | 0.0 (gated)   |
//! | structural coupling| 0.0 (gated)   |
//!
//! # Invariants
//!
//! - All weights must be finite and non-negative (`>= 0.0`).
//! - Negative weights, `NaN`, and `±inf` are rejected by
//!   [`CompositeWeights::validate`].
//! - Zero weight is allowed — the signal contributes nothing to the RRF sum.

pub use crate::compound::intersection::CompositeWeights;

// ============================================================================
// Per-signal weight constants (six-signal #200 profile)
// ============================================================================

/// Default weight for the lexical (BM25F) signal (six-signal profile).
pub const WEIGHT6_LEXICAL: f64 = 0.5;

/// Default weight for the AST structural signal (six-signal profile).
pub const WEIGHT6_AST: f64 = 0.3;

/// Default weight for the temporal co-change (Jaccard) signal.
pub const WEIGHT6_TEMPORAL: f64 = 0.2;

/// Default weight for the import-graph signal (gated at 0.0 until measured).
///
/// Set to a non-zero value once a relative-lift benchmark confirms positive
/// marginal lift (applies ADR-003).
pub const WEIGHT6_IMPORT_GRAPH: f64 = 0.0;

/// Default weight for the directory-proximity signal (gated at 0.0 until measured).
pub const WEIGHT6_DIR_PROXIMITY: f64 = 0.0;

/// Default weight for the structural-coupling signal (gated at 0.0 until measured).
pub const WEIGHT6_STRUCTURAL_COUPLING: f64 = 0.0;

// ============================================================================
// Type alias — single canonical representation
// ============================================================================

/// Six-signal weight container — type alias for [`CompositeWeights`].
///
/// `CompositeWeights` is the **canonical** container: #200 extended it
/// additively with `temporal`, `import_graph`, `dir_proximity`, and
/// `structural_coupling` fields.  This alias exists for backward source
/// compatibility and documents the six-signal profile explicitly.
///
/// # Six-signal default
///
/// Use `CompositeWeights::with_six_signal_defaults()` to obtain the six-signal
/// #200 profile (`lexical=0.5, ast=0.3, temporal=0.2, extended=0.0`).
/// `CompositeWeights::default()` returns the two-signal #198 profile
/// (`lexical=1.0, ast=1.0, extended=0.0`).
///
/// Validation and CLI parsing are available as inherent methods on
/// `CompositeWeights`:
/// - `CompositeWeights::validate(&self)` — rejects NaN/inf/negative weights.
/// - `CompositeWeights::parse_weights_flag(s)` — parses `"l,a,t"` flag strings.
///
/// # Example
///
/// ```
/// # use rskim_search::compound::CompositeWeights6;
/// let w = CompositeWeights6::with_six_signal_defaults();
/// assert!(w.validate().is_ok());
/// assert_eq!(w.lexical, 0.5);
/// assert_eq!(w.temporal, 0.2);
/// assert_eq!(w.import_graph, 0.0); // gated until measured
/// ```
pub type CompositeWeights6 = CompositeWeights;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "weights_tests.rs"]
mod tests;
