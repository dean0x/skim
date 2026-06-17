//! Composite RRF weight container for all search signals (#200).
//!
//! # Weight semantics
//!
//! Each field controls the relative contribution of one signal ranked list to
//! the fused RRF score.  The RRF formula is scale-free — only the RANK of a
//! file in each signal list drives the output — so the absolute magnitudes of
//! the per-signal weights are arbitrary; only their RATIOS matter.
//!
//! # Default profile
//!
//! `Default::default()` returns the starting profile below.  Extended-signal
//! weights default to `0.0` because no corpus-grounded lift measurement has
//! been performed yet (applies ADR-003 / avoids PF-005).  They will be
//! promoted to a non-zero value once a relative-lift benchmark (composite
//! macro-F1 >= jaccard-only baseline F1) confirms positive marginal lift on
//! the same corpus in the same run.
//!
//! | Signal            | Default weight |
//! |-------------------|---------------|
//! | lexical (BM25F)   | 0.5           |
//! | AST structural    | 0.3           |
//! | temporal co-change| 0.2           |
//! | import graph      | 0.0 (gated)   |
//! | dir proximity     | 0.0 (gated)   |
//! | structural coupling| 0.0 (gated)  |
//!
//! # Invariants
//!
//! - All weights must be finite and non-negative (`>= 0.0`).
//! - Negative weights, `NaN`, and `±inf` are rejected by [`CompositeWeights6::validate`].
//! - Zero weight is allowed — the signal contributes nothing to the RRF sum.

use crate::types::{Result, SearchError};

// ============================================================================
// Per-signal weight constants
// ============================================================================

/// Default weight for the lexical (BM25F) signal.
pub const WEIGHT6_LEXICAL: f64 = 0.5;

/// Default weight for the AST structural signal.
pub const WEIGHT6_AST: f64 = 0.3;

/// Default weight for the temporal co-change (Jaccard) signal.
pub const WEIGHT6_TEMPORAL: f64 = 0.2;

/// Default weight for the import-graph signal (gated at 0.0 until measured).
///
/// Set to a non-zero value once a relative-lift benchmark confirms positive
/// marginal lift (applies ADR-003, avoids PF-005).
pub const WEIGHT6_IMPORT_GRAPH: f64 = 0.0;

/// Default weight for the directory-proximity signal (gated at 0.0 until measured).
pub const WEIGHT6_DIR_PROXIMITY: f64 = 0.0;

/// Default weight for the structural-coupling signal (gated at 0.0 until measured).
pub const WEIGHT6_STRUCTURAL_COUPLING: f64 = 0.0;

// ============================================================================
// Weight container
// ============================================================================

/// Per-signal fusion weights for N-signal weighted RRF (#200).
///
/// #198 defines the two-signal (lexical + AST) blend in
/// `compound::intersection::CompositeWeights`.  This struct is the canonical
/// extension to N signals.  The `merge_layer_scores` function in
/// `compound::merge` uses these weights to fuse up to 6 ranked lists via:
///
/// ```text
/// score(d) = Σᵢ wᵢ / (RRF_K + rankᵢ(d))
/// ```
///
/// where `rankᵢ(d)` is d's 1-based position in layer i's DESC-sorted list.
/// A layer in which d is absent contributes 0 (graceful absence — this is
/// what enables UNION-mode co-change-only files to score positively).
///
/// # Example
///
/// ```
/// # use rskim_search::compound::CompositeWeights6;
/// let w = CompositeWeights6::default();
/// assert!(w.validate().is_ok());
/// assert_eq!(w.lexical, 0.5);
/// assert_eq!(w.temporal, 0.2);
/// assert_eq!(w.import_graph, 0.0); // gated until measured
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompositeWeights6 {
    /// Weight for the lexical (BM25F) ranked list.
    pub lexical: f64,
    /// Weight for the AST structural ranked list.
    pub ast: f64,
    /// Weight for the temporal co-change Jaccard ranked list.
    pub temporal: f64,
    /// Weight for the import-graph signal (default 0.0 — ADR-003 gated).
    pub import_graph: f64,
    /// Weight for the directory-proximity signal (default 0.0 — ADR-003 gated).
    pub dir_proximity: f64,
    /// Weight for the structural-coupling signal (default 0.0 — ADR-003 gated).
    pub structural_coupling: f64,
}

