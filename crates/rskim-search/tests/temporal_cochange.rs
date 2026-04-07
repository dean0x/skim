//! Integration tests for `temporal::cochange::build_cochange_matrix`.
//!
//! Tests that use `parse_history` build real git repos in `TempDir` via the
//! git CLI. Pure unit-level tests construct `CommitInfo` directly for speed
//! and isolation.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod temporal_test_helpers;

use rskim_search::temporal::{build_cochange_matrix, parse_history, CommitInfo};
use std::path::PathBuf;
use tempfile::TempDir;
use temporal_test_helpers::{build_fixture_repo, FixtureCommit};

// ============================================================================
// Helpers
// ============================================================================

fn make_commit(changed: &[&str]) -> CommitInfo {
    CommitInfo {
        hash: "test".to_string(),
        timestamp: 9_999_999_999,
        message: "test".to_string(),
        is_fix: false,
        changed_files: changed.iter().map(|p| PathBuf::from(p)).collect(),
    }
}

// ============================================================================
// Pure construction tests (no git I/O)
// ============================================================================

#[test]
fn cochange_empty_commits() {
    let result = build_cochange_matrix(&[]);
    assert!(result.is_empty(), "expected empty vec for empty input");
}

#[test]
fn cochange_single_commit_two_files_filtered() {
    // co_occurrences = 1 < MIN_CO_OCCURRENCES (2) → filtered out.
    let commits = vec![make_commit(&["a.rs", "b.rs"])];
    let result = build_cochange_matrix(&commits);
    assert!(
        result.is_empty(),
        "single co-occurrence must be filtered; got {result:?}"
    );
}

#[test]
fn cochange_two_commits_same_pair() {
    let commits = vec![
        make_commit(&["a.rs", "b.rs"]),
        make_commit(&["a.rs", "b.rs"]),
    ];
    let result = build_cochange_matrix(&commits);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].path_a, PathBuf::from("a.rs"));
    assert_eq!(result[0].path_b, PathBuf::from("b.rs"));
    assert_eq!(result[0].co_occurrences, 2);
    // a=2, b=2, co=2 → union=2 → jaccard=1.0
    assert!(
        (result[0].jaccard - 1.0).abs() < f32::EPSILON,
        "jaccard should be 1.0, got {}",
        result[0].jaccard
    );
}

#[test]
fn cochange_paired_vs_unpaired() {
    // a.rs and b.rs co-change 3×; c.rs changes alone 2×.
    let commits = vec![
        make_commit(&["a.rs", "b.rs"]),
        make_commit(&["a.rs", "b.rs"]),
        make_commit(&["a.rs", "b.rs"]),
        make_commit(&["c.rs"]),
        make_commit(&["c.rs"]),
    ];
    let result = build_cochange_matrix(&commits);
    // Only (a, b) pair qualifies; c has no partner.
    assert_eq!(result.len(), 1, "only (a,b) pair expected; got {result:?}");
    assert_eq!(result[0].path_a, PathBuf::from("a.rs"));
    assert_eq!(result[0].path_b, PathBuf::from("b.rs"));
}

#[test]
fn cochange_canonical_ordering() {
    // z.rs and a.rs — canonical order must be (a.rs, z.rs).
    let commits = vec![
        make_commit(&["z.rs", "a.rs"]),
        make_commit(&["z.rs", "a.rs"]),
    ];
    let result = build_cochange_matrix(&commits);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].path_a, PathBuf::from("a.rs"));
    assert_eq!(result[0].path_b, PathBuf::from("z.rs"));
}

#[test]
fn cochange_jaccard_calculation() {
    // a: 3 commits, b: 3 commits, co: 2 → union = 4 → jaccard = 0.5
    let commits = vec![
        make_commit(&["a.rs", "b.rs"]),
        make_commit(&["a.rs", "b.rs"]),
        make_commit(&["a.rs"]),
        make_commit(&["b.rs"]),
    ];
    let result = build_cochange_matrix(&commits);
    assert_eq!(result.len(), 1);
    assert!(
        (result[0].jaccard - 0.5).abs() < 1e-6,
        "expected jaccard=0.5, got {}",
        result[0].jaccard
    );
}

