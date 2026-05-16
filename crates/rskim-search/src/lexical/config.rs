//! BM25F scoring configuration with per-field boosts and length normalisation.
//!
//! # Field ordering
//!
//! The eight fields correspond to [`crate::SearchField`] discriminants 0–7 in
//! order:
//!
//! | Index | Field            | Default boost |
//! |-------|-----------------|---------------|
//! | 0     | TypeDefinition  | 5.0           |
//! | 1     | FunctionSignature | 4.0         |
//! | 2     | SymbolName      | 3.5           |
//! | 3     | ImportExport    | 3.0           |
//! | 4     | FunctionBody    | 1.0           |
//! | 5     | Comment         | 0.8           |
//! | 6     | StringLiteral   | 0.5           |
//! | 7     | Other           | 1.0           |

use serde::{Deserialize, Serialize};

use crate::{Result, SearchError};

/// Number of searchable fields in the BM25F scoring model.
///
/// Derived from [`crate::SearchField::count()`] so it stays in sync
/// automatically when variants are added or removed.
pub const FIELD_COUNT: usize = crate::SearchField::count();

/// Compile-time assertion: `FIELD_COUNT` must equal `SearchField::ALL.len()`.
///
/// This fires if `count()` and `ALL` diverge (e.g. a variant is added to one
/// but not the other), catching the inconsistency at build time.
const _: () = assert!(
    FIELD_COUNT == crate::SearchField::ALL.len(),
    "FIELD_COUNT must equal SearchField::ALL.len()"
);

/// BM25F scoring parameters controlling relevance ranking.
///
/// # Invariants
///
/// - `k1` must be ≥ 0.0.
/// - Every `field_boosts[i]` must be ≥ 0.0.
/// - Every `field_b[i]` must be in \[0.0, 1.0\].
///
/// These are enforced by [`BM25FConfig::validate`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BM25FConfig {
    /// Term-frequency saturation parameter. Typical value: 1.2.
    pub k1: f32,
    /// Per-field relevance boost weights.  Higher boost → field terms matter more.
    pub field_boosts: [f32; FIELD_COUNT],
    /// Per-field Okapi length-normalisation parameters.  0.0 = no normalisation;
    /// 1.0 = full normalisation.  Typical value: 0.75.
    pub field_b: [f32; FIELD_COUNT],
}

impl BM25FConfig {
    /// Validate that all parameters are within legal ranges.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::InvalidQuery`] if any invariant is violated.
    pub fn validate(&self) -> Result<()> {
        if !self.k1.is_finite() || self.k1 < 0.0 {
            return Err(SearchError::InvalidQuery(format!(
                "BM25FConfig: k1 must be a finite number >= 0.0, got {}",
                self.k1
            )));
        }
        for (i, &boost) in self.field_boosts.iter().enumerate() {
            if !boost.is_finite() || boost < 0.0 {
                return Err(SearchError::InvalidQuery(format!(
                    "BM25FConfig: field_boosts[{i}] must be a finite number >= 0.0, got {boost}"
                )));
            }
        }
        for (i, &b) in self.field_b.iter().enumerate() {
            if !b.is_finite() || !(0.0..=1.0).contains(&b) {
                return Err(SearchError::InvalidQuery(format!(
                    "BM25FConfig: field_b[{i}] must be a finite number in [0.0, 1.0], got {b}"
                )));
            }
        }
        Ok(())
    }
}

impl Default for BM25FConfig {
    /// Sensible defaults that boost structural fields above implementation fields.
    ///
    /// Boosts: TypeDef=5.0, FnSig=4.0, Symbol=3.5, Import=3.0,
    ///         FnBody=1.0, Comment=0.8, StringLit=0.5, Other=1.0
    fn default() -> Self {
        Self {
            k1: 1.2,
            field_boosts: [5.0, 4.0, 3.5, 3.0, 1.0, 0.8, 0.5, 1.0],
            field_b: [0.75; FIELD_COUNT],
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
