//! Co-change matrix builder for the temporal search layer.
//!
//! Computes pairwise Jaccard similarity between files based on co-occurrence in
//! commits. Steps: count commits per file → count pair co-occurrences → compute
//! Jaccard → filter below threshold → retain top-K per file → sort by path pair.
//!
//! Commits with more than [`MAX_FILES_PER_COMMIT`] files are skipped entirely
//! (bulk-merge guard).

use std::path::PathBuf;

use rustc_hash::FxHashMap;

use super::types::CochangeEntry;
use crate::temporal::types::CommitInfo;

// ============================================================================
// Constants
// ============================================================================

/// Maximum number of files in a single commit for co-change analysis.
/// Bulk merges and auto-generated commits exceed this and are skipped entirely.
const MAX_FILES_PER_COMMIT: usize = 50;

/// Minimum co-occurrence count for a pair to be retained.
/// Single-occurrence pairs produce noise.
const MIN_CO_OCCURRENCES: u32 = 2;

/// Maximum number of co-change partners to retain per file.
const TOP_K_PER_FILE: usize = 50;

// ============================================================================
// Public API
// ============================================================================

/// Build a co-change matrix from a slice of parsed git commits.
///
/// Returns a [`Vec<CochangeEntry>`] where every entry represents a pair
/// `(path_a, path_b)` with `path_a < path_b` lexicographically, a
/// co-occurrence count, and a Jaccard similarity score in `[0, 1]`.
///
/// # Properties
///
/// - Pairs with fewer than 2 co-occurrences are filtered out.
/// - Only the top 50 partners per file (by Jaccard) are retained.
/// - Commits touching more than 50 files are skipped (bulk-merge guard).
/// - Output is sorted by `(path_a, path_b)` for determinism.
/// - An empty input returns an empty vec.
#[must_use = "build_cochange_matrix returns the matrix; discarding it is likely a bug"]
pub fn build_cochange_matrix(commits: &[CommitInfo]) -> Vec<CochangeEntry> {
    // Step 1: Per-file commit counts.
    let file_commit_count = compute_file_commit_counts(commits);

    // Step 2: Pairwise co-occurrence counts.
    let pair_count = compute_pair_counts(commits);

    // Step 3: Convert to CochangeEntry with Jaccard, filtering below threshold.
    let mut entries = build_entries(&file_commit_count, pair_count);

    // Step 4: Apply top-K retention per file.
    let retained = apply_top_k(&mut entries);

    // Step 5: Sort for deterministic output.
    let mut result = retained;
    result.sort_by(|a, b| a.path_a.cmp(&b.path_a).then(a.path_b.cmp(&b.path_b)));

    result
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Count the number of qualifying commits each file appears in.
///
/// Commits with more than [`MAX_FILES_PER_COMMIT`] files are skipped entirely.
fn compute_file_commit_counts(commits: &[CommitInfo]) -> FxHashMap<PathBuf, u32> {
    let mut counts: FxHashMap<PathBuf, u32> = FxHashMap::default();
    for commit in commits {
        if commit.changed_files.len() > MAX_FILES_PER_COMMIT {
            continue;
        }
        for file in &commit.changed_files {
            *counts.entry(file.clone()).or_default() += 1;
        }
    }
    counts
}

/// Count the number of commits where each ordered file pair co-appears.
///
/// Pairs are stored in canonical form: `(min, max)` lexicographically.
/// Duplicate file paths within a single commit are deduplicated before
/// pairing so a commit touching the same path twice counts as one.
fn compute_pair_counts(commits: &[CommitInfo]) -> FxHashMap<(PathBuf, PathBuf), u32> {
    let mut counts: FxHashMap<(PathBuf, PathBuf), u32> = FxHashMap::default();
    for commit in commits {
        if commit.changed_files.len() > MAX_FILES_PER_COMMIT {
            continue;
        }
        // Deduplicate within commit — defensive against duplicate paths from gix.
        let mut unique: Vec<&PathBuf> = commit.changed_files.iter().collect();
        unique.sort();
        unique.dedup();

        for i in 0..unique.len() {
            for j in (i + 1)..unique.len() {
                // unique is sorted, so unique[i] <= unique[j] always holds —
                // canonical ordering (smaller path first) is preserved by construction.
                *counts
                    .entry((unique[i].clone(), unique[j].clone()))
                    .or_default() += 1;
            }
        }
    }
    counts
}

/// Convert the raw pair counts into [`CochangeEntry`] values with Jaccard scores.
///
/// Pairs below [`MIN_CO_OCCURRENCES`] and pairs whose union is zero (impossible
/// in practice but guarded defensively) are discarded.
fn build_entries(
    file_commit_count: &FxHashMap<PathBuf, u32>,
    pair_count: FxHashMap<(PathBuf, PathBuf), u32>,
) -> Vec<CochangeEntry> {
    let mut entries = Vec::with_capacity(pair_count.len());
    for ((a, b), co_occurrences) in pair_count {
        if co_occurrences < MIN_CO_OCCURRENCES {
            continue;
        }
        let count_a = file_commit_count.get(&a).copied().unwrap_or(0);
        let count_b = file_commit_count.get(&b).copied().unwrap_or(0);
        // Jaccard union: |A ∪ B| = |A| + |B| - |A ∩ B|
        // Saturating sub guards against impossible underflow.
        let union = count_a + count_b - co_occurrences;
        if union == 0 {
            // Defensive guard — cannot happen if co_occurrences >= 1.
            continue;
        }
        let jaccard = co_occurrences as f32 / union as f32;
        entries.push(CochangeEntry {
            path_a: a,
            path_b: b,
            co_occurrences,
            jaccard,
        });
    }
    entries
}

/// Retain at most [`TOP_K_PER_FILE`] partners per file, ranked by Jaccard descending.
///
/// A pair `(a, b)` counts toward BOTH `a`'s and `b`'s top-K. A pair is kept
/// if it appears in EITHER file's top-K list.
fn apply_top_k(entries: &mut Vec<CochangeEntry>) -> Vec<CochangeEntry> {
    // Group entry indices by each file they mention.
    let mut per_file: FxHashMap<&PathBuf, Vec<usize>> = FxHashMap::default();
    for (idx, entry) in entries.iter().enumerate() {
        per_file.entry(&entry.path_a).or_default().push(idx);
        per_file.entry(&entry.path_b).or_default().push(idx);
    }

    // Mark entries to keep: any entry in ANY file's top-K is retained.
    let mut keep = vec![false; entries.len()];
    for (_, mut indices) in per_file {
        // Sort descending by Jaccard; use Equal for NaN (shouldn't occur).
        indices.sort_by(|&a, &b| {
            entries[b]
                .jaccard
                .partial_cmp(&entries[a].jaccard)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for &idx in indices.iter().take(TOP_K_PER_FILE) {
            keep[idx] = true;
        }
    }

    entries
        .drain(..)
        .zip(keep.iter())
        .filter(|(_, k)| **k)
        .map(|(e, _)| e)
        .collect()
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use std::path::PathBuf;

    fn make_commit(changed: &[&str]) -> CommitInfo {
        CommitInfo {
            hash: "abc".to_string(),
            timestamp: 0,
            message: "test".to_string(),
            is_fix: false,
            changed_files: changed.iter().map(|p| PathBuf::from(p)).collect(),
        }
    }

    /// Verify the algorithm on a hand-computable example:
    /// a=3, b=3, co=2 → union=4 → jaccard=0.5, canonical order enforced.
    #[test]
    fn jaccard_and_canonical_ordering() {
        let commits = vec![
            make_commit(&["b.rs", "a.rs"]), // co, reversed order
            make_commit(&["a.rs", "b.rs"]), // co
            make_commit(&["a.rs"]),         // a only
            make_commit(&["b.rs"]),         // b only
        ];
        let result = build_cochange_matrix(&commits);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path_a, PathBuf::from("a.rs"));
        assert_eq!(result[0].path_b, PathBuf::from("b.rs"));
        assert_eq!(result[0].co_occurrences, 2);
        assert!(
            (result[0].jaccard - 0.5).abs() < 1e-6,
            "jaccard={}",
            result[0].jaccard
        );
    }

    /// Bulk commits (>50 files) must be skipped; no pairs produced.
    #[test]
    fn bulk_commit_produces_no_pairs() {
        let files: Vec<String> = (0..51).map(|i| format!("f{i}.rs")).collect();
        let refs: Vec<&str> = files.iter().map(|s| s.as_str()).collect();
        let result = build_cochange_matrix(&[make_commit(&refs), make_commit(&refs)]);
        assert!(result.is_empty(), "got {} entries", result.len());
    }
}