#[test]
fn cochange_bulk_commit_capped() {
    // 51 files per commit — exceeds MAX_FILES_PER_COMMIT, both commits skipped.
    let files: Vec<String> = (0..51).map(|i| format!("f{i}.rs")).collect();
    let refs: Vec<&str> = files.iter().map(|s| s.as_str()).collect();
    let commits = vec![make_commit(&refs), make_commit(&refs)];
    let result = build_cochange_matrix(&commits);
    assert!(
        result.is_empty(),
        "bulk commits must produce no pairs; got {} entries",
        result.len()
    );
}

#[test]
fn cochange_duplicate_files_in_commit() {
    // Same path twice in a single commit — deduplicated before pairing.
    let commits = vec![
        make_commit(&["a.rs", "a.rs", "b.rs"]),
        make_commit(&["a.rs", "b.rs"]),
    ];
    let result = build_cochange_matrix(&commits);
    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0].co_occurrences, 2,
        "duplicate path must count once"
    );
}

#[test]
fn cochange_top_k_retains_highest_jaccard() {
    // hub.rs co-changes with 3 other files at different frequencies.
    // All 3 pairs have co_occurrences >= 2, so all should be retained (< TOP_K=50).
    // Verify the pair with the highest Jaccard (hub+close) is present.
    let commits = vec![
        // hub + close: 4 co-changes (jaccard near-1.0)
        make_commit(&["hub.rs", "close.rs"]),
        make_commit(&["hub.rs", "close.rs"]),
        make_commit(&["hub.rs", "close.rs"]),
        make_commit(&["hub.rs", "close.rs"]),
        // hub + mid: 2 co-changes
        make_commit(&["hub.rs", "mid.rs"]),
        make_commit(&["hub.rs", "mid.rs"]),
        // hub + far: 2 co-changes; far also changes alone
        make_commit(&["hub.rs", "far.rs"]),
        make_commit(&["hub.rs", "far.rs"]),
        make_commit(&["far.rs"]),
        make_commit(&["far.rs"]),
    ];
    let result = build_cochange_matrix(&commits);
    // All 3 pairs qualify and are below TOP_K, so all 3 must be retained.
    assert_eq!(result.len(), 3, "expected 3 pairs; got {result:?}");

    // The (close, hub) pair must have the highest jaccard.
    let close_hub = result
        .iter()
        .find(|e| {
            (e.path_a == PathBuf::from("close.rs") && e.path_b == PathBuf::from("hub.rs"))
                || (e.path_a == PathBuf::from("hub.rs") && e.path_b == PathBuf::from("close.rs"))
        })
        .expect("close-hub pair not found");

    // hub: 8 commits, close: 4 commits, co=4 → union=8 → jaccard=4/8=0.5.
    // close-hub should have the highest jaccard among the three pairs.
    let max_jaccard = result.iter().map(|e| e.jaccard).fold(0.0_f32, f32::max);
    assert!(
        (close_hub.jaccard - max_jaccard).abs() < 1e-6,
        "close-hub should have the highest jaccard ({max_jaccard}), got {}",
        close_hub.jaccard
    );
}

#[test]
fn cochange_determinism() {
    // Same input twice must produce byte-identical results.
    let commits = vec![
        make_commit(&["x.rs", "y.rs"]),
        make_commit(&["x.rs", "y.rs"]),
        make_commit(&["a.rs", "b.rs"]),
        make_commit(&["a.rs", "b.rs"]),
        make_commit(&["a.rs", "c.rs"]),
        make_commit(&["a.rs", "c.rs"]),
    ];
    let first = build_cochange_matrix(&commits);
    let second = build_cochange_matrix(&commits);
    assert_eq!(first.len(), second.len());
    for (a, b) in first.iter().zip(second.iter()) {
        assert_eq!(a.path_a, b.path_a);
        assert_eq!(a.path_b, b.path_b);
        assert_eq!(a.co_occurrences, b.co_occurrences);
        assert_eq!(a.jaccard.to_bits(), b.jaccard.to_bits());
    }
}

