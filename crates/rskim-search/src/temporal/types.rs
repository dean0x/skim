//! Shared temporal layer types.
//!
//! These types are locked after Phase 0 — subsequent phases (git_parser,
//! cochange, scoring, storage, query) depend on this module's API surface
//! and must not modify it without coordination.

use std::path::PathBuf;

/// Opaque identifier for a file within the temporal layer.
///
/// Distinct from lexical [`FileId`](crate::FileId) to prevent accidental
/// cross-layer mixing at compile time. Temporal stores its own path table
/// because files in git history may not exist in the current lexical index
/// (deleted, renamed, gitignored).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TemporalFileId(u32);

impl TemporalFileId {
    /// Construct a `TemporalFileId` from its raw representation.
    #[must_use]
    pub fn new(id: u32) -> Self {
        Self(id)
    }

    /// Return the raw `u32` value.
    #[must_use]
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

/// Parsed metadata for a single git commit.
///
/// Produced by the git parser (Phase 1), consumed by co-change (Phase 2a)
/// and scoring (Phase 2b). Paths are repo-relative with forward slashes.
#[derive(Debug, Clone)]
pub struct CommitInfo {
    /// Abbreviated or full commit hash.
    pub hash: String,
    /// Commit timestamp as unix epoch seconds (UTC).
    pub timestamp: u64,
    /// Commit message subject (first line only; full message not retained).
    pub message: String,
    /// `true` if the commit message matches the fix-pattern regex.
    pub is_fix: bool,
    /// Files changed in this commit (repo-relative, forward-slash form).
    /// For merge commits, only first-parent changes are included.
    pub changed_files: Vec<PathBuf>,
}

/// Hotspot score for a single file (30/90-day commit activity).
///
/// Produced by `scoring::hotspot_scores` (Phase 2b). Scores are normalized
/// to `[0, 1]` by dividing by the max raw score in the set.
#[derive(Debug, Clone)]
pub struct HotspotScore {
    /// Repo-relative path of the file.
    pub path: PathBuf,
    /// Number of commits touching this file in the last 30 days.
    pub commit_count_30d: u32,
    /// Number of commits touching this file in the last 90 days.
    pub commit_count_90d: u32,
    /// Normalized hotspot score in `[0, 1]`.
    pub score: f32,
}

/// Risk score for a single file (fix-commit density).
///
/// Produced by `scoring::risk_scores` (Phase 2b). Files with fewer than
/// 3 total commits receive a score of `0.0` to avoid 1/1 = 1.0 false
/// positives. Scores are normalized to `[0, 1]`.
#[derive(Debug, Clone)]
pub struct RiskScore {
    /// Repo-relative path of the file.
    pub path: PathBuf,
    /// Total commits touching this file in the lookback window.
    pub total_commits: u32,
    /// Number of those commits classified as fix commits.
    pub fix_commits: u32,
    /// Ratio `fix_commits / total_commits` (0.0 if total_commits < 3).
    pub fix_density: f32,
    /// Normalized risk score in `[0, 1]`.
    pub score: f32,
}

/// Co-change entry: a pair of files that frequently change together.
///
/// Produced by `cochange::build_cochange_matrix` (Phase 2a). Pairs are
/// stored in canonical order (`path_a < path_b` lexicographically) to
/// avoid duplication. Only pairs with `co_occurrences >= 2` are retained.
#[derive(Debug, Clone)]
pub struct CochangeEntry {
    /// First file in the pair (lexicographically smaller).
    pub path_a: PathBuf,
    /// Second file in the pair (lexicographically larger).
    pub path_b: PathBuf,
    /// Number of commits touching both files.
    pub co_occurrences: u32,
    /// Jaccard similarity: `co_occurrences / (commits_a + commits_b - co_occurrences)`.
    /// Range `[0, 1]`.
    pub jaccard: f32,
}
