//! Integration tests for `temporal::scoring::{hotspot_scores, risk_scores}`.
//!
//! Tests that need git history build real repos via the git CLI using the
//! `temporal_test_helpers` module. Pure-data tests construct `CommitInfo`
//! directly for speed and isolation.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod temporal_test_helpers;

use rskim_search::temporal::{hotspot_scores, parse_history, risk_scores};
use tempfile::TempDir;
use temporal_test_helpers::{build_fixture_repo, FixtureCommit};

// ============================================================================
// Helpers
// ============================================================================

/// Seconds per day.
const DAY_SECS: u64 = 86_400;

/// Current Unix epoch seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time before Unix epoch")
        .as_secs()
}

// ============================================================================
// Hotspot integration tests (real git repos)
// ============================================================================

#[test]
fn hotspot_single_file_single_commit() {
    let dir = TempDir::new().expect("tempdir");
    let now = now_secs();

    build_fixture_repo(
        dir.path(),
        &[FixtureCommit {
            message: "add foo",
            changes: vec![("foo.rs", "fn main() {}")],
            timestamp_override: Some(now as i64 - DAY_SECS as i64), // 1 day ago
        }],
    );

    let commits = parse_history(dir.path(), 365).expect("parse_history");
    let scores = hotspot_scores(&commits, now);

    assert_eq!(scores.len(), 1, "expected exactly 1 hotspot entry");
    assert!(
        (scores[0].score - 1.0).abs() < 1e-6,
        "sole file must have score = 1.0, got {}",
        scores[0].score
    );
    assert_eq!(scores[0].commit_count_90d, 1);
}

#[test]
fn hotspot_multiple_files_ranked_by_recency() {
    let dir = TempDir::new().expect("tempdir");
    let now = now_secs();

    // Three commits, progressively older.
    build_fixture_repo(
        dir.path(),
        &[
            FixtureCommit {
                message: "add recent.rs",
                changes: vec![("recent.rs", "fn a() {}")],
                timestamp_override: Some(now as i64 - DAY_SECS as i64), // 1 day ago
            },
            FixtureCommit {
                message: "add middle.rs",
                changes: vec![("middle.rs", "fn b() {}")],
                timestamp_override: Some(now as i64 - 30 * DAY_SECS as i64), // 30 days ago
            },
            FixtureCommit {
                message: "add older.rs",
                changes: vec![("older.rs", "fn c() {}")],
                timestamp_override: Some(now as i64 - 60 * DAY_SECS as i64), // 60 days ago
            },
        ],
    );

    let commits = parse_history(dir.path(), 365).expect("parse_history");
    let scores = hotspot_scores(&commits, now);

    assert_eq!(scores.len(), 3, "all 3 files within 90 days");

    // Scores must be strictly decreasing (more recent = higher score).
    let recent = scores
        .iter()
        .find(|s| s.path.file_name().unwrap_or_default() == "recent.rs")
        .expect("recent.rs");
    let middle = scores
        .iter()
        .find(|s| s.path.file_name().unwrap_or_default() == "middle.rs")
        .expect("middle.rs");
    let older = scores
        .iter()
        .find(|s| s.path.file_name().unwrap_or_default() == "older.rs")
        .expect("older.rs");

    assert!(
        recent.score > middle.score,
        "recent ({}) must outrank middle ({})",
        recent.score,
        middle.score
    );
    assert!(
        middle.score > older.score,
        "middle ({}) must outrank older ({})",
        middle.score,
        older.score
    );
    assert!(
        (recent.score - 1.0).abs() < 1e-6,
        "most recent file must have score = 1.0"
    );
}

// ============================================================================
// Risk integration tests (real git repos)
// ============================================================================

#[test]
fn risk_fix_commits_counted() {
    let dir = TempDir::new().expect("tempdir");

    build_fixture_repo(
        dir.path(),
        &[
            FixtureCommit {
                message: "feat: add app",
                changes: vec![("app.rs", "fn a() {}")],
                timestamp_override: None,
            },
            FixtureCommit {
                message: "fix: crash on startup",
                changes: vec![("app.rs", "fn a() { /* fixed */ }")],
                timestamp_override: None,
            },
            FixtureCommit {
                message: "feat: add more",
                changes: vec![("app.rs", "fn a() { /* v2 */ }")],
                timestamp_override: None,
            },
        ],
    );

    let commits = parse_history(dir.path(), 365).expect("parse_history");
    let scores = risk_scores(&commits);

    let entry = scores
        .iter()
        .find(|s| s.path.file_name().unwrap_or_default() == "app.rs")
        .expect("app.rs");

    assert_eq!(entry.total_commits, 3, "3 commits touch app.rs");
    assert_eq!(entry.fix_commits, 1, "1 fix commit");
    assert!(
        (entry.fix_density - 1.0 / 3.0).abs() < 1e-5,
        "fix_density expected ~0.333, got {}",
        entry.fix_density
    );
}

