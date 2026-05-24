//! Tests for CochangeMatrixReader.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use tempfile::TempDir;

use super::CochangeMatrixReader;
use crate::FileId;
use crate::cochange::CochangeMatrixBuilder;
use crate::cochange::test_helpers::{make_history, make_path_map};

fn build_matrix(tmp: &TempDir, commits: Vec<Vec<&str>>, paths: &[&str]) -> CochangeMatrixReader {
    let history = make_history(commits);
    let path_map = make_path_map(paths);
    let builder = CochangeMatrixBuilder::new(tmp.path().to_path_buf()).unwrap();
    builder.build(&history, &path_map).unwrap();
    CochangeMatrixReader::open(tmp.path()).unwrap()
}

// -----------------------------------------------------------------------
// Open errors
// -----------------------------------------------------------------------

#[test]
fn test_open_nonexistent_dir_fails() {
    let result = CochangeMatrixReader::open(&PathBuf::from("/nonexistent/path"));
    assert!(result.is_err());
}

#[test]
fn test_open_corrupt_file_fails() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("cochange.skcc");
    std::fs::write(&path, b"garbage data not a valid skcc file").unwrap();
    let result = CochangeMatrixReader::open(tmp.path());
    assert!(result.is_err());
    let err = result.err().unwrap();
    let msg = format!("{err}");
    assert!(
        msg.contains("magic") || msg.contains("corrupt") || msg.contains("truncated"),
        "error should describe corruption: {msg}"
    );
}

// -----------------------------------------------------------------------
// Empty matrix
// -----------------------------------------------------------------------

#[test]
fn test_empty_matrix_pair_count_zero() {
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(&tmp, vec![], &[]);

    // No pairs for any IDs
    assert_eq!(reader.pair_count(FileId(0), FileId(1)).unwrap(), 0);
}

// -----------------------------------------------------------------------
// pair_count
// -----------------------------------------------------------------------

#[test]
fn test_pair_count_correct() {
    let tmp = TempDir::new().unwrap();
    // a and b co-change in 3 commits
    let reader = build_matrix(
        &tmp,
        vec![
            vec!["a.rs", "b.rs"],
            vec!["a.rs", "b.rs"],
            vec!["a.rs", "b.rs"],
        ],
        &["a.rs", "b.rs"],
    );

    assert_eq!(reader.pair_count(FileId(0), FileId(1)).unwrap(), 3);
}

#[test]
fn test_pair_count_canonical_order_transparent_to_caller() {
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(&tmp, vec![vec!["a.rs", "b.rs"]], &["a.rs", "b.rs"]);

    // Regardless of which order the caller passes the IDs, result is the same.
    assert_eq!(reader.pair_count(FileId(0), FileId(1)).unwrap(), 1);
    assert_eq!(reader.pair_count(FileId(1), FileId(0)).unwrap(), 1);
}

#[test]
fn test_pair_count_absent_pair_returns_zero() {
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(&tmp, vec![vec!["a.rs", "b.rs"]], &["a.rs", "b.rs", "c.rs"]);

    // c.rs never co-changed with anyone
    assert_eq!(reader.pair_count(FileId(0), FileId(2)).unwrap(), 0);
}

// -----------------------------------------------------------------------
// Jaccard similarity
// -----------------------------------------------------------------------

#[test]
fn test_jaccard_known_values() {
    // a appears in 4 commits, b appears in 4 commits, they co-change in 2.
    // Jaccard = 2 / (4 + 4 - 2) = 2/6 ≈ 0.333
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(
        &tmp,
        vec![
            vec!["a.rs", "b.rs"],
            vec!["a.rs", "b.rs"],
            vec!["a.rs"],
            vec!["a.rs"],
            vec!["b.rs"],
            vec!["b.rs"],
        ],
        &["a.rs", "b.rs"],
    );

    let j = reader.jaccard(FileId(0), FileId(1)).unwrap();
    let expected = 2.0 / 6.0;
    assert!(
        (j - expected).abs() < 1e-9,
        "Jaccard should be ~{expected:.4}, got {j:.4}"
    );
}

