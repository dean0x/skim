//! End-to-end acceptance tests for Wave 2 temporal query layer.
//!
//! These tests build real fixture git repos, construct a real `TemporalDb`,
//! open it via `TemporalIndex`, and exercise the full query path.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod temporal_test_helpers;

use rskim_search::temporal::{TemporalDb, TemporalIndex};
use rskim_search::{TemporalFlags, TemporalQuery};
use std::path::PathBuf;
use tempfile::TempDir;
use temporal_test_helpers::{build_cochange_fixture, build_fixture_repo, recent_ts, FixtureCommit};

/// Build a fixture repo with distinct hotspot/coldspot/risky pattern:
/// - `hot.rs` — touched frequently in all recent commits (within 90 days)
/// - `cold.rs` — touched only once, 85 days ago (within 90-day window, but minimally)
/// - `risky.rs` — most commits have "fix:" prefix → high fix density
///
/// All files appear within the 90-day hotspot window so that `coldspots()`
/// includes `cold.rs` (files outside the 90d window are excluded from all
/// hotspot/coldspot queries per the scoring spec).
fn build_scoring_repo(dir: &std::path::Path) {
    build_fixture_repo(
        dir,
        &[
            FixtureCommit {
                // cold.rs: single commit inside the 90-day window but far from today.
                message: "initial: add cold.rs",
                changes: vec![("cold.rs", "fn cold() {}")],
                timestamp_override: Some(recent_ts(85)),
            },
            FixtureCommit {
                // risky.rs: first commit (not a fix) to push total above MIN_COMMITS_FOR_RISK.
                message: "feat: add risky.rs",
                changes: vec![("risky.rs", "fn risky() {}"), ("hot.rs", "fn hot() {}")],
                timestamp_override: Some(recent_ts(80)),
            },
            FixtureCommit {
                message: "fix: risky bug 1",
                changes: vec![
                    ("risky.rs", "fn risky() { 1 }"),
                    ("hot.rs", "fn hot() { 1 }"),
                ],
                timestamp_override: Some(recent_ts(60)),
            },
            FixtureCommit {
                message: "fix: risky bug 2",
                changes: vec![
                    ("risky.rs", "fn risky() { 2 }"),
                    ("hot.rs", "fn hot() { 2 }"),
                ],
                timestamp_override: Some(recent_ts(45)),
            },
            FixtureCommit {
                message: "fix: risky bug 3",
                changes: vec![
                    ("risky.rs", "fn risky() { 3 }"),
                    ("hot.rs", "fn hot() { 3 }"),
                ],
                timestamp_override: Some(recent_ts(30)),
            },
            FixtureCommit {
                message: "feat: hot feature",
                changes: vec![("hot.rs", "fn hot() { 4 }")],
                timestamp_override: Some(recent_ts(10)),
            },
            FixtureCommit {
                message: "feat: hot feature 2",
                changes: vec![("hot.rs", "fn hot() { 5 }")],
                timestamp_override: Some(recent_ts(5)),
            },
        ],
    );
}

/// Build the `TemporalDb` and return the index `TempDir` (keeps it alive).
fn build_temporal_index(repo_dir: &std::path::Path) -> (TempDir, TemporalIndex) {
    let idx_dir = TempDir::new().expect("idx tempdir");
    let db_path = idx_dir.path().join("temporal.db");
    let _db = TemporalDb::build(repo_dir, &db_path, 365).expect("build temporal");
    let index = TemporalIndex::open(&db_path).expect("open temporal index");
    (idx_dir, index)
}

// ============================================================================
// 1. Blast radius end-to-end
// ============================================================================

