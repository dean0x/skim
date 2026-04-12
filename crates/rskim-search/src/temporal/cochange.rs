//! Co-change matrix builder for the temporal search layer.
//!
//! Computes pairwise Jaccard similarity between files based on co-occurrence in
//! commits. Steps: count commits per file → count pair co-occurrences → compute
//! Jaccard → filter below threshold → retain top-K per file → sort by path pair.
//!
//! Commits with more than [`MAX_FILES_PER_COMMIT`] files are skipped entirely
//! (bulk-merge guard).

use std::path::{Path, PathBuf};

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
    // Build an intern table once so all downstream helpers work with u32 IDs
    // instead of cloning PathBuf on every inner-loop iteration.
    let (path_to_id, id_to_path) = build_intern_table(commits);

    // Step 1: Per-file commit counts (id → count).
    let file_commit_count = compute_file_commit_counts(commits, &path_to_id);

    // Step 2: Pairwise co-occurrence counts (id pair → count).
    let pair_count = compute_pair_counts(commits, &path_to_id);

    // Step 3: Convert to CochangeEntry with Jaccard, filtering below threshold.
    let mut entries = build_entries(&file_commit_count, pair_count, &id_to_path);

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

/// Build a path intern table from all files referenced in `commits`.
///
/// Returns `(path_to_id, id_to_path)` where `id_to_path[id]` is the canonical
/// `&Path` for that id, and `path_to_id` maps each path to its u32 id.
/// IDs are assigned in first-seen order across all qualifying commits.
fn build_intern_table<'a>(
    commits: &'a [CommitInfo],
) -> (FxHashMap<&'a Path, u32>, Vec<&'a Path>) {
    let mut path_to_id: FxHashMap<&'a Path, u32> = FxHashMap::default();
    let mut id_to_path: Vec<&'a Path> = Vec::new();

    for commit in commits {
        if commit.changed_files.len() > MAX_FILES_PER_COMMIT {
            continue;
        }
        for file in &commit.changed_files {
            if let std::collections::hash_map::Entry::Vacant(e) =
                path_to_id.entry(file.as_path())
            {
                let id = u32::try_from(id_to_path.len()).unwrap_or(u32::MAX);
                e.insert(id);
                id_to_path.push(file.as_path());
            }
        }
    }

    (path_to_id, id_to_path)
}

/// Count the number of qualifying commits each file appears in.
///
/// Commits with more than [`MAX_FILES_PER_COMMIT`] files are skipped entirely.
/// Returns a map of `file_id → commit_count`.
fn compute_file_commit_counts(
    commits: &[CommitInfo],
    path_to_id: &FxHashMap<&Path, u32>,
) -> FxHashMap<u32, u32> {
    let mut counts: FxHashMap<u32, u32> = FxHashMap::default();
    for commit in commits {
        if commit.changed_files.len() > MAX_FILES_PER_COMMIT {
            continue;
        }
        for file in &commit.changed_files {
            if let Some(&id) = path_to_id.get(file.as_path()) {
                *counts.entry(id).or_default() += 1;
            }
        }
    }
    counts
}

/// Count the number of commits where each ordered file pair co-appears.
///
/// Pairs are stored in canonical form with IDs ordered by their corresponding
/// path's lexicographic order (smaller path first), matching the original
/// `PathBuf` key behaviour. Duplicate file paths within a single commit are
/// deduplicated before pairing so a commit touching the same path twice counts
/// as one. Returns a map of `(id_a, id_b) → co_occurrence_count` where
/// `id_to_path[id_a] < id_to_path[id_b]` lexicographically.
fn compute_pair_counts(
    commits: &[CommitInfo],
    path_to_id: &FxHashMap<&Path, u32>,
) -> FxHashMap<(u32, u32), u32> {
    let mut counts: FxHashMap<(u32, u32), u32> = FxHashMap::default();
    for commit in commits {
        if commit.changed_files.len() > MAX_FILES_PER_COMMIT {
            continue;
        }
        // Resolve paths to (path, id) pairs, sort by path for canonical ordering,
        // then deduplicate — defensive against duplicate paths from gix.
        let mut unique: Vec<(&Path, u32)> = commit
            .changed_files
            .iter()
            .filter_map(|f| path_to_id.get(f.as_path()).map(|&id| (f.as_path(), id)))
            .collect();
        // Sort by path to produce lexicographic canonical ordering (smaller path first).
        unique.sort_unstable_by_key(|&(p, _)| p);
        unique.dedup_by_key(|&mut (p, _)| p);

        for i in 0..unique.len() {
            for j in (i + 1)..unique.len() {
                // unique is sorted by path, so unique[i].0 <= unique[j].0 always —
                // canonical ordering (smaller path first) is preserved by construction.
                *counts.entry((unique[i].1, unique[j].1)).or_default() += 1;
            }
        }
    }
    counts
}

/// Convert the raw pair counts into [`CochangeEntry`] values with Jaccard scores.
///
/// Pairs below [`MIN_CO_OCCURRENCES`] and pairs whose union is zero (impossible
/// in practice but guarded defensively) are discarded.
/// Only at this point are IDs converted back to `PathBuf` for the output structs.
fn build_entries(
    file_commit_count: &FxHashMap<u32, u32>,
    pair_count: FxHashMap<(u32, u32), u32>,
    id_to_path: &[&Path],
) -> Vec<CochangeEntry> {
    let mut entries = Vec::with_capacity(pair_count.len());
    for ((id_a, id_b), co_occurrences) in pair_count {
        if co_occurrences < MIN_CO_OCCURRENCES {
            continue;
        }
        let count_a = file_commit_count.get(&id_a).copied().unwrap_or(0);
        let count_b = file_commit_count.get(&id_b).copied().unwrap_or(0);
        // Jaccard union: |A ∪ B| = |A| + |B| - |A ∩ B|
        // Widen to u64 to prevent overflow when count_a + count_b exceeds u32::MAX.
        let union = u64::from(count_a) + u64::from(count_b) - u64::from(co_occurrences);
        if union == 0 {
            // Defensive guard — cannot happen if co_occurrences >= 1.
            continue;
        }
        let jaccard = co_occurrences as f32 / union as f32;
        // Convert IDs back to PathBuf only at output boundary.
        let path_a = id_to_path[id_a as usize].to_path_buf();
        let path_b = id_to_path[id_b as usize].to_path_buf();
        entries.push(CochangeEntry {
            path_a,
            path_b,
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
