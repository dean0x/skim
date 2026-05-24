//! Tests for CochangeMatrixBuilder.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::path::PathBuf;

use tempfile::TempDir;

use super::{COUPLING_MAX_FILES, CochangeMatrixBuilder};
use crate::{CommitInfo, FileChangeInfo, FileId, HistoryResult, TemporalMetadata};

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

fn make_history(commits: Vec<Vec<&str>>) -> HistoryResult {
    let commit_list = commits
        .into_iter()
        .enumerate()
        .map(|(i, paths)| CommitInfo {
            hash: format!("{i:040x}"),
            timestamp: i as i64,
            author: "test".to_string(),
            message: "test commit".to_string(),
            changed_files: paths
                .into_iter()
                .map(|p| FileChangeInfo {
                    path: PathBuf::from(p),
                    additions: 1,
                    deletions: 0,
                })
                .collect(),
        })
        .collect();
    HistoryResult {
        commits: commit_list,
        metadata: TemporalMetadata {
            is_shallow: false,
            commit_count: 0,
        },
    }
}

fn make_path_map(paths: &[&str]) -> HashMap<PathBuf, FileId> {
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| (PathBuf::from(p), FileId(i as u32)))
        .collect()
}

// -----------------------------------------------------------------------
// Constructor validation
// -----------------------------------------------------------------------

#[test]
fn test_constructor_nonexistent_dir_fails() {
    let dir = PathBuf::from("/nonexistent/path/that/does/not/exist");
    let result = CochangeMatrixBuilder::new(dir);
    assert!(result.is_err(), "should fail on nonexistent directory");
}

#[test]
fn test_constructor_existing_dir_succeeds() {
    let tmp = TempDir::new().unwrap();
    let result = CochangeMatrixBuilder::new(tmp.path().to_path_buf());
    assert!(result.is_ok(), "should succeed with existing directory");
}

// -----------------------------------------------------------------------
// Empty history
// -----------------------------------------------------------------------

#[test]
fn test_empty_history_writes_matrix_with_zero_stats() {
    let tmp = TempDir::new().unwrap();
    let builder = CochangeMatrixBuilder::new(tmp.path().to_path_buf()).unwrap();
    let history = make_history(vec![]);
    let path_map = make_path_map(&[]);

    let stats = builder.build(&history, &path_map).unwrap();

    assert_eq!(stats.pair_count, 0);
    assert_eq!(stats.file_count, 0);
    assert_eq!(stats.commits_processed, 0);
    assert_eq!(stats.commits_skipped_too_large, 0);
    assert_eq!(stats.unknown_paths_skipped, 0);

    // File must exist
    assert!(tmp.path().join("cochange.skcc").exists());
}

// -----------------------------------------------------------------------
// Single commit / two files
// -----------------------------------------------------------------------

#[test]
fn test_single_commit_two_files_creates_one_pair() {
    let tmp = TempDir::new().unwrap();
    let builder = CochangeMatrixBuilder::new(tmp.path().to_path_buf()).unwrap();
    let history = make_history(vec![vec!["a.rs", "b.rs"]]);
    let path_map = make_path_map(&["a.rs", "b.rs"]);

    let stats = builder.build(&history, &path_map).unwrap();

    assert_eq!(stats.pair_count, 1);
    assert_eq!(stats.file_count, 2);
    assert_eq!(stats.commits_processed, 1);
    assert_eq!(stats.commits_skipped_too_large, 0);
    assert_eq!(stats.unknown_paths_skipped, 0);
}

// -----------------------------------------------------------------------
// Self-pairs excluded
// -----------------------------------------------------------------------

#[test]
fn test_single_file_commit_produces_no_pairs() {
    let tmp = TempDir::new().unwrap();
    let builder = CochangeMatrixBuilder::new(tmp.path().to_path_buf()).unwrap();
    let history = make_history(vec![vec!["a.rs"]]);
    let path_map = make_path_map(&["a.rs"]);

    let stats = builder.build(&history, &path_map).unwrap();

    assert_eq!(stats.pair_count, 0, "a single file has no pairs");
}

// -----------------------------------------------------------------------
// Canonical pair ordering (min, max)
// -----------------------------------------------------------------------

#[test]
fn test_canonical_pair_ordering() {
    let tmp = TempDir::new().unwrap();
    let builder = CochangeMatrixBuilder::new(tmp.path().to_path_buf()).unwrap();
    // Reverse order to verify normalization
    let history = make_history(vec![vec!["b.rs", "a.rs"]]);
    let path_map = make_path_map(&["a.rs", "b.rs"]);

    let stats = builder.build(&history, &path_map).unwrap();

    // Still one canonical pair regardless of file order in commit
    assert_eq!(stats.pair_count, 1);

    // Verify the pair exists via reader
    use crate::cochange::CochangeMatrixReader;
    let reader = CochangeMatrixReader::open(tmp.path()).unwrap();
    let count = reader.pair_count(FileId(0), FileId(1)).unwrap();
    assert_eq!(count, 1);
}

// -----------------------------------------------------------------------
// COUPLING_MAX_FILES threshold
// -----------------------------------------------------------------------

#[test]
fn test_coupling_max_files_skip_exceeds() {
    let tmp = TempDir::new().unwrap();
    let builder = CochangeMatrixBuilder::new(tmp.path().to_path_buf()).unwrap();

    // Create a commit with COUPLING_MAX_FILES + 1 files
    let paths: Vec<String> = (0..=COUPLING_MAX_FILES).map(|i| format!("f{i}.rs")).collect();
    let path_strs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let history = make_history(vec![path_strs.clone()]);
    let path_map = make_path_map(&path_strs);

    let stats = builder.build(&history, &path_map).unwrap();

    assert_eq!(
        stats.commits_skipped_too_large, 1,
        "commit with > COUPLING_MAX_FILES files should be skipped"
    );
    assert_eq!(stats.pair_count, 0, "no pairs from skipped commit");
}