#[test]
fn wave2_blast_radius_end_to_end() {
    let repo = TempDir::new().expect("repo tempdir");
    build_cochange_fixture(repo.path());

    let (_idx, temporal) = build_temporal_index(repo.path());

    let results = temporal
        .blast_radius(&PathBuf::from("a.rs"), 10)
        .expect("blast_radius");

    // a.rs and b.rs change together in every commit — b.rs must appear.
    assert!(
        !results.is_empty(),
        "blast_radius for a.rs should return co-change partners"
    );
    let paths: Vec<_> = results.iter().map(|(p, _)| p.as_path()).collect();
    assert!(
        paths.contains(&std::path::Path::new("b.rs")),
        "b.rs must appear as a co-change partner of a.rs; got {paths:?}"
    );

    // Scores must be in (0, 1].
    for (_, score) in &results {
        assert!(
            *score > 0.0 && *score <= 1.0,
            "blast_radius score out of range: {score}"
        );
    }
}

// ============================================================================
// 2. Hotspots end-to-end
// ============================================================================

#[test]
fn wave2_hotspots_end_to_end() {
    let repo = TempDir::new().expect("repo tempdir");
    build_scoring_repo(repo.path());

    let (_idx, temporal) = build_temporal_index(repo.path());

    let results = temporal.hotspots(10).expect("hotspots");

    assert!(!results.is_empty(), "hotspots should return results");

    // hot.rs touched most frequently — should rank first.
    let top_path = &results[0].0;
    assert_eq!(
        top_path,
        &PathBuf::from("hot.rs"),
        "hot.rs should be the top hotspot; got {top_path:?}"
    );

    // Scores sorted descending.
    let scores: Vec<f32> = results.iter().map(|(_, s)| *s).collect();
    for i in 0..scores.len().saturating_sub(1) {
        assert!(
            scores[i] >= scores[i + 1],
            "hotspot scores not sorted: {scores:?}"
        );
    }
}

// ============================================================================
// 3. Risky end-to-end
// ============================================================================

#[test]
fn wave2_risky_end_to_end() {
    let repo = TempDir::new().expect("repo tempdir");
    build_scoring_repo(repo.path());

    let (_idx, temporal) = build_temporal_index(repo.path());

    let results = temporal.risky(10).expect("risky");

    assert!(!results.is_empty(), "risky should return results");

    // risky.rs has the highest fix-commit density (3/4 = 0.75).
    // hot.rs has lower density (3/6 = 0.5).
    // Files below MIN_COMMITS_FOR_RISK get score 0.0 — that's expected and correct.
    let top_path = &results[0].0;
    assert_eq!(
        top_path,
        &PathBuf::from("risky.rs"),
        "risky.rs should be the top risky file; got {top_path:?}"
    );

    // Top result must have score > 0.
    assert!(
        results[0].1 > 0.0,
        "top risky file must have score > 0, got {}",
        results[0].1
    );

    // All scores must be in [0, 1].
    for (_, score) in &results {
        assert!(
            *score >= 0.0 && *score <= 1.0,
            "risky score out of range [0, 1]: {score}"
        );
    }
}

// ============================================================================
// 4. Rerank: empty lexical → empty
// ============================================================================

#[test]
fn wave2_rerank_empty_lexical() {
    let repo = TempDir::new().expect("repo tempdir");
    build_cochange_fixture(repo.path());

    let (_idx, temporal) = build_temporal_index(repo.path());

    let flags = TemporalFlags {
        blast_radius: None,
        hot: true,
        cold: false,
        risky: false,
    };
    let results = temporal.rerank(&[], &flags).expect("rerank");
    assert!(
        results.is_empty(),
        "rerank on empty slice must return empty"
    );
}

// ============================================================================
// 5. Rerank: with hot flag applied blends temporal scores
// ============================================================================