impl Default for CompositeWeights6 {
    fn default() -> Self {
        Self {
            lexical: WEIGHT6_LEXICAL,
            ast: WEIGHT6_AST,
            temporal: WEIGHT6_TEMPORAL,
            import_graph: WEIGHT6_IMPORT_GRAPH,
            dir_proximity: WEIGHT6_DIR_PROXIMITY,
            structural_coupling: WEIGHT6_STRUCTURAL_COUPLING,
        }
    }
}

impl CompositeWeights6 {
    /// Validate that all weights are finite and non-negative.
    ///
    /// Returns `Ok(())` when all six weights satisfy:
    /// - Not NaN (`w.is_nan()` is false)
    /// - Not infinite (`w.is_infinite()` is false)
    /// - Non-negative (`w >= 0.0`)
    ///
    /// Returns `Err(SearchError::InvalidQuery(...))` for the first invalid
    /// weight encountered.  This function never panics (engineering rule:
    /// Result, never throw in business logic).
    ///
    /// # Example
    ///
    /// ```
    /// # use rskim_search::compound::CompositeWeights6;
    /// assert!(CompositeWeights6::default().validate().is_ok());
    ///
    /// let bad = CompositeWeights6 { lexical: -0.5, ..Default::default() };
    /// assert!(bad.validate().is_err());
    /// ```
    pub fn validate(&self) -> Result<()> {
        let fields = [
            ("lexical", self.lexical),
            ("ast", self.ast),
            ("temporal", self.temporal),
            ("import_graph", self.import_graph),
            ("dir_proximity", self.dir_proximity),
            ("structural_coupling", self.structural_coupling),
        ];
        for (name, w) in fields {
            if w.is_nan() {
                return Err(SearchError::InvalidQuery(format!(
                    "weight '{name}' is NaN — all weights must be finite and non-negative"
                )));
            }
            if w.is_infinite() {
                return Err(SearchError::InvalidQuery(format!(
                    "weight '{name}' is infinite — all weights must be finite and non-negative"
                )));
            }
            if w < 0.0 {
                return Err(SearchError::InvalidQuery(format!(
                    "weight '{name}' is negative ({w}) — all weights must be >= 0.0"
                )));
            }
        }
        Ok(())
    }

    /// Parse a comma-separated weights string `"l,a,t"` into a `CompositeWeights6`.
    ///
    /// Accepts exactly 3 values: lexical, ast, temporal.  Extended-signal weights
    /// (import_graph, dir_proximity, structural_coupling) remain at their defaults
    /// (all 0.0) — they are not user-configurable until benchmark lift is measured
    /// (applies ADR-003).
    ///
    /// Returns `Err` when the string does not contain exactly 3 comma-separated
    /// values, or any value fails to parse as a finite non-negative f64.
    ///
    /// # Example
    ///
    /// ```
    /// # use rskim_search::compound::CompositeWeights6;
    /// let w = CompositeWeights6::parse_weights_flag("0.5,0.3,0.2").unwrap();
    /// assert_eq!(w.lexical, 0.5);
    /// assert_eq!(w.ast, 0.3);
    /// assert_eq!(w.temporal, 0.2);
    /// ```
    pub fn parse_weights_flag(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split(',').collect();
        if parts.len() != 3 {
            return Err(SearchError::InvalidQuery(format!(
                "--weights requires exactly 3 comma-separated values (lexical,ast,temporal), got: {s:?}"
            )));
        }
        let mut vals = [0.0f64; 3];
        for (i, part) in parts.iter().enumerate() {
            let v: f64 = part.trim().parse().map_err(|_| {
                SearchError::InvalidQuery(format!(
                    "--weights value {part:?} is not a valid number (field {})",
                    ["lexical", "ast", "temporal"][i]
                ))
            })?;
            vals[i] = v;
        }
        let candidate = Self {
            lexical: vals[0],
            ast: vals[1],
            temporal: vals[2],
            import_graph: WEIGHT6_IMPORT_GRAPH,
            dir_proximity: WEIGHT6_DIR_PROXIMITY,
            structural_coupling: WEIGHT6_STRUCTURAL_COUPLING,
        };
        candidate.validate()?;
        Ok(candidate)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "weights_tests.rs"]
mod tests;