// ============================================================================
// Integration tests using real git repos + parse_history
// ============================================================================

#[test]
fn cochange_symmetric_count() {
    // a.rs and b.rs co-change in both commits; jaccard must be 1.0.
    let tmp = TempDir::new().expect("create tempdir");
    build_fixture_repo(
        tmp.path(),
        &[
            FixtureCommit {
                message: "first",
                changes: vec![("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")],
                timestamp_override: None,
            },
            FixtureCommit {
                message: "second",
                changes: vec![("a.rs", "fn a2() {}\n"), ("b.rs", "fn b2() {}\n")],
                timestamp_override: None,
            },
        ],
    );
    let commits = parse_history(tmp.path(), 365).expect("parse_history");
    let entries = build_cochange_matrix(&commits);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path_a, PathBuf::from("a.rs"));
    assert_eq!(entries[0].path_b, PathBuf::from("b.rs"));
    assert_eq!(entries[0].co_occurrences, 2);
    assert!(
        (entries[0].jaccard - 1.0).abs() < f32::EPSILON,
        "jaccard should be 1.0, got {}",
        entries[0].jaccard
    );
}

#[test]
fn cochange_real_parsed_commits_end_to_end() {
    // 5-commit fixture repo. Verify end-to-end with parse_history.
    // Commit layout:
    //   1. a.rs + b.rs
    //   2. a.rs + b.rs
    //   3. a.rs + c.rs
    //   4. a.rs + c.rs
    //   5. d.rs (solo)
    //
    // Expected pairs: (a,b) co=2 jaccard=1.0, (a,c) co=2 jaccard=1.0.
    // d has no partner → no entry.
    let tmp = TempDir::new().expect("create tempdir");
    build_fixture_repo(
        tmp.path(),
        &[
            FixtureCommit {
                message: "commit-1",
                changes: vec![("a.rs", "1\n"), ("b.rs", "1\n")],
                timestamp_override: None,
            },
            FixtureCommit {
                message: "commit-2",
                changes: vec![("a.rs", "2\n"), ("b.rs", "2\n")],
                timestamp_override: None,
            },
            FixtureCommit {
                message: "commit-3",
                changes: vec![("a.rs", "3\n"), ("c.rs", "1\n")],
                timestamp_override: None,
            },
            FixtureCommit {
                message: "commit-4",
                changes: vec![("a.rs", "4\n"), ("c.rs", "2\n")],
                timestamp_override: None,
            },
            FixtureCommit {
                message: "commit-5",
                changes: vec![("d.rs", "1\n")],
                timestamp_override: None,
            },
        ],
    );
    let commits = parse_history(tmp.path(), 365).expect("parse_history");
    assert_eq!(commits.len(), 5, "expected 5 commits from parse_history");

    let entries = build_cochange_matrix(&commits);

    // (a,b) and (a,c) must both be present.
    let ab = entries
        .iter()
        .find(|e| e.path_a == PathBuf::from("a.rs") && e.path_b == PathBuf::from("b.rs"))
        .expect("(a,b) pair missing");
    assert_eq!(ab.co_occurrences, 2);

    let ac = entries
        .iter()
        .find(|e| e.path_a == PathBuf::from("a.rs") && e.path_b == PathBuf::from("c.rs"))
        .expect("(a,c) pair missing");
    assert_eq!(ac.co_occurrences, 2);

    // d.rs should have no partner.
    let d_pairs = entries
        .iter()
        .filter(|e| e.path_a == PathBuf::from("d.rs") || e.path_b == PathBuf::from("d.rs"));
    assert_eq!(
        d_pairs.count(),
        0,
        "d.rs (solo) should have no co-change partners"
    );
}