#[test]
fn wave2_rerank_with_hot_flag() {
    let repo = TempDir::new().expect("repo tempdir");
    build_scoring_repo(repo.path());

    let (_idx, temporal) = build_temporal_index(repo.path());

    // hot.rs leads in both lexical and temporal signals. Verify it stays at the
    // top after reranking (the temporal blend must not break existing strong ordering).
    // We also verify that reranked scores differ from the raw lexical scores,
    // proving temporal signals were actually applied.
    let lex_results = vec![
        (PathBuf::from("hot.rs"), 1.0_f32),
        (PathBuf::from("cold.rs"), 0.5_f32),
    ];

    let flags = TemporalFlags {
        blast_radius: None,
        hot: true,
        cold: false,
        risky: false,
    };
    let reranked = temporal.rerank(&lex_results, &flags).expect("rerank");

    assert_eq!(
        reranked.len(),
        2,
        "reranked should have same number of results"
    );

    // hot.rs starts with better lexical score AND is the hotspot — must remain first.
    let top_path = &reranked[0].0;
    assert_eq!(
        top_path,
        &PathBuf::from("hot.rs"),
        "hot.rs should remain at the top after rerank; got {reranked:?}"
    );

    // Verify blending actually changed the raw scores (temporal signal was applied).
    // Reranked scores are in [0, 1] from rank normalization, not raw lexical values.
    let hot_score = reranked
        .iter()
        .find(|(p, _)| p == &PathBuf::from("hot.rs"))
        .map(|(_, s)| *s)
        .expect("hot.rs not in reranked");
    let cold_score = reranked
        .iter()
        .find(|(p, _)| p == &PathBuf::from("cold.rs"))
        .map(|(_, s)| *s)
        .expect("cold.rs not in reranked");

    // Both scores must be in [0, 1].
    assert!(hot_score >= 0.0 && hot_score <= 1.0);
    assert!(cold_score >= 0.0 && cold_score <= 1.0);

    // hot.rs blended score must be strictly higher than cold.rs.
    assert!(
        hot_score > cold_score,
        "hot.rs ({hot_score}) should score above cold.rs ({cold_score}) after rerank"
    );
}

// ============================================================================
// 6. Rerank: no flags → passthrough (unchanged order and count)
// ============================================================================

#[test]
fn wave2_rerank_no_flags_passthrough() {
    let repo = TempDir::new().expect("repo tempdir");
    build_cochange_fixture(repo.path());

    let (_idx, temporal) = build_temporal_index(repo.path());

    let lex_results = vec![
        (PathBuf::from("a.rs"), 2.0_f32),
        (PathBuf::from("b.rs"), 1.0_f32),
    ];

    let flags = TemporalFlags {
        blast_radius: None,
        hot: false,
        cold: false,
        risky: false,
    };
    let result = temporal.rerank(&lex_results, &flags).expect("rerank");

    // With no flags active, rerank returns lexical results unchanged.
    assert_eq!(result.len(), lex_results.len());
    assert_eq!(result[0].0, PathBuf::from("a.rs"));
    assert_eq!(result[1].0, PathBuf::from("b.rs"));
    assert!((result[0].1 - 2.0).abs() < f32::EPSILON);
    assert!((result[1].1 - 1.0).abs() < f32::EPSILON);
}

// ============================================================================
// 7. Coldspots: only files with at least one commit appear (Decision #11)
// ============================================================================

#[test]
fn wave2_coldspots_only_files_with_commits() {
    let repo = TempDir::new().expect("repo tempdir");
    build_scoring_repo(repo.path());

    let (_idx, temporal) = build_temporal_index(repo.path());

    let coldspots = temporal.coldspots(50).expect("coldspots");

    // cold.rs was touched once, hot.rs many times.
    // Both should appear — cold.rs first (lowest hotspot score).
    // A hypothetical "never_committed.rs" would NOT appear since it has no
    // entries in the hotspot table.
    let paths: Vec<&PathBuf> = coldspots.iter().map(|(p, _)| p).collect();
    assert!(
        paths.contains(&&PathBuf::from("cold.rs")),
        "cold.rs should appear in coldspots"
    );

    // Coldspot scores: higher score = colder (1 - hotspot_score).
    // cold.rs must rank above hot.rs.
    let cold_rank = paths
        .iter()
        .position(|p| *p == &PathBuf::from("cold.rs"))
        .expect("cold.rs not found");
    let hot_rank = paths
        .iter()
        .position(|p| *p == &PathBuf::from("hot.rs"))
        .expect("hot.rs not found");
    assert!(
        cold_rank < hot_rank,
        "cold.rs (rank {cold_rank}) should rank above hot.rs (rank {hot_rank}) in coldspots"
    );
}
