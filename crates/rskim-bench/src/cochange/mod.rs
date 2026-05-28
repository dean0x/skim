//! Co-change validation benchmark.
//!
//! Measures the precision/recall of blast-radius predictions against actual
//! PR file sets from OSS repositories, establishing baseline metrics for
//! Jaccard threshold tuning.
//!
//! # Modules
//!
//! - [`types`]          — shared result types
//! - [`deny_list`]      — lock-file and generated-file exclusions
//! - [`temporal_split`] — chronological train/test split
//! - [`validate`]       — core evaluation pipeline
//! - [`report`]         — JSON and Markdown output

pub mod deny_list;
pub mod report;
pub mod temporal_split;
pub mod types;
pub mod validate;

// ============================================================================
// Shared test utilities
// ============================================================================

/// Shared helpers used by unit and integration tests in the `cochange` module.
///
/// Always compiled (not gated by `#[cfg(test)]`) so that integration tests in
/// `tests/` — which compile against the library as a separate crate — can
/// import from here.  The module is deliberately not re-exported from the crate
/// root so callers must opt-in explicitly.
///
/// Both the per-module unit tests (`validate.rs`, `temporal_split.rs`) and the
/// integration tests (`tests/cochange_validation.rs`) import from here to
/// avoid duplicating the `make_commit` helper with slightly differing signatures.
pub mod test_utils {
    use std::path::PathBuf;

    use rskim_search::{CommitInfo, FileChangeInfo};

    /// Build a [`CommitInfo`] suitable for tests.
    ///
    /// - `id`        — used as both the commit hash (zero-padded hex) and log message
    /// - `timestamp` — Unix timestamp (seconds)
    /// - `paths`     — repo-relative file paths changed in this commit
    pub fn make_commit(id: usize, timestamp: i64, paths: &[&str]) -> CommitInfo {
        CommitInfo {
            hash: format!("{id:040x}"),
            timestamp,
            author: "test".to_string(),
            message: format!("commit {id}"),
            changed_files: paths
                .iter()
                .map(|p| FileChangeInfo {
                    path: PathBuf::from(p),
                    additions: 1,
                    deletions: 0,
                })
                .collect(),
        }
    }

    /// Build `count` commits in newest-first order (as [`GixSource`] returns).
    ///
    /// Timestamps run from `count` down to `1`; each commit touches a single
    /// unique file `file_{i}.rs`.
    pub fn make_commits_newest_first(count: usize) -> Vec<CommitInfo> {
        (0..count)
            .map(|i| make_commit(i, (count - i) as i64, &[&format!("file_{i}.rs")]))
            .collect()
    }
}