#[test]
fn test_jaccard_self_pair_returns_zero() {
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(&tmp, vec![vec!["a.rs", "b.rs"]], &["a.rs", "b.rs"]);

    // Self-pair: file_id A with itself
    let j = reader.jaccard(FileId(0), FileId(0)).unwrap();
    assert_eq!(j, 0.0, "self-pair Jaccard should be 0.0");
}

#[test]
fn test_jaccard_absent_pair_returns_zero() {
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(&tmp, vec![vec!["a.rs", "b.rs"]], &["a.rs", "b.rs", "c.rs"]);

    let j = reader.jaccard(FileId(0), FileId(2)).unwrap();
    assert_eq!(j, 0.0, "absent pair Jaccard should be 0.0");
}

#[test]
fn test_jaccard_no_shared_commits_returns_zero() {
    // Both files appear in 0 commits each (empty matrix with unknown file IDs)
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(&tmp, vec![], &["a.rs", "b.rs"]);

    let j = reader.jaccard(FileId(0), FileId(1)).unwrap();
    assert_eq!(j, 0.0, "zero denominator Jaccard should be 0.0");
}

// -----------------------------------------------------------------------
// file_commits
// -----------------------------------------------------------------------

#[test]
fn test_file_commits_correct() {
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(
        &tmp,
        vec![vec!["a.rs", "b.rs"], vec!["a.rs"], vec!["a.rs"]],
        &["a.rs", "b.rs"],
    );

    assert_eq!(
        reader.file_commits(FileId(0)).unwrap(),
        3,
        "a.rs in 3 commits"
    );
    assert_eq!(
        reader.file_commits(FileId(1)).unwrap(),
        1,
        "b.rs in 1 commit"
    );
}

#[test]
fn test_file_commits_unknown_id_returns_zero() {
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(&tmp, vec![vec!["a.rs"]], &["a.rs"]);

    // FileId(99) was never seen
    assert_eq!(reader.file_commits(FileId(99)).unwrap(), 0);
}

// -----------------------------------------------------------------------
// pairs_for_file
// -----------------------------------------------------------------------

#[test]
fn test_pairs_for_file_sorted_by_count_desc() {
    let tmp = TempDir::new().unwrap();
    // a co-changes with b twice and c once
    let reader = build_matrix(
        &tmp,
        vec![
            vec!["a.rs", "b.rs"],
            vec!["a.rs", "b.rs"],
            vec!["a.rs", "c.rs"],
        ],
        &["a.rs", "b.rs", "c.rs"],
    );

    let pairs = reader.pairs_for_file(FileId(0)).unwrap();
    assert_eq!(pairs.len(), 2, "a.rs co-changes with 2 files");

    // First result should be the highest count (b.rs with count 2)
    assert_eq!(
        pairs[0].0,
        FileId(1),
        "highest count partner should be first"
    );
    assert_eq!(pairs[0].1, 2);

    assert_eq!(pairs[1].0, FileId(2));
    assert_eq!(pairs[1].1, 1);
}

#[test]
fn test_pairs_for_file_unknown_file_returns_empty() {
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(&tmp, vec![vec!["a.rs", "b.rs"]], &["a.rs", "b.rs"]);

    let pairs = reader.pairs_for_file(FileId(99)).unwrap();
    assert!(pairs.is_empty());
}

// -----------------------------------------------------------------------
// Send + Sync compile check
// -----------------------------------------------------------------------

#[test]
fn test_reader_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CochangeMatrixReader>();
}

// -----------------------------------------------------------------------
// CRC32 mismatch detection
// -----------------------------------------------------------------------

