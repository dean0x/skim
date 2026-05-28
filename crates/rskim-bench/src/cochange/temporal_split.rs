//! Temporal train/test split for co-change validation.
//!
//! # Temporal split contract
//!
//! - [`GixSource`] returns commits in **newest-first** order.
//! - We reverse to **chronological** order (oldest-first) before splitting so
//!   that the training set contains the oldest commits and the test set
//!   contains the most recent ones.
//! - The split index is `floor(len * train_fraction)`.  All commits with index
//!   `< split_index` are training commits; the rest are test commits.
//! - This guarantees no temporal leakage: every training commit is strictly
//!   older than every test commit (assuming monotonically increasing
//!   timestamps, which is the common case for well-maintained repos).
//!
//! # Edge cases
//!
//! - **Same timestamp for all commits**: falls back to a pure index-based split
//!   on the reversed (oldest-first) slice.  The split boundary is still at
//!   `floor(len * train_fraction)`.
//! - **Empty input**: returns two empty vecs with `split_timestamp = 0`.
//! - **Single commit**: treated as training data; test vec is empty.

use rskim_search::CommitInfo;

// ============================================================================
// Public types
// ============================================================================

/// Result of a temporal split.
#[derive(Debug, Clone)]
pub struct TemporalSplit {
    /// Chronologically older commits (training set).
    pub train: Vec<CommitInfo>,
    /// Chronologically newer commits (test set).
    pub test: Vec<CommitInfo>,
    /// Unix timestamp at the split boundary (first test commit's timestamp).
    ///
    /// Equals `0` for empty inputs.
    pub split_timestamp: i64,
}

// ============================================================================
// Public API
// ============================================================================

