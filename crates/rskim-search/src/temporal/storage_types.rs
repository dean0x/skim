//! Row types for the temporal SQLite persistence layer.
//!
//! These types are shared between [`super::storage`] (the database wrapper)
//! and [`super::storage_ops`] (the store/load/sync implementations).

// ============================================================================
// Row types
// ============================================================================

/// A row from the `hotspot` table.
#[derive(Debug, Clone, PartialEq)]
pub struct HotspotRow {
    /// Repository-root-relative file path.
    pub file_path: String,
    /// Decay-weighted commit frequency, max-normalized to `[0.0, 1.0]`.
    pub score: f64,
    /// Raw commit count within the last 30 days.
    pub changes_30d: i64,
    /// Raw commit count within the last 90 days.
    pub changes_90d: i64,
}

/// A row from the `risk` table.
#[derive(Debug, Clone, PartialEq)]
pub struct RiskRow {
    /// Repository-root-relative file path.
    pub file_path: String,
    /// Bug-fix density score in `[0.0, 1.0]`.
    pub risk_score: f64,
    /// Total number of commits touching this file.
    pub total_commits: i64,
    /// Number of commits classified as fix commits.
    pub fix_commits: i64,
    /// Ratio of fix commits to total commits, in `[0.0, 1.0]`.
    pub fix_density: f64,
}

/// A row from the `cochange` table.
#[derive(Debug, Clone, PartialEq)]
pub struct CochangeRow {
    /// Repository-root-relative path of the first file in the pair (lexically smaller).
    pub file_a: String,
    /// Repository-root-relative path of the second file in the pair.
    pub file_b: String,
    /// Number of commits that touched both files.
    pub count: i64,
    /// Jaccard similarity of the two files' commit sets.
    pub jaccard: f64,
}