#[test]
fn test_crc32_mismatch_detected() {
    let tmp = TempDir::new().unwrap();
    // Build a valid matrix first
    let builder = CochangeMatrixBuilder::new(tmp.path().to_path_buf()).unwrap();
    let history = make_history(vec![vec!["a.rs", "b.rs"]]);
    let path_map = make_path_map(&["a.rs", "b.rs"]);
    builder.build(&history, &path_map).unwrap();

    // Corrupt the file by flipping bytes in the data section (after the header)
    use crate::cochange::format::HEADER_SIZE;
    let path = tmp.path().join("cochange.skcc");
    let mut data = std::fs::read(&path).unwrap();
    assert!(data.len() > HEADER_SIZE, "test requires data section after header");
    data[HEADER_SIZE] ^= 0xFF; // flip first byte after header
    std::fs::write(&path, &data).unwrap();

    let result = CochangeMatrixReader::open(tmp.path());
    assert!(result.is_err());
    let err = result.err().unwrap();
    let msg = format!("{err}");
    assert!(
        msg.contains("checksum") || msg.contains("corrupt"),
        "error should mention checksum: {msg}"
    );
}

// -----------------------------------------------------------------------
// Size mismatch detection (body truncated after valid header)
// -----------------------------------------------------------------------

#[test]
fn test_open_size_mismatch_detected() {
    use crate::cochange::format::HEADER_SIZE;

    let tmp = TempDir::new().unwrap();
    // Build a valid matrix with some data
    let builder = CochangeMatrixBuilder::new(tmp.path().to_path_buf()).unwrap();
    let history = make_history(vec![vec!["a.rs", "b.rs"]]);
    let path_map = make_path_map(&["a.rs", "b.rs"]);
    builder.build(&history, &path_map).unwrap();

    // Read the valid file, then truncate after header
    let path = tmp.path().join("cochange.skcc");
    let data = std::fs::read(&path).unwrap();
    assert!(
        data.len() > HEADER_SIZE + 1,
        "valid matrix should have data after header"
    );

    // Keep header intact but remove some body bytes
    let truncated = &data[..data.len() - 4];
    std::fs::write(&path, truncated).unwrap();

    let result = CochangeMatrixReader::open(tmp.path());
    assert!(result.is_err());
    let msg = format!("{}", result.err().unwrap());
    assert!(
        msg.contains("size mismatch") || msg.contains("checksum"),
        "error should mention size mismatch or checksum: {msg}"
    );
}

// -----------------------------------------------------------------------
// Jaccard perfect coupling (1.0)
// -----------------------------------------------------------------------

#[test]
fn test_jaccard_perfect_coupling() {
    // a and b co-change in every commit; neither appears alone.
    // Jaccard = 3 / (3 + 3 - 3) = 3/3 = 1.0
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(
        &tmp,
        vec![
            vec!["a.rs", "b.rs"],
            vec!["a.rs", "b.rs"],
            vec!["a.rs", "b.rs"],
        ],
        &["a.rs", "b.rs"],
    );

    let j = reader.jaccard(FileId(0), FileId(1)).unwrap();
    assert!(
        (j - 1.0_f64).abs() < 1e-9,
        "Jaccard should be 1.0 for perfectly coupled files, got {j:.9}"
    );
}

// -----------------------------------------------------------------------
// pairs_for_file — file appears only as the higher ID
// -----------------------------------------------------------------------

#[test]
fn test_pairs_for_file_higher_id() {
    // Setup: three files a(0), b(1), c(2).
    // Commits: (a,c) and (b,c).
    // Canonical pairs stored: (0,2) and (1,2).
    // c has FileId(2) — it always appears as file_b (higher ID) in both pairs.
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(
        &tmp,
        vec![vec!["a.rs", "c.rs"], vec!["b.rs", "c.rs"]],
        &["a.rs", "b.rs", "c.rs"],
    );

    // Querying FileId(2) exercises the `entry.file_b == id` branch.
    let pairs = reader.pairs_for_file(FileId(2)).unwrap();
    assert_eq!(pairs.len(), 2, "c.rs co-changes with both a.rs and b.rs");

    // Both partners should be present; counts are 1 each.
    let partner_ids: Vec<u32> = pairs.iter().map(|(fid, _)| fid.0).collect();
    assert!(
        partner_ids.contains(&0),
        "a.rs (FileId(0)) should be a partner of c.rs"
    );
    assert!(
        partner_ids.contains(&1),
        "b.rs (FileId(1)) should be a partner of c.rs"
    );
    for &(_, count) in &pairs {
        assert_eq!(count, 1, "each partnership has count 1");
    }
}