/// Split `commits` into chronological train and test sets.
///
/// `commits` is expected to be in **newest-first** order (as returned by
/// [`GixSource::parse_history`]).  The function reverses to chronological order
/// before splitting.
///
/// `train_fraction` must be in `(0.0, 1.0)`.  Values outside this range are
/// clamped to `[0.01, 0.99]` to prevent degenerate empty splits.
///
/// # Panics
///
/// Does not panic — all edge cases (empty input, single commit, NaN
/// `train_fraction`) are handled gracefully.
#[must_use]
pub fn temporal_split(commits: &[CommitInfo], train_fraction: f64) -> TemporalSplit {
    if commits.is_empty() {
        return TemporalSplit {
            train: vec![],
            test: vec![],
            split_timestamp: 0,
        };
    }

    // Single commit: always goes to training. There is nothing to test against.
    if commits.len() == 1 {
        return TemporalSplit {
            train: commits.to_vec(),
            test: vec![],
            split_timestamp: 0,
        };
    }

    // Clamp fraction to avoid empty splits. Guard against NaN by treating
    // non-finite values as the default 0.8.
    let fraction = if train_fraction.is_finite() {
        train_fraction.clamp(0.01, 0.99)
    } else {
        0.8
    };

    // Reverse to chronological order (oldest first).
    // GixSource returns newest-first; we need oldest-first for the split.
    let mut chronological: Vec<CommitInfo> = commits.to_vec();
    chronological.reverse();

    let split_index = ((chronological.len() as f64) * fraction).floor() as usize;
    // Ensure at least 1 training commit and at least 1 test commit.
    let split_index = split_index.max(1).min(chronological.len() - 1);

    let split_timestamp = chronological
        .get(split_index)
        .map(|c| c.timestamp)
        .unwrap_or(0);

    let (train_slice, test_slice) = chronological.split_at(split_index);

    TemporalSplit {
        train: train_slice.to_vec(),
        test: test_slice.to_vec(),
        split_timestamp,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use rskim_search::{CommitInfo, FileChangeInfo};

    use super::*;

    /// Build a `CommitInfo` with a given timestamp (newest-first ordering).
    fn make_commit(timestamp: i64, id: usize) -> CommitInfo {
        CommitInfo {
            hash: format!("{id:040x}"),
            timestamp,
            author: "test".to_string(),
            message: format!("commit {id}"),
            changed_files: vec![FileChangeInfo {
                path: PathBuf::from(format!("file_{id}.rs")),
                additions: 1,
                deletions: 0,
            }],
        }
    }

    /// Build a slice of commits in newest-first order (as GixSource returns).
    fn make_commits_newest_first(count: usize) -> Vec<CommitInfo> {
        // timestamps: count, count-1, ..., 1 (newest first)
        (0..count)
            .map(|i| make_commit((count - i) as i64, i))
            .collect()
    }

    // --- 80/20 split ---

    #[test]
    fn split_80_20_correct_sizes() {
        let commits = make_commits_newest_first(10);
        let split = temporal_split(&commits, 0.8);
        assert_eq!(split.train.len(), 8, "expected 8 training commits");
        assert_eq!(split.test.len(), 2, "expected 2 test commits");
        assert_eq!(split.train.len() + split.test.len(), commits.len());
    }

    #[test]
    fn split_preserves_total_count() {
        for n in [5, 10, 20, 100] {
            let commits = make_commits_newest_first(n);
            let split = temporal_split(&commits, 0.8);
            assert_eq!(
                split.train.len() + split.test.len(),
                n,
                "total count must be preserved for n={n}"
            );
        }
    }

    // --- Chronological order preservation ---

    #[test]
    fn training_commits_are_chronologically_oldest() {
        let commits = make_commits_newest_first(10);
        let split = temporal_split(&commits, 0.8);

        // After reversing, training commits should have the lowest timestamps.
        let max_train_ts = split.train.iter().map(|c| c.timestamp).max().unwrap_or(0);
        let min_test_ts = split
            .test
            .iter()
            .map(|c| c.timestamp)
            .min()
            .unwrap_or(i64::MAX);
        assert!(
            max_train_ts <= min_test_ts,
            "training set must be chronologically older: max_train={max_train_ts}, min_test={min_test_ts}"
        );
    }

    #[test]
    fn split_timestamp_equals_first_test_commit() {
        let commits = make_commits_newest_first(10);
        let split = temporal_split(&commits, 0.8);
        // The split timestamp should equal the first test commit's timestamp.
        let first_test_ts = split.test.first().map(|c| c.timestamp).unwrap_or(0);
        assert_eq!(split.split_timestamp, first_test_ts);
    }

    // --- Edge cases ---

    #[test]
    fn empty_input_returns_empty_split() {
        let split = temporal_split(&[], 0.8);
        assert!(split.train.is_empty());
        assert!(split.test.is_empty());
        assert_eq!(split.split_timestamp, 0);
    }

    #[test]
    fn single_commit_goes_to_training() {
        let commits = vec![make_commit(1000, 0)];
        let split = temporal_split(&commits, 0.8);
        assert_eq!(split.train.len(), 1, "single commit must go to training");
        assert!(
            split.test.is_empty(),
            "test must be empty for single commit"
        );
        assert_eq!(split.split_timestamp, 0);
    }

    #[test]
    fn two_commits_split_correctly() {
        let commits = make_commits_newest_first(2);
        let split = temporal_split(&commits, 0.8);
        // floor(2 * 0.8) = 1, so 1 train, 1 test.
        assert_eq!(split.train.len(), 1);
        assert_eq!(split.test.len(), 1);
    }

    // --- Same timestamp fallback ---

    #[test]
    fn same_timestamp_fallback_splits_by_index() {
        let same_ts_commits: Vec<CommitInfo> = (0..10)
            .map(|i| make_commit(42, i)) // all same timestamp
            .collect();
        let split = temporal_split(&same_ts_commits, 0.8);
        // Should still split at floor(10 * 0.8) = 8.
        assert_eq!(split.train.len(), 8);
        assert_eq!(split.test.len(), 2);
    }

    // --- No leakage property ---

    #[test]
    fn test_commits_not_in_training_set() {
        let commits = make_commits_newest_first(20);
        let split = temporal_split(&commits, 0.8);

        let train_hashes: std::collections::HashSet<&str> =
            split.train.iter().map(|c| c.hash.as_str()).collect();
        let test_hashes: std::collections::HashSet<&str> =
            split.test.iter().map(|c| c.hash.as_str()).collect();

        assert!(
            train_hashes.is_disjoint(&test_hashes),
            "no commit should appear in both train and test"
        );
    }

    // --- Fraction clamping ---

    #[test]
    fn fraction_above_1_clamped() {
        let commits = make_commits_newest_first(10);
        let split = temporal_split(&commits, 1.5);
        // Clamped to 0.99 → split at floor(10 * 0.99) = 9.
        assert_eq!(split.train.len() + split.test.len(), 10);
        assert!(!split.train.is_empty());
    }

    #[test]
    fn fraction_below_0_clamped() {
        let commits = make_commits_newest_first(10);
        let split = temporal_split(&commits, -0.5);
        // Clamped to 0.01 → split at floor(10 * 0.01) = 0, then max(1) → 1.
        assert_eq!(split.train.len() + split.test.len(), 10);
        assert!(!split.train.is_empty());
    }
}