#[test]
fn risk_below_threshold_is_zero() {
    let dir = TempDir::new().expect("tempdir");

    // rare.rs touched by only 2 commits (< MIN_COMMITS_FOR_RISK = 3).
    build_fixture_repo(
        dir.path(),
        &[
            FixtureCommit {
                message: "feat: add common and rare",
                changes: vec![("common.rs", "fn a() {}"), ("rare.rs", "fn b() {}")],
                timestamp_override: None,
            },
            FixtureCommit {
                message: "fix: patch rare",
                changes: vec![("rare.rs", "fn b() { /* fixed */ }")],
                timestamp_override: None,
            },
            // Give common.rs enough commits to reach threshold,
            // so the test has a meaningful non-zero score in the set.
            FixtureCommit {
                message: "feat: update common",
                changes: vec![("common.rs", "fn a() { /* v2 */ }")],
                timestamp_override: None,
            },
            FixtureCommit {
                message: "fix: fix common",
                changes: vec![("common.rs", "fn a() { /* fixed */ }")],
                timestamp_override: None,
            },
        ],
    );

    let commits = parse_history(dir.path(), 365).expect("parse_history");
    let scores = risk_scores(&commits);

    let rare = scores
        .iter()
        .find(|s| s.path.file_name().unwrap_or_default() == "rare.rs")
        .expect("rare.rs");

    assert_eq!(
        rare.fix_density, 0.0,
        "rare.rs has only 2 commits — below threshold, density must be 0.0"
    );
}

// ============================================================================
// End-to-end test
// ============================================================================

#[test]
fn end_to_end_with_parse_history() {
    let dir = TempDir::new().expect("tempdir");
    let now = now_secs();

    // hot.rs: 3 recent commits (2 fixes) → high risk + high hotspot.
    // cold.rs: 1 commit 80 days ago → in 90d window, lower hotspot.
    build_fixture_repo(
        dir.path(),
        &[
            FixtureCommit {
                message: "feat: init hot",
                changes: vec![("hot.rs", "fn a() {}")],
                timestamp_override: Some(now as i64 - 5 * DAY_SECS as i64),
            },
            FixtureCommit {
                message: "fix: patch hot once",
                changes: vec![("hot.rs", "fn a() { /* 1 */ }")],
                timestamp_override: Some(now as i64 - 3 * DAY_SECS as i64),
            },
            FixtureCommit {
                message: "fix: patch hot twice",
                changes: vec![("hot.rs", "fn a() { /* 2 */ }")],
                timestamp_override: Some(now as i64 - DAY_SECS as i64),
            },
            FixtureCommit {
                message: "feat: add cold",
                changes: vec![("cold.rs", "fn b() {}")],
                timestamp_override: Some(now as i64 - 80 * DAY_SECS as i64),
            },
        ],
    );

    let commits = parse_history(dir.path(), 365).expect("parse_history");

    // Hotspot: hot.rs (3 recent commits) must outrank cold.rs (1 old commit).
    let hotspots = hotspot_scores(&commits, now);
    let hot_h = hotspots
        .iter()
        .find(|s| s.path.file_name().unwrap_or_default() == "hot.rs")
        .expect("hot.rs in hotspots");
    let cold_h = hotspots
        .iter()
        .find(|s| s.path.file_name().unwrap_or_default() == "cold.rs")
        .expect("cold.rs in hotspots");

    assert!(
        hot_h.score > cold_h.score,
        "hot.rs hotspot ({}) must exceed cold.rs ({})",
        hot_h.score,
        cold_h.score
    );
    assert_eq!(
        hot_h.commit_count_30d, 3,
        "hot.rs should have 3 commits in 30d"
    );
    assert_eq!(cold_h.commit_count_30d, 0, "cold.rs is outside 30d window");

    // Risk: hot.rs has 3 commits, 2 fixes → density = 0.666…
    let risks = risk_scores(&commits);
    let hot_r = risks
        .iter()
        .find(|s| s.path.file_name().unwrap_or_default() == "hot.rs")
        .expect("hot.rs in risks");

    assert_eq!(hot_r.total_commits, 3);
    assert_eq!(hot_r.fix_commits, 2);
    assert!(
        (hot_r.fix_density - 2.0 / 3.0).abs() < 1e-5,
        "fix_density expected ~0.666, got {}",
        hot_r.fix_density
    );
    assert!(
        (hot_r.score - 1.0).abs() < 1e-6,
        "hot.rs must be the riskiest file (score = 1.0)"
    );
}