#[test]
fn test_coupling_max_files_exactly_at_limit_processed() {
    let tmp = TempDir::new().unwrap();
    let builder = CochangeMatrixBuilder::new(tmp.path().to_path_buf()).unwrap();

    // Exactly COUPLING_MAX_FILES files — should be processed
    let paths: Vec<String> = (0..COUPLING_MAX_FILES).map(|i| format!("f{i}.rs")).collect();
    let path_strs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let history = make_history(vec![path_strs.clone()]);
    let path_map = make_path_map(&path_strs);

    let stats = builder.build(&history, &path_map).unwrap();

    assert_eq!(
        stats.commits_skipped_too_large, 0,
        "commit with exactly COUPLING_MAX_FILES files should be processed"
    );
    // n files → n*(n-1)/2 pairs
    let expected_pairs = (COUPLING_MAX_FILES * (COUPLING_MAX_FILES - 1)) / 2;
    assert_eq!(stats.pair_count as usize, expected_pairs);
}

#[test]
fn test_coupling_max_files_below_limit_processed() {
    let tmp = TempDir::new().unwrap();
    let builder = CochangeMatrixBuilder::new(tmp.path().to_path_buf()).unwrap();

    let paths: Vec<String> = (0..3).map(|i| format!("f{i}.rs")).collect();
    let path_strs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let history = make_history(vec![path_strs.clone()]);
    let path_map = make_path_map(&path_strs);

    let stats = builder.build(&history, &path_map).unwrap();

    assert_eq!(stats.commits_skipped_too_large, 0);
    assert_eq!(stats.pair_count, 3, "3 files → 3 pairs");
}

// -----------------------------------------------------------------------
// Unknown paths
// -----------------------------------------------------------------------

#[test]
fn test_unknown_paths_skipped_counted() {
    let tmp = TempDir::new().unwrap();
    let builder = CochangeMatrixBuilder::new(tmp.path().to_path_buf()).unwrap();
    // "c.rs" is not in path_map
    let history = make_history(vec![vec!["a.rs", "b.rs", "c.rs"]]);
    let path_map = make_path_map(&["a.rs", "b.rs"]);

    let stats = builder.build(&history, &path_map).unwrap();

    assert_eq!(stats.unknown_paths_skipped, 1);
    // a.rs and b.rs still form a valid pair
    assert_eq!(stats.pair_count, 1);
}

// -----------------------------------------------------------------------
// Commits processed counter
// -----------------------------------------------------------------------

#[test]
fn test_commits_processed_counts_all_commits() {
    let tmp = TempDir::new().unwrap();
    let builder = CochangeMatrixBuilder::new(tmp.path().to_path_buf()).unwrap();

    // 3 commits, one is too large to produce pairs but still counted
    let paths: Vec<String> = (0..=COUPLING_MAX_FILES).map(|i| format!("f{i}.rs")).collect();
    let path_strs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let history = make_history(vec![
        vec!["a.rs", "b.rs"],
        path_strs.clone(),
        vec!["a.rs", "c.rs"],
    ]);
    let mut path_map = make_path_map(&["a.rs", "b.rs", "c.rs"]);
    for (i, p) in path_strs.iter().enumerate() {
        path_map.entry(PathBuf::from(p)).or_insert(FileId(i as u32 + 100));
    }

    let stats = builder.build(&history, &path_map).unwrap();

    assert_eq!(stats.commits_processed, 3, "all 3 commits counted");
    assert_eq!(stats.commits_skipped_too_large, 1);
}

// -----------------------------------------------------------------------
// Atomic write produces file
// -----------------------------------------------------------------------

#[test]
fn test_atomic_write_produces_file() {
    let tmp = TempDir::new().unwrap();
    let builder = CochangeMatrixBuilder::new(tmp.path().to_path_buf()).unwrap();
    let history = make_history(vec![vec!["a.rs", "b.rs"]]);
    let path_map = make_path_map(&["a.rs", "b.rs"]);

    builder.build(&history, &path_map).unwrap();

    assert!(
        tmp.path().join("cochange.skcc").exists(),
        "cochange.skcc must be written"
    );
}

// -----------------------------------------------------------------------
// Build-then-read roundtrip
// -----------------------------------------------------------------------

#[test]
fn test_build_then_read_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let builder = CochangeMatrixBuilder::new(tmp.path().to_path_buf()).unwrap();

    // Two commits: (a,b) and (a,b,c)
    let history = make_history(vec![
        vec!["a.rs", "b.rs"],
        vec!["a.rs", "b.rs", "c.rs"],
    ]);
    let path_map = make_path_map(&["a.rs", "b.rs", "c.rs"]);

    let stats = builder.build(&history, &path_map).unwrap();

    // a-b co-changed in both commits
    assert_eq!(stats.pair_count, 3); // (a,b), (a,c), (b,c)

    use crate::cochange::CochangeMatrixReader;
    let reader = CochangeMatrixReader::open(tmp.path()).unwrap();

    let ab = reader.pair_count(FileId(0), FileId(1)).unwrap();
    assert_eq!(ab, 2, "a and b co-changed in 2 commits");

    let ac = reader.pair_count(FileId(0), FileId(2)).unwrap();
    assert_eq!(ac, 1, "a and c co-changed in 1 commit");

    let bc = reader.pair_count(FileId(1), FileId(2)).unwrap();
    assert_eq!(bc, 1, "b and c co-changed in 1 commit");
}
