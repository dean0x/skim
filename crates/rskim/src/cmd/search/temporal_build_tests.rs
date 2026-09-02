//! Tests for temporal_build.rs
//!
//! - Unit tests on pure functions (build_cochange_rows, build_hotspot_rows,
//!   build_risk_rows) with hand-built fixtures — no git, no I/O.
//! - Integration tests that create a real git repository via subprocess
//!   (git init + git commit) and assert discriminating behaviour.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::process::Command;

use rskim_search::{CommitInfo, FileChangeInfo, FileRiskScores, FileTemporalStats, HistoryResult};
use tempfile::tempdir;

use super::super::staleness::ReanchorPolicy;
use super::{
    BuildLoudness, build_cochange_rows, build_hotspot_rows, build_risk_rows, rebuild_temporal,
    rebuild_temporal_with_source, rel_is_regular_file,
};

// ============================================================================
// Fixtures
// ============================================================================

fn make_file_change(path: &str) -> FileChangeInfo {
    FileChangeInfo {
        path: std::path::PathBuf::from(path),
        additions: 1,
        deletions: 0,
    }
}

fn make_commit(hash: &str, ts: i64, msg: &str, files: &[&str]) -> CommitInfo {
    CommitInfo {
        hash: hash.to_string(),
        timestamp: ts,
        author: "test".to_string(),
        message: msg.to_string(),
        changed_files: files.iter().map(|p| make_file_change(p)).collect(),
    }
}

fn make_history(commits: Vec<CommitInfo>) -> HistoryResult {
    let count = commits.len();
    HistoryResult {
        commits,
        metadata: rskim_search::TemporalMetadata {
            is_shallow: false,
            commit_count: count,
        },
    }
}

// ============================================================================
// AC10 — Co-change pure builder: exact Jaccard + 50-file skip
// ============================================================================

/// AC10: Co-change pure builder exact Jaccard for known input.
///
/// X and Y co-occur in k=3 of union=5 commits → Jaccard = 3/5 = 0.6.
/// Discriminating: exact numeric Jaccard within f64 epsilon.
#[test]
fn test_cochange_exact_jaccard() {
    // X: 5 commits total; Y: 5 commits total; X∧Y: 3 commits
    // union = 5 + 5 - 3 = 7? No — wait:
    //   count_x = 5 (commits touching X: 3 joint + 2 X-only)
    //   count_y = 5 (commits touching Y: 3 joint + 2 Y-only)
    //   count_xy = 3
    //   jaccard = 3 / (5 + 5 - 3) = 3/7 ≈ 0.4286
    //
    // The plan says "k of (cx + cy - k) union commits", which matches the formula.
    // Let's make cx=4, cy=4, k=3 → union = 4+4-3=5 → jaccard = 3/5 = 0.6.
    let mut commits = vec![];

    // 3 joint commits (both X and Y changed)
    for i in 0..3u32 {
        commits.push(make_commit(
            &format!("joint{i}"),
            1_000_000 + i64::from(i),
            "feat: joint",
            &["X.rs", "Y.rs"],
        ));
    }
    // 1 X-only commit
    commits.push(make_commit("xonly1", 2_000_000, "feat: x", &["X.rs"]));
    // 1 Y-only commit
    commits.push(make_commit("yonly1", 2_000_001, "feat: y", &["Y.rs"]));

    // Now: count_x=4, count_y=4, count_xy=3, union=4+4-3=5, jaccard=3/5=0.6
    let history = make_history(commits);
    let rows = build_cochange_rows(&history);

    // Must have exactly one row for (X.rs, Y.rs).
    assert_eq!(rows.len(), 1, "expected exactly 1 co-change pair");
    let row = &rows[0];
    assert_eq!(row.file_a, "X.rs");
    assert_eq!(row.file_b, "Y.rs");
    assert_eq!(row.count, 3);
    let expected_jaccard = 3.0_f64 / 5.0;
    assert!(
        (row.jaccard - expected_jaccard).abs() < 1e-9,
        "jaccard = {:.9}, expected {:.9}",
        row.jaccard,
        expected_jaccard
    );
}

/// AC10: A commit touching >50 files contributes NO pairs.
/// Discriminating: exact exclusion of the 51-file commit.
#[test]
fn test_cochange_51_file_commit_excluded_from_pairs() {
    // One commit with 51 files — must produce zero pairs.
    let files: Vec<String> = (0..51).map(|i| format!("file_{i}.rs")).collect();
    let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();

    let commits = vec![make_commit(
        "big_commit",
        1_000_000,
        "chore: reformat",
        &file_refs,
    )];
    let history = make_history(commits);
    let rows = build_cochange_rows(&history);

    assert!(
        rows.is_empty(),
        "51-file commit must produce zero co-change pairs"
    );
}

/// AC10: file_a < file_b ordering invariant holds for all emitted rows.
#[test]
fn test_cochange_file_a_less_than_file_b() {
    let commits = vec![
        make_commit("c1", 1_000_000, "feat", &["z.rs", "a.rs"]),
        make_commit("c2", 1_000_001, "feat", &["z.rs", "a.rs"]),
    ];
    let history = make_history(commits);
    let rows = build_cochange_rows(&history);

    for row in &rows {
        assert!(
            row.file_a < row.file_b,
            "file_a ({}) must be lexically less than file_b ({})",
            row.file_a,
            row.file_b
        );
    }
}

/// AC4: Sub-0.10 Jaccard pair must be excluded from emitted rows.
///
/// A appears in 10 commits, D in 10 commits, sharing exactly 1 commit →
/// Jaccard = 1/(10+10-1) = 1/19 ≈ 0.053 < 0.10.
#[test]
fn test_cochange_sub_threshold_excluded() {
    let mut commits = vec![];
    // 1 joint commit
    commits.push(make_commit("joint", 1_000_000, "feat", &["A.rs", "D.rs"]));
    // 9 A-only commits
    for i in 0..9u32 {
        commits.push(make_commit(
            &format!("a{i}"),
            1_000_001 + i64::from(i),
            "feat",
            &["A.rs"],
        ));
    }
    // 9 D-only commits
    for i in 0..9u32 {
        commits.push(make_commit(
            &format!("d{i}"),
            1_000_100 + i64::from(i),
            "feat",
            &["D.rs"],
        ));
    }
    // count_A = 10, count_D = 10, count_AD = 1
    // jaccard = 1/(10+10-1) = 1/19 ≈ 0.0526 < 0.10
    let history = make_history(commits);
    let rows = build_cochange_rows(&history);

    // No (A.rs, D.rs) pair should be present.
    let ad_row = rows.iter().find(|r| {
        (r.file_a == "A.rs" && r.file_b == "D.rs") || (r.file_a == "D.rs" && r.file_b == "A.rs")
    });
    assert!(
        ad_row.is_none(),
        "sub-threshold Jaccard ({:.4}) pair must be excluded (AC4)",
        1.0_f64 / 19.0
    );
}

// ============================================================================
// AC11 — Join correctness: hotspot and risk row field mapping
// ============================================================================

/// AC11: Joint presence — verify each field maps to the correct source.
/// Discriminating: each field individually asserted against the known fixture value.
#[test]
fn test_join_hotspot_row_field_mapping() {
    let mut risk_scores: HashMap<String, FileRiskScores> = HashMap::new();
    risk_scores.insert(
        "p.rs".to_string(),
        FileRiskScores {
            hotspot: 0.7,
            fix_density: 0.25,
        },
    );

    let mut temporal_stats: HashMap<String, FileTemporalStats> = HashMap::new();
    temporal_stats.insert(
        "p.rs".to_string(),
        FileTemporalStats {
            changes_30d: 2,
            changes_90d: 5,
            total_commits: 8,
            fix_commits: 3,
        },
    );

    let hotspot_rows = build_hotspot_rows(&risk_scores, &temporal_stats);
    assert_eq!(hotspot_rows.len(), 1);
    let row = hotspot_rows.into_iter().next().unwrap();

    assert_eq!(row.file_path, "p.rs");
    assert!(
        (row.score - 0.7).abs() < 1e-9,
        "score must come from FileRiskScores.hotspot"
    );
    assert_eq!(row.changes_30d, 2, "changes_30d from FileTemporalStats");
    assert_eq!(row.changes_90d, 5, "changes_90d from FileTemporalStats");
}

/// AC11 / AC12 (#378): Joint presence — verify risk row field mapping.
///
/// `risk_score` MUST equal the COMPUTED volume-weighted helper output
/// `risk_score_wilson_decay(decay_fix_factor, fix_commits, total_commits)`, NOT
/// the old bare decay-weighted ratio (0.375). This test MUST FAIL against the
/// pre-#378 `risk_score == 0.375` behavior. `fix_density` stays the raw ratio
/// 3/8 (AD-378-3 two-field separation: risk_score != fix_density).
#[test]
fn test_join_risk_row_field_mapping() {
    // decay_fix_factor (decay-weighted fix proportion) = 3/8.
    let decay_fix_factor = 0.375;
    let mut risk_scores: HashMap<String, FileRiskScores> = HashMap::new();
    risk_scores.insert(
        "p.rs".to_string(),
        FileRiskScores {
            hotspot: 0.7,
            fix_density: decay_fix_factor, // 3/8
        },
    );

    let mut temporal_stats: HashMap<String, FileTemporalStats> = HashMap::new();
    temporal_stats.insert(
        "p.rs".to_string(),
        FileTemporalStats {
            changes_30d: 2,
            changes_90d: 5,
            total_commits: 8,
            fix_commits: 3,
        },
    );

    let risk_rows = build_risk_rows(&risk_scores, &temporal_stats);
    assert_eq!(risk_rows.len(), 1);
    let row = risk_rows.into_iter().next().unwrap();

    assert_eq!(row.file_path, "p.rs");
    // risk_score == COMPUTED helper output (decay × Wilson-LB over raw 3/8), NOT 0.375.
    let expected_risk = rskim_search::risk_score_wilson_decay(decay_fix_factor, 3, 8);
    assert!(
        (row.risk_score - expected_risk).abs() < 1e-9,
        "risk_score must equal risk_score_wilson_decay(0.375, 3, 8) = {expected_risk:.6}, \
         got {:.6}",
        row.risk_score
    );
    // Falsifiability (AC12): the new score MUST differ from the old bare-ratio 0.375.
    assert!(
        (row.risk_score - 0.375).abs() > 1e-9,
        "risk_score must NOT be the old bare decay-weighted ratio 0.375 (#378 volume weighting)"
    );
    assert_eq!(row.total_commits, 8, "total_commits from FileTemporalStats");
    assert_eq!(row.fix_commits, 3, "fix_commits from FileTemporalStats");
    // AD-378-3: fix_density stays the raw ratio and is distinct from risk_score.
    assert!(
        (row.fix_density - 0.375).abs() < 1e-9,
        "fix_density must remain the raw ratio 3/8 (shown in Fix%)"
    );
    assert!(
        (row.fix_density - row.risk_score).abs() > 1e-9,
        "AD-378-3: risk_score and fix_density MUST be unequal (two-field separation)"
    );
}

/// AC11: Path present in only the risk_scores map → changes_30d/90d zeroed, no panic.
#[test]
fn test_join_path_only_in_risk_scores() {
    let mut risk_scores: HashMap<String, FileRiskScores> = HashMap::new();
    risk_scores.insert(
        "q.rs".to_string(),
        FileRiskScores {
            hotspot: 0.5,
            fix_density: 0.1,
        },
    );
    let temporal_stats: HashMap<String, FileTemporalStats> = HashMap::new();

    let hotspot_rows = build_hotspot_rows(&risk_scores, &temporal_stats);
    let row = hotspot_rows.into_iter().find(|r| r.file_path == "q.rs");
    assert!(row.is_some(), "path only in risk_scores must produce a row");
    let row = row.unwrap();
    assert_eq!(
        row.changes_30d, 0,
        "changes_30d zeroed when path only in risk_scores"
    );
    assert_eq!(
        row.changes_90d, 0,
        "changes_90d zeroed when path only in risk_scores"
    );
}

/// AC11: Path present in only the temporal_stats map → score zeroed, no panic.
#[test]
fn test_join_path_only_in_temporal_stats() {
    let risk_scores: HashMap<String, FileRiskScores> = HashMap::new();
    let mut temporal_stats: HashMap<String, FileTemporalStats> = HashMap::new();
    temporal_stats.insert(
        "q.rs".to_string(),
        FileTemporalStats {
            changes_30d: 1,
            changes_90d: 3,
            total_commits: 3,
            fix_commits: 0,
        },
    );

    let hotspot_rows = build_hotspot_rows(&risk_scores, &temporal_stats);
    let row = hotspot_rows.into_iter().find(|r| r.file_path == "q.rs");
    assert!(
        row.is_some(),
        "path only in temporal_stats must produce a row"
    );
    let row = row.unwrap();
    assert!(
        row.score.abs() < 1e-9,
        "score zeroed when path only in temporal_stats"
    );
    assert_eq!(row.changes_30d, 1);
    assert_eq!(row.changes_90d, 3);
}

// ============================================================================
// Integration tests requiring a real git repository
// ============================================================================

/// Initialise an empty git repository in `dir` with test identity.
fn init_git_repo(dir: &std::path::Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir)
        .output()
        .expect("git config email");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .expect("git config name");
}

/// Create a real git repository with commits.
///
/// Delegates to the canonical `staleness::create_real_git_repo` shared helper
/// (see #357 cycle-2 findings 9/14). Named identically to the counterpart in
/// staleness_tests.rs and mod.rs so a reader scanning the three test files sees
/// the same shared helper rather than three apparently-distinct helpers (#357
/// cycle-2 finding 3). The `init_git_repo` helper is kept for tests that need
/// an unborn repo (no commits).
fn create_real_git_repo(dir: &std::path::Path, commit_files: &[(&str, &[(&str, &str)])]) -> String {
    super::super::staleness::create_real_git_repo(dir, commit_files)
}

/// Extended form of [`create_real_git_repo`] that accepts an optional per-commit
/// date string (e.g. `"2020-01-01 00:00:00 +0000"`) so tests that need
/// `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` control can share the same helper path
/// as undated tests rather than hand-rolling repeated `Command::new("git")` blocks.
#[allow(clippy::type_complexity)]
fn create_real_git_repo_with_dates(
    dir: &std::path::Path,
    commit_files: &[(&str, Option<&str>, &[(&str, &str)])],
) -> String {
    super::super::staleness::create_real_git_repo_with_dates(dir, commit_files)
}

/// AC5 / AC6 — HEAD stored in temporal.db equals full 40-hex SHA and matches
/// `git rev-parse HEAD` (no false-stale warning).
///
/// Discriminating: assert_eq on full SHA bytes, and assert check_temporal_staleness
/// returns None.
#[test]
fn test_rebuild_temporal_head_full_sha_and_fresh() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    let head = create_real_git_repo(
        dir.path(),
        &[
            ("feat: first", &[("hot.rs", "fn a() {}")]),
            ("feat: second", &[("hot.rs", "fn b() {}")]),
        ],
    );
    assert_eq!(head.len(), 40, "git rev-parse HEAD must return 40-char SHA");

    let now = super::current_epoch_secs();
    rebuild_temporal(dir.path(), &cache_dir, &head, now).unwrap();

    let db_path = cache_dir.join("temporal.db");
    assert!(db_path.exists(), "temporal.db must exist after rebuild");

    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    let stored_head = db
        .get_meta(rskim_search::META_GIT_HEAD)
        .unwrap()
        .expect("META_GIT_HEAD must be set");

    assert_eq!(
        stored_head, head,
        "stored HEAD must equal the full 40-hex SHA byte-for-byte (AC5)"
    );

    // AC6: check_temporal_staleness must return None after rebuild.
    // We call it directly with the same DB and root.
    use crate::cmd::search::temporal::check_temporal_staleness;
    let stale_msg = check_temporal_staleness(&db, dir.path());
    assert!(
        stale_msg.is_none(),
        "check_temporal_staleness must return None immediately after rebuild (AC6), got: {stale_msg:?}"
    );
}

/// AC7 — Temporal failure on non-git directory does NOT fail lexical query.
///
/// Discriminating: rebuild_temporal returns Ok(()) on a non-git dir AND
/// temporal.db IS created with META_GIT_HEAD set (Finding 2 / D5 backoff:
/// parse_history failure falls through with empty rows so META_GIT_HEAD is
/// written, preventing temporal_db_is_stale from returning true on every
/// subsequent query and triggering an infinite rebuild retry loop).
#[test]
fn test_rebuild_temporal_nongit_returns_ok() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // No git repo here — GixSource::parse_history will fail.
    let fake_head = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let now = super::current_epoch_secs();
    let result = rebuild_temporal(dir.path(), &cache_dir, fake_head, now);

    assert!(
        result.is_ok(),
        "rebuild_temporal must return Ok(()) on non-git directory (AC7), got: {result:?}"
    );
    // temporal.db MUST be created even when parse_history fails: the empty-row
    // fall-through writes META_GIT_HEAD so temporal_db_is_stale returns false
    // on subsequent queries, breaking the per-query rebuild retry loop.
    let db_path = cache_dir.join("temporal.db");
    assert!(
        db_path.exists(),
        "temporal.db must be created on non-git root to prevent retry loop (Finding 2)"
    );
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    let stored = db
        .get_meta(rskim_search::META_GIT_HEAD)
        .unwrap()
        .expect("META_GIT_HEAD must be set even when parse_history failed");
    assert_eq!(
        stored, fake_head,
        "META_GIT_HEAD must equal the passed head on non-git root"
    );
    assert!(
        db.top_hotspots(20).unwrap().is_empty(),
        "temporal.db on non-git root must have zero hotspot rows"
    );
    // The backoff sentinel must NOT be written — parse_history failure is handled
    // via the empty-row fall-through, not the sentinel path.
    assert!(
        !cache_dir.join("temporal.db.build_backoff").exists(),
        "backoff sentinel must not be written when parse_history fall-through succeeds"
    );
}

/// AC1 / AC2 — After auto-refresh on a git repo, top_hotspots and top_risks
/// are non-empty and ordered correctly.
///
/// Discriminating:
/// - AC1: hot.rs (5 recent commits) ranks above cold.rs (1 old commit).
/// - AC2: buggy.rs (fix commits) has strictly higher risk_score than clean.rs.
#[test]
fn test_rebuild_temporal_hot_and_risky_ordering() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Build a repo where hot.rs has many recent commits and cold.rs has one old commit.
    // buggy.rs has fix commits; clean.rs has none.
    let head = create_real_git_repo(
        dir.path(),
        &[
            ("feat: cold", &[("cold.rs", "// cold")]),
            ("feat: hot 1", &[("hot.rs", "// 1")]),
            ("feat: hot 2", &[("hot.rs", "// 2")]),
            ("feat: hot 3", &[("hot.rs", "// 3")]),
            ("feat: hot 4", &[("hot.rs", "// 4")]),
            ("feat: hot 5", &[("hot.rs", "// 5")]),
            ("feat: clean 1", &[("clean.rs", "// a")]),
            ("feat: clean 2", &[("clean.rs", "// b")]),
            ("feat: clean 3", &[("clean.rs", "// c")]),
            ("fix: buggy 1", &[("buggy.rs", "// fix1")]),
            ("fix: buggy 2", &[("buggy.rs", "// fix2")]),
            ("fix: buggy 3", &[("buggy.rs", "// fix3")]),
            ("feat: buggy 4", &[("buggy.rs", "// nf")]),
        ],
    );

    let now = super::current_epoch_secs();
    rebuild_temporal(dir.path(), &cache_dir, &head, now).unwrap();

    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();

    // AC1: hot.rs must rank at position 0 in top_hotspots.
    let hotspots = db.top_hotspots(20).unwrap();
    assert!(!hotspots.is_empty(), "hotspot list must be non-empty (AC1)");
    assert_eq!(
        hotspots[0].file_path, "hot.rs",
        "hot.rs must rank first in hotspots (AC1)"
    );

    // AC2: buggy.rs risk_score > clean.rs risk_score.
    let risks = db.top_risks(20).unwrap();
    let buggy = risks.iter().find(|r| r.file_path == "buggy.rs");
    let clean = risks.iter().find(|r| r.file_path == "clean.rs");
    assert!(buggy.is_some(), "buggy.rs must appear in risk list (AC2)");
    assert!(clean.is_some(), "clean.rs must appear in risk list (AC2)");
    assert!(
        buggy.unwrap().risk_score > clean.unwrap().risk_score,
        "buggy.rs risk_score ({:.4}) must exceed clean.rs risk_score ({:.4}) (AC2)",
        buggy.unwrap().risk_score,
        clean.unwrap().risk_score,
    );
}

/// AC3 — blast-radius returns correct co-change partner and excludes non-partner.
///
/// A and B co-change in 4 of their commits; C never co-changes with A.
/// Discriminating: B present AND C absent in cochanges_for_file("A.rs").
#[test]
fn test_rebuild_temporal_blast_radius_partner() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // A and B together in 4 commits; C only in its own commits.
    let head = create_real_git_repo(
        dir.path(),
        &[
            ("feat: ab1", &[("A.rs", "// a1"), ("B.rs", "// b1")]),
            ("feat: ab2", &[("A.rs", "// a2"), ("B.rs", "// b2")]),
            ("feat: ab3", &[("A.rs", "// a3"), ("B.rs", "// b3")]),
            ("feat: ab4", &[("A.rs", "// a4"), ("B.rs", "// b4")]),
            ("feat: a5", &[("A.rs", "// a5")]),
            ("feat: c1", &[("C.rs", "// c1")]),
            ("feat: c2", &[("C.rs", "// c2")]),
        ],
    );

    let now = super::current_epoch_secs();
    rebuild_temporal(dir.path(), &cache_dir, &head, now).unwrap();

    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();

    let partners = db.cochanges_for_file("A.rs").unwrap();
    let partner_paths: std::collections::HashSet<String> = partners
        .iter()
        .map(|p| {
            if p.file_a == "A.rs" {
                p.file_b.clone()
            } else {
                p.file_a.clone()
            }
        })
        .collect();

    assert!(
        partner_paths.contains("B.rs"),
        "B.rs must be a co-change partner of A.rs (AC3)"
    );
    assert!(
        !partner_paths.contains("C.rs"),
        "C.rs must NOT be a co-change partner of A.rs (AC3)"
    );
}

/// AC13 — window bucketing: a single full-history walk's commits are bucketed
/// into the 30d/90d windows by timestamp. This is NOT a 90-day walk *cutoff* —
/// the walk covers all history; windowing happens downstream (see the
/// implementation note below).
///
/// Two recent commits (within the windows) and two old commits (set via commit
/// date manipulation — we use the git committer date env var).
/// Discriminating: changes_90d == 2 (only in-window), not 4.
///
/// # Implementation note (Decision O-B)
///
/// After the fix that removed the dead 90-day hotspot walk, `rebuild_temporal`
/// now performs a single full-history walk and delegates windowing to
/// `compute_file_temporal_stats` via timestamp comparison against `now_epoch`.
/// This test remains discriminating because it verifies that the windowed
/// field (`changes_90d`) is correctly computed from timestamps — changing
/// `now_epoch` or the commit dates changes the result.  The prior version of
/// this test was non-discriminating because it asserted `changes_90d` produced
/// by `compute_file_temporal_stats` (timestamp-based windowing) while the
/// 90-day `parse_history` walk being tested was only used for an `is_empty()`
/// guard.  Now the single walk feeds all computation, so the test correctly
/// exercises the full data path.
#[test]
fn test_rebuild_temporal_window_bucketing() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    init_git_repo(dir.path());

    // Two old commits outside the 90-day window.
    // now_epoch is pinned below to 1_781_337_600 = 2026-06-13 08:00:00 UTC;
    // 90 days prior ≈ 2026-03-15, so 2025-10-01 is well outside the window.
    let old_git_date = "2025-10-01 00:00:00 +0000";

    for i in 0..2u32 {
        std::fs::write(dir.path().join("file.rs"), format!("// old {i}")).unwrap();
        Command::new("git")
            .args(["add", "file.rs"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", &format!("old {i}")])
            .current_dir(dir.path())
            .env("GIT_AUTHOR_DATE", old_git_date)
            .env("GIT_COMMITTER_DATE", old_git_date)
            .output()
            .unwrap();
    }

    // Two recent commits (within the last 90 days — today).
    let recent_git_date = "2026-06-15 00:00:00 +0000";
    for i in 0..2u32 {
        std::fs::write(dir.path().join("file.rs"), format!("// recent {i}")).unwrap();
        Command::new("git")
            .args(["add", "file.rs"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", &format!("recent {i}")])
            .current_dir(dir.path())
            .env("GIT_AUTHOR_DATE", recent_git_date)
            .env("GIT_COMMITTER_DATE", recent_git_date)
            .output()
            .unwrap();
    }

    let head_out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let head = String::from_utf8_lossy(&head_out.stdout).trim().to_string();

    // Pin now_epoch so the windowed counts are deterministic regardless of the
    // wall clock. 1_781_337_600 = 2026-06-13 08:00:00 UTC. The recent commits are
    // dated 2026-06-15 (slightly AFTER now_epoch); `compute_file_temporal_stats`
    // treats future commits as elapsed = 0, so they still fall inside both windows.
    let now_epoch: u64 = 1_781_337_600;

    rebuild_temporal(dir.path(), &cache_dir, &head, now_epoch).unwrap();

    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();

    let hotspot = db
        .hotspot_for_file("file.rs")
        .unwrap()
        .expect("file.rs must have a hotspot row");

    assert_eq!(
        hotspot.changes_90d, 2,
        "changes_90d must be 2 (only in-window commits counted), got {} (AC13)",
        hotspot.changes_90d
    );
    // The recent commits are dated after now_epoch → treated as elapsed = 0 →
    // inside the 30d window; the old commits (2025-10-01) are far outside it.
    assert_eq!(
        hotspot.changes_30d, 2,
        "changes_30d must be 2 (recent commits are within 30d of now_epoch), got {}",
        hotspot.changes_30d
    );
}

// ============================================================================
// O-C / ADR-003 — Full-history risk stats correctness
// ============================================================================

/// O-C: total_commits must count commits outside the 90-day window.
///
/// A file has 2 old commits (well outside the 90-day window from now_epoch)
/// and 1 recent commit. The risk row must report total_commits = 3, not 1.
/// This tests that rebuild_temporal feeds the full-history walk to
/// compute_file_temporal_stats (not just the 90-day hotspot walk).
///
/// Discriminating: total_commits == 3 (not == 1 if windowed).
#[test]
fn test_risk_row_total_commits_includes_out_of_window_commits() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    init_git_repo(dir.path());

    // now_epoch pinned to 2026-06-13 08:00:00 UTC.
    let now_epoch: u64 = 1_781_337_600;

    // Two old commits well outside the 90-day window (2024-01-01 is ~890 days ago).
    let old_date = "2024-01-01 00:00:00 +0000";
    for i in 0..2u32 {
        std::fs::write(dir.path().join("tracked.rs"), format!("// old {i}")).unwrap();
        Command::new("git")
            .args(["add", "tracked.rs"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", &format!("fix: old fix {i}")])
            .current_dir(dir.path())
            .env("GIT_AUTHOR_DATE", old_date)
            .env("GIT_COMMITTER_DATE", old_date)
            .output()
            .unwrap();
    }

    // One recent commit inside the 90-day window.
    let recent_date = "2026-06-01 00:00:00 +0000";
    std::fs::write(dir.path().join("tracked.rs"), "// recent").unwrap();
    Command::new("git")
        .args(["add", "tracked.rs"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "feat: recent"])
        .current_dir(dir.path())
        .env("GIT_AUTHOR_DATE", recent_date)
        .env("GIT_COMMITTER_DATE", recent_date)
        .output()
        .unwrap();

    let head_out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let head = String::from_utf8_lossy(&head_out.stdout).trim().to_string();

    rebuild_temporal(dir.path(), &cache_dir, &head, now_epoch).unwrap();

    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    let risk = db
        .risk_for_file("tracked.rs")
        .unwrap()
        .expect("tracked.rs must have a risk row");

    assert_eq!(
        risk.total_commits, 3,
        "total_commits must count ALL commits including those >90 days ago (O-C / ADR-003), \
         got {} (regression: windowed 90-day walk used instead of full history)",
        risk.total_commits
    );
    // 2 of the 3 commits are fix commits (the old ones have "fix:" prefix).
    assert_eq!(
        risk.fix_commits, 2,
        "fix_commits must count fix commits across full history, got {}",
        risk.fix_commits
    );
}

// ============================================================================
// fix_density contract — raw ratio, not decay-weighted
// ============================================================================

/// RiskRow.fix_density must equal fix_commits/total_commits (raw ratio).
///
/// build_risk_rows previously set fix_density to FileRiskScores.fix_density
/// (decay-weighted), which violated the schema contract in storage_types.rs
/// ("ratio of fix commits to total commits"). Discriminating: fix_density must
/// equal the exact fraction fix_commits/total_commits from the stats, not the
/// decay-weighted ratio.
#[test]
fn test_risk_row_fix_density_is_raw_ratio() {
    // Hand-built maps — no I/O, no git.
    let mut risk_scores: HashMap<String, rskim_search::FileRiskScores> = HashMap::new();
    risk_scores.insert(
        "p.rs".to_string(),
        rskim_search::FileRiskScores {
            hotspot: 0.8,
            // Decay-weighted fix_density — deliberately different from raw ratio.
            fix_density: 0.9,
        },
    );

    let mut temporal_stats: HashMap<String, rskim_search::FileTemporalStats> = HashMap::new();
    temporal_stats.insert(
        "p.rs".to_string(),
        rskim_search::FileTemporalStats {
            changes_30d: 1,
            changes_90d: 2,
            total_commits: 8,
            fix_commits: 2, // raw ratio = 2/8 = 0.25
        },
    );

    let rows = build_risk_rows(&risk_scores, &temporal_stats);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];

    // fix_density must be the raw ratio (2/8 = 0.25), NOT the decay-weighted 0.9.
    let expected_raw = 2.0_f64 / 8.0;
    assert!(
        (row.fix_density - expected_raw).abs() < 1e-9,
        "fix_density must be raw fix_commits/total_commits ({:.4}), got {:.4} \
         (should NOT be the decay-weighted FileRiskScores.fix_density = 0.9)",
        expected_raw,
        row.fix_density
    );
    // risk_score must be the #378 volume-weighted value: decay_fix_factor (0.9) ×
    // Wilson-LB over the RAW counts (2/8) — NOT the bare decay-weighted 0.9.
    let expected_risk = rskim_search::risk_score_wilson_decay(0.9, 2, 8);
    assert!(
        (row.risk_score - expected_risk).abs() < 1e-9,
        "risk_score must be risk_score_wilson_decay(0.9, 2, 8) = {expected_risk:.6}, got {:.6}",
        row.risk_score
    );
    assert!(
        (row.risk_score - 0.9).abs() > 1e-9,
        "risk_score must NOT be the old bare decay-weighted 0.9 (#378 volume weighting)"
    );
}

// ============================================================================
// AC4 discriminating — sub-threshold pairs absent in the DB
// ============================================================================

/// AC4: After rebuild, sub-threshold Jaccard pair must NOT exist in the DB.
///
/// This is the discriminating complement of test_cochange_sub_threshold_excluded
/// (which tests the pure builder). This test exercises the full rebuild path
/// and verifies the DB contains no (A.rs, D.rs) row after a rebuild with a
/// sub-threshold pair.
#[test]
fn test_rebuild_temporal_sub_threshold_pair_not_in_db() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // A and D co-change in 1 of 10 commits each.
    // Jaccard = 1/(10+10-1) = 1/19 ≈ 0.053 < 0.10 — must be filtered.
    // Build the commits directly via shell so we don't need to fight lifetime
    // constraints on format!() temporaries in a Vec<(&str, Vec<(&str, &str)>)>.
    init_git_repo(dir.path());

    // 1 joint commit.
    std::fs::write(dir.path().join("A.rs"), "// a").unwrap();
    std::fs::write(dir.path().join("D.rs"), "// d").unwrap();
    Command::new("git")
        .args(["add", "A.rs", "D.rs"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "feat: joint"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // 9 A-only commits.
    for i in 1..10u32 {
        std::fs::write(dir.path().join("A.rs"), format!("// a{i}")).unwrap();
        Command::new("git")
            .args(["add", "A.rs"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "feat: a"])
            .current_dir(dir.path())
            .output()
            .unwrap();
    }

    // 9 D-only commits.
    for i in 1..10u32 {
        std::fs::write(dir.path().join("D.rs"), format!("// d{i}")).unwrap();
        Command::new("git")
            .args(["add", "D.rs"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "feat: d"])
            .current_dir(dir.path())
            .output()
            .unwrap();
    }

    let head_out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let head = String::from_utf8_lossy(&head_out.stdout).trim().to_string();

    let now = super::current_epoch_secs();
    rebuild_temporal(dir.path(), &cache_dir, &head, now).unwrap();

    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();

    // The sub-threshold (A.rs, D.rs) pair must NOT appear in the DB.
    let partners_for_a = db.cochanges_for_file("A.rs").unwrap();
    let ad_in_db = partners_for_a.iter().any(|p| {
        (p.file_a == "A.rs" && p.file_b == "D.rs") || (p.file_a == "D.rs" && p.file_b == "A.rs")
    });
    assert!(
        !ad_in_db,
        "sub-threshold (A.rs, D.rs) pair (Jaccard ≈ 0.053) must NOT be in the DB (AC4 DB layer)"
    );
}

// ============================================================================
// AC12 — CapacityExceeded leaves prior DB rows intact
// ============================================================================

/// Second-run stability: rebuild_temporal on the same repo twice does not corrupt
/// the temporal DB or lose the stored HEAD.
///
/// This test covers the "happy path idempotency" invariant: two successive rebuilds
/// on the same 1-commit repo produce a valid DB with META_GIT_HEAD set both times.
///
/// # Scope (not AC12)
///
/// True AC12 (CapacityExceeded leaves prior DB rows intact) requires >500k rows,
/// which is impractical to simulate in a unit test. The CapacityExceeded arm is
/// integration-tested at the storage layer in
/// `rskim-search/src/temporal/storage_tests.rs`. This test only verifies
/// normal-operation DB stability; it does NOT exercise CapacityExceeded.
#[test]
fn test_rebuild_temporal_second_run_preserves_prior_head() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    let head = create_real_git_repo(dir.path(), &[("feat: first", &[("main.rs", "fn a() {}")])]);
    assert_eq!(head.len(), 40, "git rev-parse must produce a 40-char SHA");

    let now = super::current_epoch_secs();
    // First successful rebuild — seeds the DB.
    rebuild_temporal(dir.path(), &cache_dir, &head, now).unwrap();

    let db_path = cache_dir.join("temporal.db");
    assert!(
        db_path.exists(),
        "temporal.db must be created after first rebuild"
    );

    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    let stored_head = db
        .get_meta(rskim_search::META_GIT_HEAD)
        .unwrap()
        .expect("META_GIT_HEAD must be set after first rebuild");
    assert_eq!(
        stored_head, head,
        "META_GIT_HEAD must equal the passed HEAD"
    );

    // Second rebuild with same head — DB must not be corrupted.
    rebuild_temporal(dir.path(), &cache_dir, &head, now).unwrap();

    let db2 = rskim_search::TemporalDb::open(&db_path).unwrap();
    let stored_head2 = db2
        .get_meta(rskim_search::META_GIT_HEAD)
        .unwrap()
        .expect("META_GIT_HEAD must still be set after second rebuild");
    assert_eq!(
        stored_head2, head,
        "META_GIT_HEAD must be preserved (or updated) after second rebuild (AC12)"
    );
}

// ============================================================================
// B3b — E2E mutual exclusion: rebuild_temporal waits for the same lock
// ============================================================================

/// Prove that `rebuild_temporal` acquires the same `{cache_dir}/.skim-build.lock`
/// used by `build_index`, so both build paths are mutually exclusive.
///
/// Requires git. If git is unavailable the test prints "SKIP: ..." and returns.
///
/// The test acquires the advisory lock directly, spawns a worker thread that
/// calls `rebuild_temporal` on a real git repo, holds the lock ~300 ms, then
/// records `t_release` and releases. We assert:
///   - the worker succeeds (Ok), AND
///   - it completed AFTER `t_release` (t_complete >= t_release).
///
/// A 30-second `recv_timeout` ensures the test never hangs.
#[test]
fn e2e_rebuild_temporal_waits_for_same_lock() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::current_epoch_secs;
    use super::rebuild_temporal;

    // ── git availability guard ────────────────────────────────────────────────
    let init_check = Command::new("git").arg("--version").output();
    if init_check.map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("SKIP e2e_rebuild_temporal_waits_for_same_lock: git not available");
        return;
    }

    // ── set up a real git repo ────────────────────────────────────────────────
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    let head = create_real_git_repo(
        dir.path(),
        &[
            ("feat: first", &[("hot.rs", "fn a() {}")]),
            ("feat: second", &[("hot.rs", "fn b() {}")]),
        ],
    );

    if head.is_empty() {
        eprintln!(
            "SKIP e2e_rebuild_temporal_waits_for_same_lock: git commit failed (no identity?)"
        );
        return;
    }

    // ── acquire the SAME advisory lock that rebuild_temporal will use ─────────
    let lock_holder = super::super::build_lock::acquire("e2e-holder-temporal", &cache_dir)
        .expect("must acquire lock");

    // Channel: worker sends (is_ok, t_start, t_complete) after rebuild_temporal returns.
    // t_start (the worker's first action) brackets the lower bound — the worker
    // was alive and inside rebuild_temporal before the lock was released — and
    // t_complete the upper bound, together proving it blocked on the lock.
    let (tx, rx) = mpsc::channel::<(bool, Instant, Instant)>();

    let root_path = dir.path().to_path_buf();
    let cache_path = cache_dir.clone();
    let head_clone = head.clone();
    let worker = std::thread::spawn(move || {
        let t_start = Instant::now();
        let now = current_epoch_secs();
        let result = rebuild_temporal(&root_path, &cache_path, &head_clone, now);
        let t_complete = Instant::now();
        tx.send((result.is_ok(), t_start, t_complete)).ok();
    });

    // Hold the lock for ~300 ms, then record t_release and drop.
    std::thread::sleep(Duration::from_millis(300));
    let t_release = Instant::now();
    drop(lock_holder);

    // Wait up to 30 s for the worker.
    let (is_ok, t_start, t_complete) = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("worker did not complete within 30 s");

    worker.join().expect("worker thread panicked");

    assert!(
        is_ok,
        "rebuild_temporal must succeed after lock is released"
    );
    // Lower bracket: the worker entered rebuild_temporal before the ~300 ms hold ended.
    assert!(
        t_start < t_release,
        "worker must have entered rebuild_temporal BEFORE the lock was released \
         (t_start={t_start:?}, t_release={t_release:?})"
    );
    // Upper bracket: it could not finish until the lock was released.
    assert!(
        t_complete >= t_release,
        "rebuild_temporal must complete AFTER the lock was released — \
         proving it contends on the same .skim-build.lock as build_index \
         (t_complete={t_complete:?}, t_release={t_release:?})"
    );
}

// ============================================================================
// Degenerate git repo — empty history stability (LOCKED DECISION 2026-06-24)
// ============================================================================

/// API CONTRACT (degenerate git repo no-loop): When a TemporalSource returns
/// an empty commit list (zero-history repo), rebuild_temporal_with_source must
/// write a present-but-empty temporal.db with META_GIT_HEAD so that subsequent
/// calls see the repo as Current and skip rebuild — preventing the per-query
/// no-op loop the locked decision was written to prevent.
///
/// Without the fix, rebuild_temporal returned via warn_skip! BEFORE writing
/// temporal.db on the empty-history path. temporal_db_is_stale then returned
/// true on every subsequent query (no temporal.db → stale → rebuild → still
/// no temporal.db → stale again...), triggering a per-query history walk.
///
/// Uses CountingSource::new_empty() (returns empty HistoryResult immediately)
/// rather than a real git repo because a fake detached SHA in .git/HEAD fails
/// gix object resolution (NOT an unborn error — unborn == symbolic ref to
/// non-existent branch). A real unborn git repo (git init, no commits) is
/// handled by GixSource in production; here we test the empty-history branch
/// in rebuild_temporal_with_source in isolation.
///
/// Discriminating: both calls return Ok + temporal.db STABLE across calls
/// (present-and-empty after first, mtime-unchanged after second).
/// META_GIT_HEAD must be set so temporal_db_is_stale returns false.
#[test]
fn test_degenerate_repo_empty_history_writes_empty_temporal_db() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // CountingSource::new_empty() returns Ok(HistoryResult { commits: [] })
    // directly — simulating what GixSource returns for a real unborn repo.
    let src = CountingSource::new_empty();
    let fake_head = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";
    let now = super::current_epoch_secs();

    // First call: empty history — must write present-but-empty temporal.db.
    let result1 = rebuild_temporal_with_source(
        &src,
        dir.path(),
        &cache_dir,
        fake_head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    );
    assert!(
        result1.is_ok(),
        "rebuild_temporal_with_source on empty-history repo must return Ok (non-fatal), got: {result1:?}"
    );

    let db_path = cache_dir.join("temporal.db");
    assert!(
        db_path.exists(),
        "temporal.db must be written even when git history is empty \
         (LOCKED DECISION 2026-06-24: present-but-empty prevents per-query no-op loop)"
    );

    // META_GIT_HEAD must be set so temporal_db_is_stale returns false next call.
    {
        let db = rskim_search::TemporalDb::open(&db_path).unwrap();
        let stored = db
            .get_meta(rskim_search::META_GIT_HEAD)
            .unwrap()
            .expect("META_GIT_HEAD must be set in empty temporal.db");
        assert_eq!(
            stored, fake_head,
            "META_GIT_HEAD must equal the passed head even when history is empty"
        );
        let hotspots = db.top_hotspots(20).unwrap();
        assert!(
            hotspots.is_empty(),
            "empty-history temporal.db must have zero hotspot rows"
        );
    }

    // Second call: temporal.db exists with matching HEAD — stability check.
    // This is the core no-loop guard: if temporal_db_is_stale is false, the
    // caller (auto_refresh_if_stale) will NOT call rebuild_temporal again.
    // We call rebuild_temporal_with_source a second time to confirm idempotency.
    let src2 = CountingSource::new_empty();
    let result2 = rebuild_temporal_with_source(
        &src2,
        dir.path(),
        &cache_dir,
        fake_head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    );
    assert!(
        result2.is_ok(),
        "second rebuild_temporal_with_source on empty-history repo must return Ok, got: {result2:?}"
    );

    // Discriminating: temporal.db state STABLE — still present, still empty, HEAD unchanged.
    assert!(
        db_path.exists(),
        "temporal.db must still exist after second call on empty-history repo"
    );
    {
        let db = rskim_search::TemporalDb::open(&db_path).unwrap();
        let stored2 = db
            .get_meta(rskim_search::META_GIT_HEAD)
            .unwrap()
            .expect("META_GIT_HEAD must remain set after second call");
        assert_eq!(
            stored2, fake_head,
            "META_GIT_HEAD must be stable across two calls on empty-history repo"
        );
    }

    // The no-loop guard: META_GIT_HEAD is set and matches, so temporal_db_is_stale
    // returns false on subsequent queries — preventing the per-query history walk.
    // (The direct temporal_db_is_stale assertion is in staleness_tests.rs since
    // that function is pub(super) within the staleness module.)
}

// ============================================================================
// PERFORMANCE (ADR-003): parse_history called exactly once during rebuild
// ============================================================================

/// A counting `TemporalSource` test double that records how many times
/// `parse_history` was invoked.
///
/// Used by `test_rebuild_temporal_parse_history_called_once` to assert ADR-003's
/// grounded regression guard: a single history walk per rebuild, not two
/// (the prior implementation had a dead second 90-day walk).
struct CountingSource {
    call_count: std::sync::atomic::AtomicUsize,
    /// Delegate to GixSource for real git repo integration, or return empty history.
    use_real_git: bool,
}

impl CountingSource {
    fn new_empty() -> Self {
        Self {
            call_count: std::sync::atomic::AtomicUsize::new(0),
            use_real_git: false,
        }
    }

    fn new_real() -> Self {
        Self {
            call_count: std::sync::atomic::AtomicUsize::new(0),
            use_real_git: true,
        }
    }

    fn count(&self) -> usize {
        self.call_count.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl rskim_search::TemporalSource for CountingSource {
    fn parse_history(
        &self,
        repo_path: &std::path::Path,
        lookback_days: u32,
    ) -> rskim_search::Result<rskim_search::HistoryResult> {
        self.call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.use_real_git {
            rskim_search::GixSource.parse_history(repo_path, lookback_days)
        } else {
            Ok(rskim_search::HistoryResult {
                commits: vec![],
                metadata: rskim_search::TemporalMetadata {
                    is_shallow: false,
                    commit_count: 0,
                },
            })
        }
    }
}

/// A `TemporalSource` test double that always returns a `SearchError::Git` error.
///
/// Used to simulate the exposure widened by #413: `resolve_repo_toplevel` (naive
/// `.git`-exists walk) adopts roots that `gix::discover` (respects filesystem
/// boundaries) refuses, so HEAD resolves but `parse_history` fails.
struct FailingSource;

impl rskim_search::TemporalSource for FailingSource {
    fn parse_history(
        &self,
        _repo_path: &std::path::Path,
        _lookback_days: u32,
    ) -> rskim_search::Result<rskim_search::HistoryResult> {
        Err(rskim_search::SearchError::Git(
            "simulated parse_history failure (#413 test)".to_string(),
        ))
    }
}

/// API CONTRACT (parse_history failure no-loop): When `TemporalSource::parse_history`
/// returns an error, `rebuild_temporal_with_source` must fall through with empty rows
/// and write a present-but-empty `temporal.db` with `META_GIT_HEAD` set — preventing
/// the per-query rebuild retry loop that would otherwise occur because
/// `temporal_db_is_stale` returns `true` whenever `META_GIT_HEAD` is absent.
///
/// This is the primary exposure widened by #413: `resolve_repo_toplevel` (naive
/// `.git`-exists ancestor walk) adopts roots that `gix::discover` (respects
/// filesystem boundaries and ceiling directories) refuses.  Before this fix, a
/// bare `warn_skip!` returned `Ok(())` before `TemporalDb::open`, leaving
/// `temporal.db` absent so `temporal_db_is_stale` fired on every subsequent query
/// and the full-history walk was re-attempted forever.
///
/// Discriminating: `META_GIT_HEAD` is set so `temporal_db_is_stale` returns
/// `false` after the call.
#[test]
fn test_parse_history_failure_writes_meta_head_to_prevent_retry_loop() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    let src = FailingSource;
    let fake_head = "cccc2222cccc2222cccc2222cccc2222cccc2222";
    let now = super::current_epoch_secs();

    let result = rebuild_temporal_with_source(
        &src,
        dir.path(),
        &cache_dir,
        fake_head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    );
    assert!(
        result.is_ok(),
        "rebuild_temporal_with_source must return Ok(()) when parse_history fails (D5), got: {result:?}"
    );

    // temporal.db MUST be written with META_GIT_HEAD so temporal_db_is_stale
    // returns false on the next query — no retry loop.
    let db_path = cache_dir.join("temporal.db");
    assert!(
        db_path.exists(),
        "temporal.db must be created even when parse_history fails (Finding 2 / D5 backoff)"
    );
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    let stored = db
        .get_meta(rskim_search::META_GIT_HEAD)
        .unwrap()
        .expect("META_GIT_HEAD must be set so temporal_db_is_stale returns false");
    assert_eq!(
        stored, fake_head,
        "META_GIT_HEAD must equal the passed head even when parse_history failed"
    );
    assert!(
        db.top_hotspots(20).unwrap().is_empty(),
        "temporal.db written after parse_history failure must have zero hotspot rows"
    );

    // The backoff sentinel must NOT be written — the empty-row fall-through
    // writes META_GIT_HEAD directly, making the sentinel unnecessary.
    assert!(
        !cache_dir.join("temporal.db.build_backoff").exists(),
        "backoff sentinel must not be written when the empty-row fall-through succeeds"
    );

    // Idempotency: second call sees META_GIT_HEAD matches — no retry.
    // (In production this is checked by temporal_db_is_stale, not by a second
    // rebuild_temporal_with_source call; we verify stability here.)
    let src2 = FailingSource;
    let result2 = rebuild_temporal_with_source(
        &src2,
        dir.path(),
        &cache_dir,
        fake_head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    );
    assert!(
        result2.is_ok(),
        "second rebuild_temporal_with_source after parse_history failure must return Ok, got: {result2:?}"
    );
    // DB still present and HEAD unchanged (idempotent).
    assert!(db_path.exists());
    {
        let db2 = rskim_search::TemporalDb::open(&db_path).unwrap();
        let stored2 = db2.get_meta(rskim_search::META_GIT_HEAD).unwrap().unwrap();
        assert_eq!(
            stored2, fake_head,
            "META_GIT_HEAD must be stable across calls"
        );
    }
}

/// A `TemporalSource` test double that returns a fixed, pre-built `HistoryResult`
/// on every call.
///
/// Used by the AD-413-17 scope-filter tests to inject deterministic commit data
/// without relying on gix or a real git repo's history — decoupling the scope-filter
/// logic under test from the history-parsing layer.
struct FixedSource {
    history: rskim_search::HistoryResult,
}

impl rskim_search::TemporalSource for FixedSource {
    fn parse_history(
        &self,
        _repo_path: &std::path::Path,
        _lookback_days: u32,
    ) -> rskim_search::Result<rskim_search::HistoryResult> {
        Ok(self.history.clone())
    }
}

/// PERFORMANCE (ADR-003): parse_history is invoked exactly ONCE during a
/// rebuild_temporal_with_source call on a real git repo.
///
/// The pre-fix implementation had a dead second 90-day `parse_history` walk
/// (Decision O-B). After its removal, a single full-history walk supplies all
/// data. This test asserts call_count == 1 — the grounded regression guard
/// required by ADR-003 for the PERFORMANCE acceptance criterion.
///
/// Discriminating: if a second parse_history call is added anywhere in
/// rebuild_temporal_with_source, the count becomes 2 and this test fails.
#[test]
fn test_rebuild_temporal_parse_history_called_once() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Build a real git repo so parse_history returns non-empty history.
    let head = create_real_git_repo(
        dir.path(),
        &[
            ("feat: first", &[("src/lib.rs", "pub fn a() {}")]),
            ("feat: second", &[("src/main.rs", "fn main() {}")]),
        ],
    );
    assert_eq!(head.len(), 40, "HEAD must be a 40-char SHA");

    let src = CountingSource::new_real();
    let now = super::current_epoch_secs();

    let result = rebuild_temporal_with_source(
        &src,
        dir.path(),
        &cache_dir,
        &head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    );
    assert!(
        result.is_ok(),
        "rebuild_temporal_with_source must succeed on a real git repo, got: {result:?}"
    );

    // ADR-003 grounded regression guard: exactly ONE parse_history call.
    assert_eq!(
        src.count(),
        1,
        "parse_history must be called exactly ONCE during rebuild (ADR-003 PERFORMANCE guard); \
         got {} calls — a second call indicates a dead extra walk was reintroduced",
        src.count()
    );

    // Confirm temporal.db was actually populated (not an empty-history no-op).
    let db_path = cache_dir.join("temporal.db");
    assert!(db_path.exists(), "temporal.db must exist after rebuild");
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    let hotspots = db.top_hotspots(20).unwrap();
    assert!(
        !hotspots.is_empty(),
        "temporal.db must contain hotspot data (parse_history returned real commits)"
    );
}

// ============================================================================
// AC1 / AC2 — Build-time ghost filter: committed-deleted files absent from DB
// ============================================================================

/// AC1: After rebuild, a file present in git history but deleted from disk is
/// absent from top_hotspots, top_risks, and top_coldspots.  A present file is
/// still there.  Subset assertion — fails if the retain passes are removed.
///
/// AC2 (cochange both-sides rule): Both directions of the filter are tested —
/// - DROP: the (keep.rs, gone.rs) row is dropped because gone.rs is a ghost.
/// - RETAIN: the (keep.rs, peer.rs) row is retained because both files are
///   present on disk.  The retain assertion is the positive anchor that would
///   catch an over-zealous filter that drops all cochange rows.
///
/// Fixture: `peer.rs` co-changes with `keep.rs` in both joint commits.
/// Jaccard(keep, peer) = 2/(3+2-2) = 2/3 ≈ 0.67 — well above threshold.
///
/// The ghost is created via `fs::remove_file` after committing (leaving the
/// file in git history but off disk) — the `create_real_git_repo` helper does
/// not support git-rm, so we delete the file directly.
///
/// PF-007 discriminating: the subset assertion fails if the ghost filter is
/// removed (gone.rs would reappear in the output).
#[test]
fn test_ghost_filter_deleted_file_absent_from_all_tables() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Commit keep.rs, gone.rs, and peer.rs together in the first two commits so
    // all three co-change.  delete gone.rs from disk to create a ghost; peer.rs
    // remains present and anchors the AC2 retain assertion.
    let head = create_real_git_repo(
        dir.path(),
        &[
            // Joint commit: all three files co-change.
            (
                "feat: add all",
                &[
                    ("keep.rs", "fn keep() {}"),
                    ("gone.rs", "fn gone() {}"),
                    ("peer.rs", "fn peer() {}"),
                ],
            ),
            // Second joint commit: raises Jaccard for all pairs above MIN_COCHANGE_JACCARD.
            (
                "feat: update all",
                &[
                    ("keep.rs", "fn keep2() {}"),
                    ("gone.rs", "fn gone2() {}"),
                    ("peer.rs", "fn peer2() {}"),
                ],
            ),
            // Extra commit to keep.rs only (does not affect the peer/gone Jaccard).
            ("feat: keep only", &[("keep.rs", "fn keep3() {}")]),
        ],
    );

    // Delete gone.rs from disk — it remains in git history (ghost).
    std::fs::remove_file(dir.path().join("gone.rs"))
        .expect("gone.rs must exist on disk before deletion");

    let now = super::current_epoch_secs();
    rebuild_temporal(dir.path(), &cache_dir, &head, now).unwrap();

    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();

    // AC1: gone.rs must be absent from all three score tables.
    let hotspots = db.top_hotspots(50).unwrap();
    let risks = db.top_risks(50).unwrap();
    let coldspots = db.top_coldspots(50).unwrap();

    let gone_in_hotspot = hotspots.iter().any(|r| r.file_path == "gone.rs");
    let gone_in_risk = risks.iter().any(|r| r.file_path == "gone.rs");
    let gone_in_cold = coldspots.iter().any(|r| r.file_path == "gone.rs");

    assert!(
        !gone_in_hotspot,
        "AC1: gone.rs (ghost) must be ABSENT from top_hotspots — ghost filter not applied"
    );
    assert!(
        !gone_in_risk,
        "AC1: gone.rs (ghost) must be ABSENT from top_risks — ghost filter not applied"
    );
    assert!(
        !gone_in_cold,
        "AC1: gone.rs (ghost) must be ABSENT from top_coldspots — ghost filter not applied"
    );

    // keep.rs must still be present.
    let keep_in_hotspot = hotspots.iter().any(|r| r.file_path == "keep.rs");
    assert!(
        keep_in_hotspot,
        "AC1: keep.rs (present) must be in top_hotspots (should not be filtered)"
    );

    let partners = db.cochanges_for_file("keep.rs").unwrap();

    // AC2 DROP: cochange row (keep.rs, gone.rs) must be absent — gone.rs is a ghost.
    let ghost_partner = partners
        .iter()
        .any(|r| r.file_a == "gone.rs" || r.file_b == "gone.rs");
    assert!(
        !ghost_partner,
        "AC2: cochange row (keep.rs, gone.rs) must be ABSENT — ghost partner not filtered"
    );

    // AC2 RETAIN (positive anchor): cochange row (keep.rs, peer.rs) must be present.
    // Both files are on disk; the filter must retain this row.  Without this assertion
    // an over-zealous filter that drops all cochange rows — including legitimate
    // both-present pairs — would go undetected.
    let peer_partner_retained = partners
        .iter()
        .any(|r| r.file_a == "peer.rs" || r.file_b == "peer.rs");
    assert!(
        peer_partner_retained,
        "AC2: cochange row (keep.rs, peer.rs) must be RETAINED — \
         both files are present on disk; filter must not over-drop"
    );
}

/// AC9: Containment guard — a history path that is absolute or contains `..`
/// is treated as non-existent (dropped) and never stats outside root.
///
/// We test `rel_is_regular_file` directly: absolute paths and `..`-containing
/// paths are rejected without ever calling `is_file()` outside `root`.
#[test]
fn test_ghost_filter_containment_guard() {
    let dir = tempdir().unwrap();

    // A normal present file.
    std::fs::write(dir.path().join("present.rs"), "fn main() {}").unwrap();

    // Absolute path — rejected even if the target file exists.
    let abs_path = dir.path().join("present.rs").to_string_lossy().to_string();
    assert!(
        !rel_is_regular_file(dir.path(), &abs_path),
        "AC9: absolute path must be rejected by containment guard (is_absolute check)"
    );

    // Path with .. component — rejected regardless of whether target exists.
    assert!(
        !rel_is_regular_file(dir.path(), "../escape.rs"),
        "AC9: path with .. component must be rejected (ParentDir check)"
    );
    assert!(
        !rel_is_regular_file(dir.path(), "subdir/../../escape.rs"),
        "AC9: nested .. path must be rejected (ParentDir check)"
    );

    // A normal relative path to an existing file — accepted.
    assert!(
        rel_is_regular_file(dir.path(), "present.rs"),
        "AC9: present file with normal relative path must be accepted"
    );

    // A normal relative path to a missing file — correctly absent (not an error).
    assert!(
        !rel_is_regular_file(dir.path(), "missing.rs"),
        "AC9: missing file must return false (not error)"
    );

    // A path to a directory — rejected by is_file().
    let subdir = dir.path().join("subdir");
    std::fs::create_dir_all(&subdir).unwrap();
    assert!(
        !rel_is_regular_file(dir.path(), "subdir"),
        "AC9: directory path must be rejected by is_file() predicate (OD2)"
    );
}

/// Regression: ghost filter must NOT false-drop rows when `root` is a
/// subdirectory of the git worktree (AD-408-5), and the AD-413-17 scope filter
/// must re-anchor surviving paths to be root-relative.
///
/// Before the AD-408-5 fix, `apply_ghost_filter` joined REPO-ROOT-relative paths
/// against the search `root` subdir, double-nesting the prefix and causing every
/// row to fail `is_file()`:
///
/// Failure scenario (pre-AD-408-5-fix):
///   `skim search --hot --root <repo>/sub`
///   → gix discovers `<repo>` from `sub`, emits `sub/src/lib.rs`
///   → ghost filter (old): `sub.join("sub/src/lib.rs")` = `<repo>/sub/sub/src/lib.rs`
///     (double-nested; file does not exist there)
///   → all rows dropped; `--hot` returns empty output with exit 0 (silent loss).
///
/// With the AD-408-5 fix, paths are joined against `ghost_root` (the discovered
/// git workdir), so `<repo>/sub/src/lib.rs` exists and is retained.
///
/// The AD-413-17 scope filter then strips the `sub/` prefix so the stored path
/// is root-relative (`src/lib.rs`), matching what a lexical query on the same
/// root would return.
///
/// Files outside the `sub/` scope (e.g. top-level `a.rs`) are correctly excluded
/// by the scope filter — this is intentional: one `--root`, one result universe.
///
/// Discriminating: without the ghost-filter fix every hotspot row is false-ghosted
/// (double-path → `is_file()` miss); with it the row survives and after the scope
/// filter the stored path is `src/lib.rs`, not `sub/src/lib.rs`.
#[test]
fn test_ghost_filter_subdir_root_rows_survive() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Create a real git repo at dir.path() and commit `sub/src/lib.rs` there.
    // The file is INSIDE the `sub/` subtree; gix reports it as `sub/src/lib.rs`
    // (REPO-ROOT-relative).  Also commit `a.rs` at the repo root so we can
    // assert it is absent from results (AD-413-17 scope filter: one root, one
    // result universe).
    let head = create_real_git_repo(
        dir.path(),
        &[
            (
                "feat: add lib",
                &[("sub/src/lib.rs", "pub fn a() {}"), ("a.rs", "fn top() {}")],
            ),
            ("feat: update lib", &[("sub/src/lib.rs", "pub fn b() {}")]),
        ],
    );

    // Use `sub` as the search root — simulating
    // `skim search --hot --root <repo>/sub`.
    // `create_real_git_repo` already created `dir.path()/sub/src/lib.rs` on disk.
    let subdir = dir.path().join("sub");

    let now = super::current_epoch_secs();
    rebuild_temporal(&subdir, &cache_dir, &head, now).unwrap();

    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();

    let hotspots = db.top_hotspots(50).unwrap();
    assert!(
        !hotspots.is_empty(),
        "ghost filter must NOT drop rows when root is a subdirectory of the git worktree \
         (AD-408-5 regression: subdir double-path causes false ghost detection); \
         got {} hotspot rows — pre-fix behaviour returned 0",
        hotspots.len()
    );

    // AD-413-17: paths must be root-relative (prefix stripped), not repo-relative.
    let lib_present = hotspots.iter().any(|r| r.file_path == "src/lib.rs");
    assert!(
        lib_present,
        "src/lib.rs must survive with root-relative path after ghost filter + scope filter \
         (AD-408-5 / AD-413-17: gix emits repo-root-relative `sub/src/lib.rs`; \
         scope filter strips `sub/` prefix yielding `src/lib.rs`); \
         hotspot paths: {:?}",
        hotspots.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );

    // AD-413-17: the out-of-scope top-level file must NOT appear.
    let out_of_scope = hotspots.iter().any(|r| r.file_path == "a.rs");
    assert!(
        !out_of_scope,
        "a.rs lives outside sub/ and must be excluded by the AD-413-17 scope filter \
         (one --root, one result universe); hotspot paths: {:?}",
        hotspots.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );
}

/// AC4: --cold --limit N returns exactly N present rows even when ghost files
/// would have ranked at the top of the coldspot ordering.
///
/// Setup: 3 ghost files with very old commits (cold, score ~0 due to decay) and
/// 5 present files with recent commits (warmer but with only 1 commit each).
/// Without the ghost filter, the 3 coldest rows would be ghosts; with limit=4
/// a query-time filter would return only 1 present row (under-fill).  With the
/// build-time filter, the DB only contains the 5 present rows and top_coldspots(4)
/// returns exactly 4.
///
/// Discriminating: a query-time ghost filter would under-fill to 1 row; the
/// build-time filter returns exactly 4.  This test fails if the filter is moved
/// to query time.
///
/// Uses `create_real_git_repo_with_dates` to share the same setup helper as
/// undated tests (`test_ghost_filter_deleted_file_absent_from_all_tables`) and
/// avoid hand-rolling repeated `Command::new("git")` blocks.
#[test]
#[allow(clippy::type_complexity)]
fn test_ghost_filter_coldspot_limit_no_underfill() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // now_epoch pinned to 2026-06-13 08:00:00 UTC (matches other tests).
    let now_epoch: u64 = 1_781_337_600;

    // Old date → commits decay to ~0 → very cold scores.
    let ghost_date = "2020-01-01 00:00:00 +0000";
    // Recent date → commits have high decay weight → warmer scores.
    let present_date = "2026-06-10 00:00:00 +0000";

    // Co-locate each commit's (name, content, message) to avoid parallel-Vec
    // index alignment.  Owned strings live here; slices are taken below.
    let ghost_data: Vec<(String, String, String)> = (0..3u32)
        .map(|i| {
            (
                format!("ghost{i}.rs"),
                format!("// ghost {i}"),
                format!("feat: ghost {i}"),
            )
        })
        .collect();
    let present_data: Vec<(String, String, String)> = (0..5u32)
        .map(|i| {
            (
                format!("present{i}.rs"),
                format!("// present {i}"),
                format!("feat: present {i}"),
            )
        })
        .collect();

    // Per-commit file lists — each commit touches exactly one file.
    let ghost_files: Vec<Vec<(&str, &str)>> = ghost_data
        .iter()
        .map(|(n, c, _)| vec![(n.as_str(), c.as_str())])
        .collect();
    let present_files: Vec<Vec<(&str, &str)>> = present_data
        .iter()
        .map(|(n, c, _)| vec![(n.as_str(), c.as_str())])
        .collect();

    // Build commit specs: (message, date, files).
    let mut commit_specs: Vec<(&str, Option<&str>, &[(&str, &str)])> = Vec::new();
    for (i, (_, _, msg)) in ghost_data.iter().enumerate() {
        commit_specs.push((msg.as_str(), Some(ghost_date), ghost_files[i].as_slice()));
    }
    for (i, (_, _, msg)) in present_data.iter().enumerate() {
        commit_specs.push((
            msg.as_str(),
            Some(present_date),
            present_files[i].as_slice(),
        ));
    }

    let head = create_real_git_repo_with_dates(dir.path(), &commit_specs);

    // Delete ghost files from disk (they remain in git history).
    for (name, _, _) in &ghost_data {
        std::fs::remove_file(dir.path().join(name)).unwrap();
    }

    rebuild_temporal(dir.path(), &cache_dir, &head, now_epoch).unwrap();

    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();

    // With ghost filter: DB has only 5 present rows → top_coldspots(4) returns 4.
    let limit: usize = 4;
    let coldspots = db.top_coldspots(limit).unwrap();

    assert_eq!(
        coldspots.len(),
        limit,
        "AC4: top_coldspots({limit}) must return exactly {limit} rows (build-time ghost filter \
         ensures no ghosts in DB); got {} — query-time filter would under-fill when ghosts cluster \
         at the top of the coldspot ordering",
        coldspots.len()
    );

    // All returned rows must be present on disk.
    for row in &coldspots {
        assert!(
            dir.path().join(&row.file_path).is_file(),
            "AC4: coldspot row '{}' must be present on disk (ghost filter must have removed ghosts)",
            row.file_path
        );
    }

    // Ghost files must not appear in any coldspot row.
    for (name, _, _) in &ghost_data {
        let ghost_present = coldspots.iter().any(|r| r.file_path == *name);
        assert!(
            !ghost_present,
            "AC4: ghost file '{name}' must not appear in top_coldspots"
        );
    }
}

// ============================================================================
// PF-017 — Refuse policy must not overwrite a differing temporal anchor
// ============================================================================

/// PF-017 regression: `ReanchorPolicy::Refuse` must not overwrite an existing
/// [`rskim_search::META_GIT_TOPLEVEL`] anchor that differs from the live
/// `ghost_root`.
///
/// Without the PF-017 gate in `record_temporal_anchor`, a plain lexical query
/// whose HEAD changed could trigger an auto-refresh rebuild, and that rebuild
/// would silently overwrite the anchor set by a prior explicit `--rebuild` —
/// corrupting the per-worktree isolation invariant (AD-413-16 / ADR-009).
///
/// Method:
/// 1. Create a real git repo so `discover_git_workdir` can find it.
/// 2. Create a plain subdirectory inside the repo.
/// 3. Rebuild with `root = subdir`, `Allow` → anchor is written (adopted case).
/// 4. Overwrite the anchor with a sentinel (`/pf017-sentinel/...`) to simulate
///    a DB anchored to a *different* linked-worktree root.
/// 5. Rebuild again with `Refuse` → PF-017 guard must block the write.
/// 6. Assert the anchor is still the sentinel.
///
/// The test skips gracefully when the subdirectory adoption check does not fire
/// (e.g. `gix::discover` returns `None` so `ghost_root` falls back to `root`);
/// that scenario cannot exercise the PF-017 guard and is not a regression.
#[test]
fn test_pf017_refuse_policy_does_not_overwrite_anchor() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join(".cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Step 1: real git repo so discover_git_workdir returns dir.path().
    let head = create_real_git_repo(
        dir.path(),
        &[("feat: initial", &[("src/lib.rs", "fn main() {}")])],
    );

    // Step 2: plain subdirectory — not a separate git repo.
    let sub_dir = dir.path().join("sub");
    std::fs::create_dir_all(&sub_dir).unwrap();

    let now = super::current_epoch_secs();

    // Step 3: Allow rebuild with root = sub_dir.
    let src1 = CountingSource::new_empty();
    let r1 = rebuild_temporal_with_source(
        &src1,
        &sub_dir,
        &cache_dir,
        &head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    );
    assert!(r1.is_ok(), "Allow rebuild must succeed: {r1:?}");

    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    let anchor_after_allow = db
        .get_meta(rskim_search::META_GIT_TOPLEVEL)
        .expect("get_meta must not fail");

    // If adopted check did not fire (ghost_root fallback == root), there is no
    // anchor to protect and the PF-017 guard path is never reached — skip.
    let Some(_live_anchor) = anchor_after_allow else {
        return;
    };

    // Step 4: overwrite with a sentinel representing a different repo.
    const SENTINEL: &str = "/pf017-sentinel/other/repo";
    db.set_meta(rskim_search::META_GIT_TOPLEVEL, SENTINEL)
        .expect("sentinel write must succeed");
    drop(db);

    // Step 5: Refuse rebuild — PF-017 gate must block the anchor overwrite.
    let src2 = CountingSource::new_empty();
    let r2 = rebuild_temporal_with_source(
        &src2,
        &sub_dir,
        &cache_dir,
        &head,
        now,
        ReanchorPolicy::Refuse,
        BuildLoudness::Silent,
    );
    assert!(r2.is_ok(), "Refuse rebuild must succeed: {r2:?}");

    // Step 6: anchor must still be the sentinel.
    let db2 = rskim_search::TemporalDb::open(&db_path).unwrap();
    let anchor_after_refuse = db2
        .get_meta(rskim_search::META_GIT_TOPLEVEL)
        .expect("get_meta must not fail after Refuse rebuild")
        .expect("anchor must remain present after Refuse rebuild");
    assert_eq!(
        anchor_after_refuse, SENTINEL,
        "PF-017 regression: Refuse policy must not overwrite a differing anchor \
         (sentinel={SENTINEL:?}, overwritten_to={anchor_after_refuse:?})"
    );
}

// ============================================================================
// AD-413-17 scope-filter unit tests (S36)
// ============================================================================

/// AD-413-17: `apply_scope_filter` retains and re-anchors ALL THREE row types
/// (hotspot, risk, cochange) when `root` is a proper subdirectory of the git
/// worktree.
///
/// # Fixture layout
///
/// ```text
/// <dir>/
///   .git/                     ← git workdir (ghost_root)
///   sub/
///     foo.rs                  ← inside scope; 3 commits (1 fix)
///     bar.rs                  ← inside scope; 2 commits
///     baz.rs                  ← inside scope; 1 commit
///   a.rs                      ← OUTSIDE scope; 2 commits
/// ```
///
/// Fake commits (repo-root-relative paths, as gix would report them):
///
/// | # | message            | files                     |
/// |---|--------------------|---------------------------|
/// | 1 | fix: fix foo       | sub/foo.rs                |
/// | 2 | feat: update sub   | sub/foo.rs, sub/bar.rs    |
/// | 3 | feat: update sub again | sub/foo.rs, sub/bar.rs |
/// | 4 | feat: update a     | a.rs                      |
/// | 5 | feat: cross boundary | a.rs, sub/baz.rs        |
///
/// Expected cochange pairs produced by `build_cochange_rows`:
/// - `(sub/bar.rs, sub/foo.rs)`: count=2, Jaccard=2/3≈0.67 ≥ 0.10 → retained
/// - `(a.rs, sub/baz.rs)`: count=1, Jaccard=1/2=0.50 ≥ 0.10 → cross-boundary → dropped
///
/// # Assertions
///
/// **hotspot:** `foo.rs`, `bar.rs`, `baz.rs` present (prefix stripped); `a.rs` absent;
/// no path retains the `sub/` prefix.
///
/// **risk:** `foo.rs` present (only file with a fix commit, so risk rows include it);
/// `a.rs` absent; no path retains the `sub/` prefix.
///
/// **cochange:** `(bar.rs, foo.rs)` present (both sides inside scope, both rewritten);
/// no partner for `baz.rs` (cross-boundary pair `(a.rs, sub/baz.rs)` dropped);
/// no path retains the `sub/` prefix.
///
/// # Reverted behaviour (discriminating)
///
/// - Delete `risk_rows.retain_mut` from `apply_scope_filter` → `a.rs` appears in
///   risk results and this test fails.
/// - Delete `cochange_rows.retain_mut` from `apply_scope_filter` → the cross-boundary
///   pair `(a.rs, sub/baz.rs)` survives (unreachable path on the query side) and the
///   `bar.rs` partner check fails because stored paths still carry the `sub/` prefix.
/// - Delete `hotspot_rows.retain_mut` → `a.rs` appears in hotspot results.
#[test]
fn test_temporal_rows_are_scoped_and_reanchored_to_subdir_root() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Create a real git repo at dir.path() so that discover_git_workdir(sub_dir)
    // resolves to dir.path() (ghost_root == dir.path()).  The commit also writes
    // the actual files to disk so apply_ghost_filter does NOT false-drop any row
    // (rel_is_regular_file checks disk existence against ghost_root).
    let head = create_real_git_repo(
        dir.path(),
        &[(
            "feat: seed all files",
            &[
                ("sub/foo.rs", "pub fn foo() {}"),
                ("sub/bar.rs", "pub fn bar() {}"),
                ("sub/baz.rs", "pub fn baz() {}"),
                ("a.rs", "fn a() {}"),
            ],
        )],
    );

    // Pinned epoch (same as other tests referencing "2026-06-13 08:00:00 UTC").
    let now_epoch: u64 = 1_781_337_600;

    // Handcrafted commits using repo-root-relative paths, matching what gix
    // reports.  Commit 1 is a fix commit (message starts with "fix:") so
    // sub/foo.rs acquires fix_density > 0 and a risk row.
    let commits = vec![
        make_commit(
            "aaa0000001",
            now_epoch as i64 - 86400 * 3,
            "fix: fix foo",
            &["sub/foo.rs"],
        ),
        make_commit(
            "aaa0000002",
            now_epoch as i64 - 86400 * 2,
            "feat: update sub",
            &["sub/foo.rs", "sub/bar.rs"],
        ),
        make_commit(
            "aaa0000003",
            now_epoch as i64 - 86400,
            "feat: update sub again",
            &["sub/foo.rs", "sub/bar.rs"],
        ),
        make_commit(
            "aaa0000004",
            now_epoch as i64 - 86400 * 5,
            "feat: update a",
            &["a.rs"],
        ),
        // This commit creates a cross-boundary cochange pair (a.rs, sub/baz.rs).
        // The scope filter must drop it because a.rs is outside sub/.
        make_commit(
            "aaa0000005",
            now_epoch as i64 - 86400 * 6,
            "feat: cross boundary",
            &["a.rs", "sub/baz.rs"],
        ),
    ];

    let src = FixedSource {
        history: make_history(commits),
    };
    let sub_dir = dir.path().join("sub");
    rebuild_temporal_with_source(
        &src,
        &sub_dir,
        &cache_dir,
        &head,
        now_epoch,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    )
    .expect("rebuild_temporal_with_source must succeed for subdir root");

    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path).expect("temporal.db must be openable");

    // ── hotspot rows ─────────────────────────────────────────────────────────
    let hotspots = db.top_hotspots(50).expect("top_hotspots must not fail");

    assert!(
        hotspots.iter().any(|r| r.file_path == "foo.rs"),
        "AD-413-17: foo.rs must appear in hotspots after scope filter strips sub/ prefix; \
         hotspot paths: {:?}",
        hotspots.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );
    assert!(
        hotspots.iter().any(|r| r.file_path == "bar.rs"),
        "AD-413-17: bar.rs must appear in hotspots after scope filter strips sub/ prefix; \
         hotspot paths: {:?}",
        hotspots.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );
    assert!(
        hotspots.iter().any(|r| r.file_path == "baz.rs"),
        "AD-413-17: baz.rs must appear in hotspots after scope filter strips sub/ prefix; \
         hotspot paths: {:?}",
        hotspots.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );
    assert!(
        hotspots.iter().all(|r| r.file_path != "a.rs"),
        "AD-413-17: a.rs is outside sub/ scope and must be absent from hotspots; \
         hotspot paths: {:?}",
        hotspots.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );
    assert!(
        hotspots.iter().all(|r| !r.file_path.starts_with("sub/")),
        "AD-413-17: no hotspot path must retain the sub/ prefix after reanchoring; \
         hotspot paths: {:?}",
        hotspots.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );

    // ── risk rows ─────────────────────────────────────────────────────────────
    // Only files with at least one commit appear in risk_rows; foo.rs is the only
    // file with a fix commit so it must carry risk_score > 0 and fix_density > 0.
    let risks = db.top_risks(50).expect("top_risks must not fail");

    assert!(
        risks.iter().any(|r| r.file_path == "foo.rs"),
        "AD-413-17: foo.rs must appear in risk rows after scope filter strips sub/ prefix; \
         risk paths: {:?}",
        risks.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );
    // foo.rs has 1 fix commit out of 3 total → fix_density > 0.
    let foo_risk = risks.iter().find(|r| r.file_path == "foo.rs").unwrap();
    assert!(
        foo_risk.fix_density > 0.0,
        "AD-413-17: foo.rs fix_density must be > 0 (has 1 fix commit); got {}",
        foo_risk.fix_density
    );
    assert!(
        risks.iter().all(|r| r.file_path != "a.rs"),
        "AD-413-17: a.rs is outside sub/ scope and must be absent from risk rows; \
         risk paths: {:?}",
        risks.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );
    assert!(
        risks.iter().all(|r| !r.file_path.starts_with("sub/")),
        "AD-413-17: no risk path must retain the sub/ prefix after reanchoring; \
         risk paths: {:?}",
        risks.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );

    // ── cochange rows ─────────────────────────────────────────────────────────
    // (bar.rs, foo.rs): both inside scope, count=2, Jaccard=2/3≈0.67 → retained +
    // rewritten.
    let foo_partners = db
        .cochanges_for_file("foo.rs")
        .expect("cochanges_for_file(foo.rs) must not fail");
    let bar_partner = foo_partners
        .iter()
        .find(|r| r.file_a == "bar.rs" || r.file_b == "bar.rs");
    assert!(
        bar_partner.is_some(),
        "AD-413-17: cochange pair (bar.rs, foo.rs) must be retained and rewritten by scope \
         filter (both sides inside sub/); foo.rs partners: {:?}",
        foo_partners
    );
    // (a.rs, sub/baz.rs) cross-boundary: scope filter drops it (cochange both-sides rule).
    let baz_partners = db
        .cochanges_for_file("baz.rs")
        .expect("cochanges_for_file(baz.rs) must not fail");
    let a_partner = baz_partners
        .iter()
        .find(|r| r.file_a == "a.rs" || r.file_b == "a.rs");
    assert!(
        a_partner.is_none(),
        "AD-413-17: cross-boundary pair (a.rs, baz.rs) must be dropped by scope filter \
         (cochange both-sides rule — a.rs is outside sub/); baz.rs partners: {:?}",
        baz_partners
    );
    // No cochange path must retain the sub/ prefix.
    let all_partners: Vec<_> = foo_partners.iter().chain(baz_partners.iter()).collect();
    assert!(
        all_partners
            .iter()
            .all(|r| !r.file_a.starts_with("sub/") && !r.file_b.starts_with("sub/")),
        "AD-413-17: no cochange path must retain the sub/ prefix after reanchoring; \
         found: {:?}",
        all_partners
    );
}

/// AD-413-17: when `root == ghost_root` (a plain toplevel repository, no subdirectory),
/// `scope` is `None` and `apply_scope_filter` is never called.
///
/// The stored rows are therefore byte-identical to the pre-AD-413-17 state —
/// every computed row survives, every path retains its original form, and in
/// particular the repository's hottest file (`a.rs`, touched by more commits
/// than any file under `sub/`) appears first in hotspot results when the root is
/// the toplevel repository.
///
/// # Discriminating
///
/// If `scope` were accidentally non-`None` for a toplevel root (e.g., an empty
/// string passed as a prefix), `apply_scope_filter` would be called.  A
/// non-empty prefix would silently drop every row; an empty prefix used in
/// `starts_with("")` would always return `true` but `drain(..0)` is a no-op,
/// so rows would survive — but this test guards the whole identity path by also
/// asserting that `a.rs` (the out-of-`sub/`-scope file) IS present, which is
/// the exact row that `test_temporal_rows_are_scoped_and_reanchored_to_subdir_root`
/// asserts is absent when the root is `sub/`.
#[test]
fn test_toplevel_root_rows_are_byte_identical_to_pre_change() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Real git repo at dir.path(): discover_git_workdir(dir.path()) returns
    // dir.path() itself, so canonical(root).strip_prefix(canonical(ghost_root))
    // yields an empty path → scope = None → apply_scope_filter is never called.
    let head = create_real_git_repo(
        dir.path(),
        &[(
            "feat: seed all files",
            &[
                ("a.rs", "fn a() {}"),
                ("sub/foo.rs", "pub fn foo() {}"),
                ("sub/bar.rs", "pub fn bar() {}"),
            ],
        )],
    );

    let now_epoch: u64 = 1_781_337_600;

    // a.rs is given strictly more commits than any sub/ file so it ranks first
    // in hotspots — matching AD-413-17's fixture requirement that the hottest
    // file lives OUTSIDE the sub/ subtree.
    let commits = vec![
        make_commit(
            "bbb0000001",
            now_epoch as i64 - 86400 * 5,
            "fix: fix a",
            &["a.rs"],
        ),
        make_commit(
            "bbb0000002",
            now_epoch as i64 - 86400 * 4,
            "fix: fix a again",
            &["a.rs"],
        ),
        make_commit(
            "bbb0000003",
            now_epoch as i64 - 86400 * 3,
            "fix: fix a third time",
            &["a.rs"],
        ),
        make_commit(
            "bbb0000004",
            now_epoch as i64 - 86400 * 2,
            "feat: update sub/foo",
            &["sub/foo.rs"],
        ),
        // Co-change pair entirely inside sub/: both sides survive at both the
        // toplevel root (identity) and the sub/ root (scope filter rewrites them).
        make_commit(
            "bbb0000005",
            now_epoch as i64 - 86400,
            "feat: update both sub files",
            &["sub/foo.rs", "sub/bar.rs"],
        ),
    ];

    let src = FixedSource {
        history: make_history(commits),
    };
    rebuild_temporal_with_source(
        &src,
        dir.path(),
        &cache_dir,
        &head,
        now_epoch,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    )
    .expect("rebuild_temporal_with_source must succeed for toplevel root");

    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path).expect("temporal.db must be openable");

    // ── hotspot rows (identity path: all rows survive unchanged) ─────────────
    let hotspots = db.top_hotspots(50).expect("top_hotspots must not fail");

    // a.rs must be present — it is the hottest file in the repo-wide history.
    // This is the core identity assertion: a.rs would be absent if scope were
    // incorrectly applied for a toplevel root.
    assert!(
        hotspots.iter().any(|r| r.file_path == "a.rs"),
        "AD-413-17 identity: a.rs must appear in hotspots for toplevel root \
         (scope filter must NOT run when root == ghost_root); \
         hotspot paths: {:?}",
        hotspots.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );
    // sub/foo.rs and sub/bar.rs must appear with their full repo-relative paths
    // (NOT stripped to foo.rs / bar.rs — scope filter is the identity).
    assert!(
        hotspots.iter().any(|r| r.file_path == "sub/foo.rs"),
        "AD-413-17 identity: sub/foo.rs must appear with its full repo-relative path \
         (scope filter must NOT strip the sub/ prefix for a toplevel root); \
         hotspot paths: {:?}",
        hotspots.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );
    assert!(
        hotspots.iter().any(|r| r.file_path == "sub/bar.rs"),
        "AD-413-17 identity: sub/bar.rs must appear with its full repo-relative path; \
         hotspot paths: {:?}",
        hotspots.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );

    // ── risk rows (identity path) ─────────────────────────────────────────────
    let risks = db.top_risks(50).expect("top_risks must not fail");

    assert!(
        risks.iter().any(|r| r.file_path == "a.rs"),
        "AD-413-17 identity: a.rs must appear in risk rows for toplevel root; \
         risk paths: {:?}",
        risks.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );
    // a.rs has 3 fix commits out of 3 → fix_density == 1.0.
    let a_risk = risks.iter().find(|r| r.file_path == "a.rs").unwrap();
    assert!(
        (a_risk.fix_density - 1.0_f64).abs() < 1e-9,
        "AD-413-17 identity: a.rs fix_density must be 1.0 (3 fix commits / 3 total); \
         got {}",
        a_risk.fix_density
    );
    // sub/foo.rs must appear with its full path (not stripped).
    assert!(
        risks.iter().any(|r| r.file_path == "sub/foo.rs"),
        "AD-413-17 identity: sub/foo.rs must appear in risk rows with full path; \
         risk paths: {:?}",
        risks.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );

    // ── cochange rows (identity path) ─────────────────────────────────────────
    // (sub/bar.rs, sub/foo.rs): count=1, Jaccard=1/(2+2-1)=0.33 ≥ 0.10 → present
    // with FULL repo-relative paths (scope filter is the identity for toplevel root).
    let foo_partners = db
        .cochanges_for_file("sub/foo.rs")
        .expect("cochanges_for_file(sub/foo.rs) must not fail");
    let bar_partner = foo_partners
        .iter()
        .find(|r| r.file_a == "sub/bar.rs" || r.file_b == "sub/bar.rs");
    assert!(
        bar_partner.is_some(),
        "AD-413-17 identity: cochange pair (sub/bar.rs, sub/foo.rs) must appear with full \
         repo-relative paths for a toplevel root (scope filter must NOT rewrite paths); \
         sub/foo.rs partners: {:?}",
        foo_partners
    );
}

// ============================================================================
// T-9 — Corrupt DB: discard + exactly-one-retry → rebuilt and populated
// ============================================================================

/// T-9 (AD-414-3): When `temporal.db` is structurally corrupt (SQLITE_NOTADB),
/// `rebuild_temporal_with_source` must:
/// 1. Print a non-debug-gated notice (AC-14).
/// 2. Delete the corrupt file (SE-3 main-first rule).
/// 3. Attempt exactly ONE re-open (never a loop) to create a fresh DB.
/// 4. Populate the new DB with the computed rows.
/// 5. Return `Ok(())`.
///
/// Discriminating: the rebuilt temporal.db is openable and contains hotspot rows
/// for the source files; the backoff sentinel is NOT written (recovery succeeded).
///
/// PF-012: corrupt fixture = `0xAB × 1024` bytes (deterministic, not /dev/urandom).
#[test]
fn test_corrupt_db_discarded_and_rebuilt() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // PF-012: deterministic corrupt fixture (SQLITE_NOTADB when opened).
    let corrupt_bytes = vec![0xABu8; 1024];
    std::fs::write(cache_dir.join("temporal.db"), &corrupt_bytes)
        .expect("write corrupt temporal.db");

    // Create "a.rs" on disk at the root so the ghost filter does not drop the row.
    std::fs::write(dir.path().join("a.rs"), b"fn a() {}")
        .expect("write a.rs for ghost-filter anchor");

    // FixedSource: two commits each touching a.rs → produces a hotspot row.
    let history = HistoryResult {
        commits: vec![
            make_commit("aaa00001", 1_000_000, "feat: initial", &["a.rs"]),
            make_commit("aaa00002", 1_000_001, "feat: update", &["a.rs"]),
        ],
        metadata: rskim_search::TemporalMetadata {
            is_shallow: false,
            commit_count: 2,
        },
    };
    let src = FixedSource { history };

    let fake_head = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";
    let now = super::current_epoch_secs();

    let result = rebuild_temporal_with_source(
        &src,
        dir.path(),
        &cache_dir,
        fake_head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    );
    assert!(
        result.is_ok(),
        "T-9: rebuild_temporal_with_source must return Ok(()) after corrupt DB discard; \
         got {result:?}"
    );

    // Discriminating: temporal.db must be openable (fresh DB, not still corrupt).
    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path)
        .expect("T-9: temporal.db must be openable after corrupt-discard rebuild");

    // Discriminating: the rebuilt DB must contain the computed rows.
    let hotspots = db.top_hotspots(20).expect("T-9: top_hotspots must succeed");
    assert!(
        hotspots.iter().any(|r| r.file_path == "a.rs"),
        "T-9: rebuilt temporal.db must contain a hotspot row for a.rs; \
         got rows: {:?}",
        hotspots.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );

    // Backoff sentinel must NOT be written (recovery succeeded, no unread failure).
    assert!(
        !cache_dir.join("temporal.db.build_backoff").exists(),
        "T-9: backoff sentinel must not be written when corrupt DB recovery succeeds"
    );
}

// ============================================================================
// T-10 — Future-schema DB: byte-unchanged + backoff sentinel written
// ============================================================================

/// T-10 (AD-414-11): When `temporal.db` has `user_version > CURRENT_VERSION`
/// (written by a future binary), `rebuild_temporal_with_source` must:
/// 1. Leave `temporal.db` byte-for-byte unchanged (R1 contract).
/// 2. Classify the error as `UnsupportedVersion` (not `DatabaseCorrupt`).
/// 3. Write the backoff sentinel so subsequent queries skip the retry.
/// 4. Return `Ok(())`.
///
/// Discriminating: the sentinel is present with the expected HEAD, and the DB
/// still has `user_version = 99` — the file was not modified.
#[test]
fn test_future_schema_db_byte_unchanged_and_backoff_written() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Create a well-formed SQLite file with user_version = 99 (> CURRENT_VERSION 2).
    // TemporalDb::open reads user_version first, so any valid SQLite header triggers
    // the UnsupportedSchemaVersion path without needing a full schema.
    let db_path = cache_dir.join("temporal.db");
    {
        let conn = rusqlite::Connection::open(&db_path)
            .expect("T-10: create sqlite file for future-schema fixture");
        conn.execute_batch("PRAGMA user_version = 99;")
            .expect("T-10: set user_version = 99");
    } // connection dropped here (file flushed)

    let fake_head = "bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222";
    let now = super::current_epoch_secs();

    let result = rebuild_temporal_with_source(
        &CountingSource::new_empty(),
        dir.path(),
        &cache_dir,
        fake_head,
        now,
        ReanchorPolicy::Allow, // explicit build: SE-1 loud
        BuildLoudness::Loud,
    );
    assert!(
        result.is_ok(),
        "T-10: rebuild_temporal_with_source must return Ok(()) on future-schema DB; \
         got {result:?}"
    );

    // Discriminating: temporal.db must still have user_version = 99 (byte-unchanged).
    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("T-10: open future-schema DB for version verification");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("T-10: read user_version");
        assert_eq!(
            version, 99,
            "T-10: temporal.db must retain user_version = 99 (R1: byte-unchanged)"
        );
    }

    // Discriminating: backoff sentinel must be written with the expected HEAD.
    let sentinel_path = cache_dir.join("temporal.db.build_backoff");
    assert!(
        sentinel_path.exists(),
        "T-10: backoff sentinel must be written after UnsupportedSchemaVersion"
    );
    let sentinel_content = std::fs::read(&sentinel_path).expect("T-10: read sentinel");
    assert_eq!(
        sentinel_content,
        fake_head.as_bytes(),
        "T-10: backoff sentinel must contain the current HEAD bytes"
    );
}

// ============================================================================
// T-16 — FX-FAKE-SOURCE: zero-row build notice for all three causes
// ============================================================================

/// T-16(i): FX-FAKE-SOURCE with zero commits → zero-row notice, Case (i).
///
/// When `parse_history` returns zero commits (empty history), the rebuilt
/// `temporal.db` must have `META_GIT_HEAD` set so `temporal_db_is_stale` returns
/// false on the next query (LOCKED DECISION 2026-06-24 / Finding 2).
///
/// AC-16 guard: the zero-row notice for Case (i) must NOT contain "shallow" or
/// "unshallow" — it fires because there are no commits, not because of shallow
/// clone state.  DB state is the discriminating assertion; stderr text is
/// indirectly constrained by the `degraded_notice` builder used (DegradedReason::Empty,
/// detail = "").
///
/// Note: all three T-16 variants exercise `sync(is_shallow=false/true)` and
/// confirm that META_IS_SHALLOW is written correctly.
#[test]
fn test_t16_case_i_zero_commits_writes_head_not_shallow() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Case (i): zero commits, is_shallow = false.
    let history = HistoryResult {
        commits: vec![],
        metadata: rskim_search::TemporalMetadata {
            is_shallow: false,
            commit_count: 0,
        },
    };
    let src = FixedSource { history };

    let fake_head = "cccc3333cccc3333cccc3333cccc3333cccc3333";
    let now = super::current_epoch_secs();

    let result = rebuild_temporal_with_source(
        &src,
        dir.path(),
        &cache_dir,
        fake_head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    );
    assert!(
        result.is_ok(),
        "T-16(i): rebuild must return Ok(()) on zero-commit history; got {result:?}"
    );

    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path)
        .expect("T-16(i): temporal.db must be openable after zero-commit rebuild");

    // META_GIT_HEAD must be set (prevents retry loop).
    let stored_head = db
        .get_meta(rskim_search::META_GIT_HEAD)
        .expect("T-16(i): get_meta must not fail")
        .expect("T-16(i): META_GIT_HEAD must be present");
    assert_eq!(
        stored_head, fake_head,
        "T-16(i): META_GIT_HEAD must equal the passed head"
    );

    // META_IS_SHALLOW must be "0" (is_shallow was false).
    let stored_shallow = db
        .get_meta(rskim_search::META_IS_SHALLOW)
        .expect("T-16(i): get_meta(META_IS_SHALLOW) must not fail")
        .expect("T-16(i): META_IS_SHALLOW must be present after sync");
    assert_eq!(
        stored_shallow, "0",
        "T-16(i): META_IS_SHALLOW must be '0' when history is not shallow"
    );

    // No hotspot rows (zero commits).
    let hotspots = db
        .top_hotspots(20)
        .expect("T-16(i): top_hotspots must succeed");
    assert!(
        hotspots.is_empty(),
        "T-16(i): temporal.db must have zero hotspot rows on zero-commit history; \
         got {} rows",
        hotspots.len()
    );
}

/// T-16(ii): FX-FAKE-SOURCE with `is_shallow=true` + no changed files → Case (ii).
///
/// When parse_history reports a shallow clone with commits that have no
/// changed_files (common in shallow-fetch scenarios where diff extraction fails),
/// `pre_ghost_hotspot == 0` and the shallow-wording notice fires.
///
/// Discriminating: `META_IS_SHALLOW = "1"` is written by `sync()` (AD-414-14),
/// confirming the shallow state was correctly threaded from metadata through to
/// the DB.  This is also the data that Check 3 in `temporal_db_is_stale` uses.
#[test]
fn test_t16_case_ii_shallow_no_changed_files_writes_shallow_meta() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Case (ii): one commit present but changed_files is empty (shallow-fetch).
    // pre_ghost_hotspot == 0 and is_shallow == true → Case (ii) fires.
    let history = HistoryResult {
        commits: vec![make_commit(
            "dddd0001",
            1_000_000,
            "feat: initial (shallow)",
            &[], // no changed files — typical in shallow fetches where diff extraction fails
        )],
        metadata: rskim_search::TemporalMetadata {
            is_shallow: true,
            commit_count: 1,
        },
    };
    let src = FixedSource { history };

    let fake_head = "dddd4444dddd4444dddd4444dddd4444dddd4444";
    let now = super::current_epoch_secs();

    let result = rebuild_temporal_with_source(
        &src,
        dir.path(),
        &cache_dir,
        fake_head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    );
    assert!(
        result.is_ok(),
        "T-16(ii): rebuild must return Ok(()) on shallow history with no diffs; got {result:?}"
    );

    let db_path = cache_dir.join("temporal.db");
    let db =
        rskim_search::TemporalDb::open(&db_path).expect("T-16(ii): temporal.db must be openable");

    // Discriminating: META_IS_SHALLOW must be "1" (AD-414-14, sync() writes it).
    let stored_shallow = db
        .get_meta(rskim_search::META_IS_SHALLOW)
        .expect("T-16(ii): get_meta(META_IS_SHALLOW) must not fail")
        .expect("T-16(ii): META_IS_SHALLOW must be present");
    assert_eq!(
        stored_shallow, "1",
        "T-16(ii): META_IS_SHALLOW must be '1' when history is shallow (AD-414-14)"
    );

    // No hotspot rows (no changed files in any commit).
    let hotspots = db
        .top_hotspots(20)
        .expect("T-16(ii): top_hotspots must succeed");
    assert!(
        hotspots.is_empty(),
        "T-16(ii): temporal.db must have zero hotspot rows when all commits have no diffs"
    );

    // META_GIT_HEAD must be set (prevents retry loop).
    let stored_head = db
        .get_meta(rskim_search::META_GIT_HEAD)
        .expect("T-16(ii): get_meta must not fail")
        .expect("T-16(ii): META_GIT_HEAD must be present");
    assert_eq!(
        stored_head, fake_head,
        "T-16(ii): META_GIT_HEAD must equal the passed head"
    );
}

/// T-16(iii): FX-FAKE-SOURCE with phantom paths → ghost filter drops all → Case (iii).
///
/// When parse_history returns commits that touch files NOT present on disk at the
/// indexed root, `pre_ghost_hotspot > 0` (rows were computed) but the ghost filter
/// drops all of them. The Case (iii) notice fires.
///
/// Discriminating: temporal.db has zero hotspot rows (all dropped by ghost filter)
/// but META_GIT_HEAD is set (LOCKED DECISION prevents retry loop), and the
/// backoff sentinel is NOT written (sync succeeded — the zero-rows DB is written
/// successfully, which is the correct outcome for an empty ghost-filter result).
#[test]
fn test_t16_case_iii_ghost_filter_drops_all_rows_writes_head() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Case (iii): commit touches "phantom.rs" which does NOT exist on disk.
    // pre_ghost_hotspot == 1 (one hotspot row computed), but ghost filter drops it.
    let history = HistoryResult {
        commits: vec![
            make_commit("eeee0001", 1_000_000, "feat: initial", &["phantom.rs"]),
            make_commit("eeee0002", 1_000_001, "feat: update", &["phantom.rs"]),
        ],
        metadata: rskim_search::TemporalMetadata {
            is_shallow: false,
            commit_count: 2,
        },
    };
    let src = FixedSource { history };

    // "phantom.rs" is NOT created on disk — ghost filter will drop the row.

    let fake_head = "eeee5555eeee5555eeee5555eeee5555eeee5555";
    let now = super::current_epoch_secs();

    let result = rebuild_temporal_with_source(
        &src,
        dir.path(),
        &cache_dir,
        fake_head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    );
    assert!(
        result.is_ok(),
        "T-16(iii): rebuild must return Ok(()) when ghost filter drops all rows; got {result:?}"
    );

    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path)
        .expect("T-16(iii): temporal.db must be openable after ghost-filter-zero rebuild");

    // META_GIT_HEAD must be set (LOCKED DECISION — prevents retry loop).
    let stored_head = db
        .get_meta(rskim_search::META_GIT_HEAD)
        .expect("T-16(iii): get_meta must not fail")
        .expect("T-16(iii): META_GIT_HEAD must be present");
    assert_eq!(
        stored_head, fake_head,
        "T-16(iii): META_GIT_HEAD must equal the passed head"
    );

    // Discriminating: zero hotspot rows (ghost filter dropped all).
    let hotspots = db
        .top_hotspots(20)
        .expect("T-16(iii): top_hotspots must succeed");
    assert!(
        hotspots.is_empty(),
        "T-16(iii): ghost filter must drop all rows when no source files exist on disk; \
         got {} rows: {:?}",
        hotspots.len(),
        hotspots.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );

    // Backoff sentinel must NOT be written (sync succeeded with zero rows — not a failure).
    assert!(
        !cache_dir.join("temporal.db.build_backoff").exists(),
        "T-16(iii): backoff sentinel must not be written when sync succeeds with zero rows"
    );
}

// ============================================================================
// T-22 / T-29 — Corrupt DB + chmod 500: Ok(()), sidecars preserved (SE-3)
// ============================================================================

/// T-22 (AD-414-3 bounded retry) / T-29 (AC-29 / SE-3 sidecar preservation):
/// When `temporal.db` is corrupt AND the cache directory is not writable (chmod 500),
/// `remove_file` fails (EACCES). The function must:
/// 1. Print the non-debug-gated AC-29 notice (actionable manual-deletion message).
/// 2. Return `Ok(())` — never panic or propagate the permission error.
/// 3. Leave `temporal.db` byte-unchanged (SE-3 main-first rule — no partial delete).
/// 4. Leave `temporal.db-wal` and `temporal.db-shm` byte-unchanged (SE-3 sidecar
///    rule: sidecars are removed ONLY after a SUCCESSFUL main unlink).
///
/// The "bounded retry" property (T-22): the function CANNOT attempt a second
/// `TemporalDb::open` because it returned early in the unlink-failure arm —
/// there is exactly zero additional opens after the failed unlink.
///
/// Discriminating: all three files are present after the call; the backoff
/// sentinel is NOT written (the corrupt-undelete arm returns before the sentinel
/// write that lives in the `Err(other)` arm).
///
/// Unix-only (chmod 500 is a POSIX concept; Windows ACLs work differently).
#[test]
#[cfg(unix)]
fn test_corrupt_db_undelete_fails_returns_ok_sidecars_preserved() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Pre-create .skim-build.lock so lock acquisition succeeds even with chmod 500.
    // On POSIX, opening an EXISTING file for write (O_CREAT|O_WRONLY on an existing
    // path) does NOT require directory write permission — only directory search (x)
    // permission is needed, which chmod 500 retains.
    std::fs::write(cache_dir.join(".skim-build.lock"), b"")
        .expect("T-22/T-29: pre-create .skim-build.lock");

    // PF-012: deterministic corrupt fixture.
    let corrupt_bytes = vec![0xABu8; 1024];
    std::fs::write(cache_dir.join("temporal.db"), &corrupt_bytes)
        .expect("T-22/T-29: write corrupt temporal.db");

    // Sidecar files — must NOT be removed when main unlink fails (SE-3).
    std::fs::write(cache_dir.join("temporal.db-wal"), b"wal-sentinel-data")
        .expect("T-22/T-29: write temporal.db-wal");
    std::fs::write(cache_dir.join("temporal.db-shm"), b"shm-sentinel-data")
        .expect("T-22/T-29: write temporal.db-shm");

    // chmod 500 (r-x------): directory is readable and searchable, but not writable.
    // Attempts to create new files or delete existing files will fail with EACCES.
    std::fs::set_permissions(&cache_dir, std::fs::Permissions::from_mode(0o500))
        .expect("T-22/T-29: chmod 500 cache_dir");

    let result = rebuild_temporal_with_source(
        &CountingSource::new_empty(),
        dir.path(),
        &cache_dir,
        "ffff6666ffff6666ffff6666ffff6666ffff6666",
        super::current_epoch_secs(),
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    );

    // Restore permissions BEFORE any assertions so tempdir cleanup can succeed
    // regardless of assertion failures.
    std::fs::set_permissions(&cache_dir, std::fs::Permissions::from_mode(0o700))
        .expect("T-22/T-29: restore cache_dir permissions");

    // T-22: returns Ok(()) — bounded retry (zero re-opens after failed unlink).
    assert!(
        result.is_ok(),
        "T-22: rebuild_temporal_with_source must return Ok(()) when corrupt DB unlink fails; \
         got {result:?}"
    );

    // AC-29 / T-29: corrupt temporal.db must still be present (SE-3 main-first rule).
    assert!(
        cache_dir.join("temporal.db").exists(),
        "T-29/AC-29: temporal.db must be byte-unchanged when unlink fails (SE-3)"
    );
    let remaining_bytes =
        std::fs::read(cache_dir.join("temporal.db")).expect("T-29: read temporal.db");
    assert_eq!(
        remaining_bytes,
        vec![0xABu8; 1024],
        "T-29/AC-29: temporal.db content must be the original corrupt bytes (byte-unchanged)"
    );

    // SE-3: sidecars must still be present (never removed when main unlink fails).
    assert!(
        cache_dir.join("temporal.db-wal").exists(),
        "T-29/SE-3: temporal.db-wal must be preserved when main unlink fails"
    );
    assert!(
        cache_dir.join("temporal.db-shm").exists(),
        "T-29/SE-3: temporal.db-shm must be preserved when main unlink fails"
    );

    // Backoff sentinel must NOT be written (corrupt-undelete arm returns before
    // the sentinel write in the Err(other) arm).
    assert!(
        !cache_dir.join("temporal.db.build_backoff").exists(),
        "T-22/T-29: backoff sentinel must not be written in the corrupt-undelete arm"
    );
}

// ============================================================================
// T-26 — LOCKED DECISION: empty DB with valid HEAD is not stale
// ============================================================================

/// T-26 (LOCKED DECISION 2026-06-24): After `rebuild_temporal_with_source` on a
/// repo with zero commits, the resulting `temporal.db` must be non-stale — i.e.,
/// `temporal_db_is_stale` returns `false` on the next call.
///
/// This is the regression guard for the LOCKED DECISION: the empty-row DB with
/// `META_GIT_HEAD` set prevents the per-query rebuild loop.  Without the decision,
/// an early-return before `TemporalDb::open` would leave `temporal.db` absent or
/// without `META_GIT_HEAD`, causing `temporal_db_is_stale` to return `true` on
/// every subsequent query — a rebuild loop.
///
/// Discriminating: `temporal_db_is_stale(cache_dir, head, None)` returns `false`
/// after the call.  This is the first test in `temporal_build_tests.rs` that
/// directly asserts the staleness check (vs. the comment in
/// `test_rebuild_temporal_empty_history_writes_head_and_data_version`).
#[test]
fn test_t26_empty_db_with_head_is_not_stale() {
    use super::super::staleness::temporal_db_is_stale;

    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    let src = CountingSource::new_empty(); // zero commits
    let fake_head = "gggg7777gggg7777gggg7777gggg7777gggg7777";
    let now = super::current_epoch_secs();

    let result = rebuild_temporal_with_source(
        &src,
        dir.path(),
        &cache_dir,
        fake_head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    );
    assert!(
        result.is_ok(),
        "T-26: rebuild must return Ok(()) on zero-commit source; got {result:?}"
    );

    // Discriminating: temporal_db_is_stale must return false (LOCKED DECISION guard).
    // If this assertion fails, the rebuild loop from Finding 2 has been reintroduced.
    assert!(
        !temporal_db_is_stale(&cache_dir, fake_head, None),
        "T-26 (LOCKED DECISION): temporal_db_is_stale must return false after a rebuild \
         that produced zero rows — if it returns true, the per-query rebuild loop is back"
    );

    // Sanity: same HEAD, different call — still non-stale (idempotent).
    assert!(
        !temporal_db_is_stale(&cache_dir, fake_head, None),
        "T-26: temporal_db_is_stale must be stable across two calls with the same HEAD"
    );

    // Sanity: different HEAD is stale (new commits arrived).
    let different_head = "hhhh8888hhhh8888hhhh8888hhhh8888hhhh8888";
    assert!(
        temporal_db_is_stale(&cache_dir, different_head, None),
        "T-26: temporal_db_is_stale must return true when HEAD differs"
    );
}

// ============================================================================
// Check 3 live test — shallow→full transition detected as stale (AD-414-14)
// ============================================================================

/// Check 3 live test (AD-414-14): After `sync()` writes `META_IS_SHALLOW = "1"`,
/// `temporal_db_is_stale` with a `git_dir` that has no `shallow` file returns
/// `true` (stale — a `git fetch --unshallow` has run since the last build).
///
/// This makes the AD-414-14 Check 3 path "live" by driving it through the full
/// `rebuild_temporal_with_source` → `sync()` → `temporal_db_is_stale` chain
/// rather than directly planting meta rows.
///
/// Two discriminating assertions:
/// 1. With `git_dir/shallow` ABSENT → stale (shallow clone became full).
/// 2. With `git_dir/shallow` PRESENT → not stale (still shallow, no rebuild needed).
///
/// The test also confirms that `META_IS_SHALLOW` is absent on a non-shallow build,
/// so Check 3 is skipped entirely (the absent-row path is safe: false-negative
/// means we simply don't trigger the self-heal until the next HEAD change).
#[test]
fn test_check3_shallow_to_full_transition_detected_as_stale() {
    use super::super::staleness::temporal_db_is_stale;

    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Simulate a shallow clone: one commit, no changed files, is_shallow = true.
    let history = HistoryResult {
        commits: vec![make_commit(
            "iiii0001",
            1_000_000,
            "feat: initial (shallow)",
            &[],
        )],
        metadata: rskim_search::TemporalMetadata {
            is_shallow: true,
            commit_count: 1,
        },
    };
    let src = FixedSource { history };

    let fake_head = "iiii9999iiii9999iiii9999iiii9999iiii9999";
    let now = super::current_epoch_secs();

    let result = rebuild_temporal_with_source(
        &src,
        dir.path(),
        &cache_dir,
        fake_head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    );
    assert!(
        result.is_ok(),
        "Check 3 setup: rebuild must succeed; got {result:?}"
    );

    // Verify META_IS_SHALLOW = "1" was written by sync() (AD-414-14).
    {
        let db_path = cache_dir.join("temporal.db");
        let db = rskim_search::TemporalDb::open(&db_path)
            .expect("Check 3: temporal.db must be openable");
        let stored_shallow = db
            .get_meta(rskim_search::META_IS_SHALLOW)
            .expect("Check 3: get_meta must not fail")
            .expect("Check 3: META_IS_SHALLOW must be present after shallow rebuild");
        assert_eq!(
            stored_shallow, "1",
            "Check 3: META_IS_SHALLOW must be '1' for a shallow clone"
        );
    }

    // Simulate a fake .git directory (no `shallow` file — unshallow has run).
    let fake_git_dir = dir.path().join("fake_git");
    std::fs::create_dir_all(&fake_git_dir).expect("Check 3: create fake_git_dir");

    // Discriminating assertion 1: no `shallow` file → stale (Check 3 fires).
    assert!(
        temporal_db_is_stale(&cache_dir, fake_head, Some(&fake_git_dir)),
        "Check 3 (AD-414-14): temporal_db_is_stale must return true when META_IS_SHALLOW='1' \
         and git_dir/shallow is absent (shallow→full transition detected)"
    );

    // Discriminating assertion 2: `shallow` file exists → not stale (still shallow).
    let shallow_file = fake_git_dir.join("shallow");
    std::fs::write(&shallow_file, b"").expect("Check 3: create shallow sentinel");
    assert!(
        !temporal_db_is_stale(&cache_dir, fake_head, Some(&fake_git_dir)),
        "Check 3 (AD-414-14): temporal_db_is_stale must return false when META_IS_SHALLOW='1' \
         and git_dir/shallow is present (clone is still shallow — no rebuild needed)"
    );

    // Discriminating assertion 3: when git_dir is None, Check 3 is skipped entirely
    // (safe false-negative — the absent-row path until the next HEAD change).
    assert!(
        !temporal_db_is_stale(&cache_dir, fake_head, None),
        "Check 3 (AD-414-14): temporal_db_is_stale must return false when git_dir is None \
         (Check 3 skipped — safe false-negative)"
    );

    // Discriminating assertion 4: a non-shallow rebuild writes META_IS_SHALLOW = "0"
    // and Check 3 does NOT fire even when git_dir/shallow is absent.
    let dir2 = tempdir().unwrap();
    let cache_dir2 = dir2.path().join("cache");
    std::fs::create_dir_all(&cache_dir2).unwrap();

    let non_shallow_history = make_history(vec![]); // is_shallow = false (from make_history)
    let src2 = FixedSource {
        history: non_shallow_history,
    };
    let head2 = "jjjj0000jjjj0000jjjj0000jjjj0000jjjj0000";

    rebuild_temporal_with_source(
        &src2,
        dir2.path(),
        &cache_dir2,
        head2,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    )
    .expect("Check 3 non-shallow setup: rebuild must succeed");

    // META_IS_SHALLOW = "0" → Check 3 does not fire even with git_dir pointing to
    // a dir with no shallow file.
    let fake_git_dir2 = dir2.path().join("fake_git2");
    std::fs::create_dir_all(&fake_git_dir2).expect("Check 3: create fake_git_dir2");
    assert!(
        !temporal_db_is_stale(&cache_dir2, head2, Some(&fake_git_dir2)),
        "Check 3 (AD-414-14): temporal_db_is_stale must return false for a non-shallow DB \
         even when git_dir/shallow is absent (META_IS_SHALLOW='0' does not trigger Check 3)"
    );
}

// ============================================================================
// AC-35(c) — Check 3 linked-worktree commondir shallow probe (regression guard)
//
// Regression guard for the linked-worktree shallow bug fixed in commit 007ccf3:
// `resolve_git_dir` returns the PER-WORKTREE gitdir (.git/worktrees/<name>),
// but the `shallow` file lives in the COMMONDIR (.git/).  Checking
// `git_dir/shallow` directly caused an unbounded rebuild loop on every query
// for linked worktrees of shallow clones.  The fix uses `resolve_common_dir` to
// land on the commondir before probing.  This test pins that fix so a revert to
// `gd.join("shallow")` would be caught immediately.
// ============================================================================

/// AC-35(c): linked-worktree `commondir` pointer is followed when probing for
/// the `shallow` file; the per-worktree gitdir is NOT used as the probe root.
///
/// Two discriminating assertions:
///
/// 1. `shallow` in COMMONDIR only → `temporal_db_is_stale` returns `false`
///    (the fix works: `resolve_common_dir` navigates to the commondir and finds
///    the file).
///
/// 2. `shallow` removed from COMMONDIR, placed ONLY in the PER-WORKTREE gitdir →
///    `temporal_db_is_stale` returns `true` (stale detected via commondir, not
///    per-worktree gitdir).
///
/// Assertion 2 is the load-bearing regression guard: if the production code
/// reverted to `gd.join("shallow")`, it would read the per-worktree gitdir and
/// incorrectly return `false` (not stale) — failing this assertion.
///
/// Structure of the fake linked-worktree gitdir created by this test:
///
/// ```text
/// fake_main_git/                      ← commondir (has HEAD to satisfy sanity gate)
/// fake_main_git/worktrees/
/// fake_main_git/worktrees/linked/     ← per-worktree gitdir (what resolve_git_dir returns)
/// fake_main_git/worktrees/linked/commondir   ← content: "../.." (relative to linked/)
/// ```
///
/// `resolve_common_dir(fake_main_git/worktrees/linked)` reads `commondir`,
/// resolves `"../.."` relative to `linked/` → `fake_main_git/` (after canonicalize).
#[test]
fn test_check3_linked_worktree_commondir_shallow_probe() {
    use super::super::staleness::temporal_db_is_stale;

    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Build temporal.db with META_IS_SHALLOW = "1" so Check 3 activates.
    let history = HistoryResult {
        commits: vec![make_commit(
            "kkkk0001",
            1_000_000,
            "feat: initial (shallow)",
            &[],
        )],
        metadata: rskim_search::TemporalMetadata {
            is_shallow: true,
            commit_count: 1,
        },
    };
    let src = FixedSource { history };
    let fake_head = "kkkk1111kkkk1111kkkk1111kkkk1111kkkk1111";
    let now = super::current_epoch_secs();

    rebuild_temporal_with_source(
        &src,
        dir.path(),
        &cache_dir,
        fake_head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    )
    .expect("linked-worktree Check 3 setup: rebuild must succeed");

    // Verify META_IS_SHALLOW = "1" so Check 3 will activate on subsequent calls.
    {
        let db = rskim_search::TemporalDb::open(&cache_dir.join("temporal.db"))
            .expect("linked-worktree Check 3: temporal.db must be openable");
        let stored = db
            .get_meta(rskim_search::META_IS_SHALLOW)
            .expect("linked-worktree Check 3: get_meta must succeed")
            .expect("linked-worktree Check 3: META_IS_SHALLOW must be present");
        assert_eq!(
            stored, "1",
            "linked-worktree Check 3: META_IS_SHALLOW must be '1' after shallow build"
        );
    }

    // Build fake linked-worktree gitdir structure.
    //   fake_main_git/           ← the commondir
    //   fake_main_git/HEAD       ← sanity gate for resolve_common_dir
    //   fake_main_git/worktrees/linked/            ← per-worktree gitdir
    //   fake_main_git/worktrees/linked/commondir   ← "../.." (relative to linked/)
    let main_git = dir.path().join("fake_main_git");
    let per_worktree_git = main_git.join("worktrees").join("linked");
    std::fs::create_dir_all(&per_worktree_git)
        .expect("linked-worktree Check 3: create per-worktree gitdir");

    // HEAD file required by resolve_common_dir's sanity gate
    // (canonical.join("HEAD").is_file() must be true for the commondir target).
    std::fs::write(main_git.join("HEAD"), b"ref: refs/heads/main\n")
        .expect("linked-worktree Check 3: write HEAD sentinel");

    // commondir file: "../.." is the relative path from `linked/` to `fake_main_git/`.
    std::fs::write(per_worktree_git.join("commondir"), b"../..\n")
        .expect("linked-worktree Check 3: write commondir");

    // ── Assertion 1: shallow file in COMMONDIR only → NOT stale ──────────────
    // resolve_common_dir(per_worktree_git) → main_git; main_git/shallow exists.
    std::fs::write(main_git.join("shallow"), b"kkkk0001 (shallow sentinel)\n")
        .expect("linked-worktree Check 3: write commondir shallow file");

    assert!(
        !temporal_db_is_stale(&cache_dir, fake_head, Some(&per_worktree_git)),
        "AC-35(c): temporal_db_is_stale must return false when META_IS_SHALLOW='1' \
         and the COMMONDIR contains a non-empty shallow file — \
         resolve_common_dir must follow the commondir pointer, not check the \
         per-worktree gitdir directly"
    );

    // ── Assertion 2: remove commondir shallow → stale (unshallow detected) ───
    std::fs::remove_file(main_git.join("shallow"))
        .expect("linked-worktree Check 3: remove commondir shallow file");

    assert!(
        temporal_db_is_stale(&cache_dir, fake_head, Some(&per_worktree_git)),
        "AC-35(c): temporal_db_is_stale must return true when META_IS_SHALLOW='1' \
         and the commondir has no shallow file (unshallow transition)"
    );

    // ── Assertion 3 (regression guard): shallow ONLY in per-worktree gitdir ──
    // If the code reverted to `gd.join("shallow")`, this would wrongly return
    // false (not stale), because `per_worktree_git/shallow` would exist.
    // The fix uses resolve_common_dir which returns main_git, so
    // `main_git/shallow` is checked — and it's absent → correctly stale.
    std::fs::write(
        per_worktree_git.join("shallow"),
        b"wrong location sentinel\n",
    )
    .expect("linked-worktree Check 3: write per-worktree shallow (wrong location)");

    assert!(
        temporal_db_is_stale(&cache_dir, fake_head, Some(&per_worktree_git)),
        "AC-35(c) REGRESSION GUARD: temporal_db_is_stale must return true when \
         `shallow` exists only in the per-worktree gitdir (wrong location) but NOT \
         in the commondir — the fix probes via resolve_common_dir, so the per-worktree \
         shallow file must NOT satisfy the check; a revert to gd.join('shallow') \
         would incorrectly return false here"
    );
}

// ============================================================================
// AC-35(c) no-loop end-to-end: rebuild triggered by Check 3 resolves the stale
// state in exactly one rebuild, not an infinite loop.
// ============================================================================

/// AC-35(c) no-loop end-to-end: Check 3 triggers exactly one rebuild.
///
/// This test runs two `rebuild_temporal_with_source` calls on the SAME cache
/// directory and verifies that after the second call (the one Check 3 would
/// trigger), `temporal_db_is_stale` returns `false` — confirming no further
/// rebuild is triggered ("exactly one rebuild, no loop").
///
/// Sequence:
/// 1. Build 1: shallow source → `META_IS_SHALLOW = "1"` stored in `temporal.db`.
/// 2. Staleness check with no `shallow` file in `git_dir` → `true` (stale;
///    Check 3 detected the unshallow transition and asks for a rebuild).
/// 3. Build 2 (the Check 3 self-heal): non-shallow source, same HEAD, same
///    `cache_dir` → `META_IS_SHALLOW = "0"` stored in `temporal.db`.
/// 4. Staleness check with no `shallow` file present → `false` (not stale;
///    Check 3 does NOT fire because `META_IS_SHALLOW = "0"`).
/// 5. Staleness check again → `false` (idempotent; no loop).
///
/// This is the missing end-to-end assertion for AC-35(c): it exercises "the
/// rebuild that Check 3 triggers" and re-checks staleness after that rebuild to
/// prove the "exactly one rebuild, no loop" contract.  Prior tests in this file
/// only exercise `temporal_db_is_stale` directly; this test proves the full
/// Build-1 → stale → Build-2 → not-stale cycle that pins the loop invariant.
#[test]
fn test_check3_self_heal_no_loop() {
    use super::super::staleness::temporal_db_is_stale;

    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // ── Build 1: shallow clone (META_IS_SHALLOW = "1") ────────────────────
    let shallow_history = HistoryResult {
        commits: vec![make_commit(
            "pppp0001",
            1_000_000,
            "feat: initial (shallow)",
            &[],
        )],
        metadata: rskim_search::TemporalMetadata {
            is_shallow: true,
            commit_count: 1,
        },
    };
    let src1 = FixedSource {
        history: shallow_history,
    };
    let fake_head = "pppp9999pppp9999pppp9999pppp9999pppp9999";
    let now = super::current_epoch_secs();

    rebuild_temporal_with_source(
        &src1,
        dir.path(),
        &cache_dir,
        fake_head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    )
    .expect("Check 3 no-loop: Build 1 (shallow) must succeed");

    // Sanity: META_IS_SHALLOW = "1" written by Build 1.
    {
        let db = rskim_search::TemporalDb::open(&cache_dir.join("temporal.db"))
            .expect("Check 3 no-loop: temporal.db must be openable after Build 1");
        let stored = db
            .get_meta(rskim_search::META_IS_SHALLOW)
            .expect("Check 3 no-loop: get_meta must succeed")
            .expect("Check 3 no-loop: META_IS_SHALLOW must be present after Build 1");
        assert_eq!(
            stored, "1",
            "Check 3 no-loop: META_IS_SHALLOW must be '1' after Build 1 (shallow source)"
        );
    }

    // ── Step 2: staleness check — Check 3 fires (no shallow file) ─────────
    // Fake git dir with no shallow file simulates the unshallow transition.
    let fake_git_dir = dir.path().join("fake_git");
    std::fs::create_dir_all(&fake_git_dir).expect("Check 3 no-loop: create fake_git_dir");

    assert!(
        temporal_db_is_stale(&cache_dir, fake_head, Some(&fake_git_dir)),
        "AC-35(c) no-loop: temporal_db_is_stale must return true after Build 1 with \
         META_IS_SHALLOW='1' and no shallow file (Check 3 fires — stale detected)"
    );

    // ── Build 2: the self-heal triggered by Check 3 (non-shallow source) ──
    // Same HEAD, same cache_dir as Build 1. After this rebuild, META_IS_SHALLOW = "0".
    let full_history = HistoryResult {
        commits: vec![make_commit(
            "pppp0001",
            1_000_000,
            "feat: initial (full clone)",
            &["src/lib.rs"],
        )],
        metadata: rskim_search::TemporalMetadata {
            is_shallow: false,
            commit_count: 1,
        },
    };
    let src2 = FixedSource {
        history: full_history,
    };

    rebuild_temporal_with_source(
        &src2,
        dir.path(),
        &cache_dir,
        fake_head,
        now,
        ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    )
    .expect("Check 3 no-loop: Build 2 (non-shallow self-heal) must succeed");

    // Sanity: META_IS_SHALLOW = "0" written by Build 2.
    {
        let db = rskim_search::TemporalDb::open(&cache_dir.join("temporal.db"))
            .expect("Check 3 no-loop: temporal.db must be openable after Build 2");
        let stored = db
            .get_meta(rskim_search::META_IS_SHALLOW)
            .expect("Check 3 no-loop: get_meta must succeed after Build 2")
            .expect("Check 3 no-loop: META_IS_SHALLOW must be present after Build 2");
        assert_eq!(
            stored, "0",
            "Check 3 no-loop: META_IS_SHALLOW must be '0' after Build 2 (non-shallow self-heal)"
        );
    }

    // ── Step 4: staleness check — Check 3 must NOT fire ───────────────────
    // fake_git_dir still has no shallow file. But META_IS_SHALLOW = "0", so
    // Check 3 is skipped — temporal_db_is_stale must return false.
    assert!(
        !temporal_db_is_stale(&cache_dir, fake_head, Some(&fake_git_dir)),
        "AC-35(c) no-loop: temporal_db_is_stale must return false after Build 2 \
         (META_IS_SHALLOW='0') even with no shallow file in fake_git_dir — Check 3 \
         does not fire when META_IS_SHALLOW is not '1'; this is the core no-loop assertion"
    );

    // ── Step 5: idempotent — second staleness check must also return false ─
    assert!(
        !temporal_db_is_stale(&cache_dir, fake_head, Some(&fake_git_dir)),
        "AC-35(c) no-loop: temporal_db_is_stale must return false on a second consecutive \
         call (idempotent — the 'no loop' guarantee: no further rebuild is triggered)"
    );
}

// ============================================================================
// AC-16 — zero_row_notice text contract (direct unit tests)
//
// These tests call zero_row_notice() directly to pin the AC-16 text contract:
//   - Case (i):  no commits → notice must NOT contain "shallow"/"unshallow"
//   - Case (ii): shallow + 0 pre_ghost_hotspot → notice MUST contain "shallow"
//   - Case (iii): pre_ghost_hotspot > 0 → notice must NOT contain "shallow"/"unshallow"
//   - All cases: notice is a single line (no embedded newlines)
//
// Without these tests, all three existing T-16 tests pass unchanged if
// zero_row_notice() is deleted or all cases emit the same wrong attribution.
// ============================================================================

/// AC-16 text contract — Case (i): zero commits → notice has no "shallow" wording.
///
/// `zero_row_notice` with an empty commit list must route through
/// `DegradedReason::Empty` with no shallow detail, producing text that
/// does NOT contain "shallow" or "unshallow".
#[test]
fn test_zero_row_notice_case_i_no_shallow_text() {
    let history = HistoryResult {
        commits: vec![],
        metadata: rskim_search::TemporalMetadata {
            is_shallow: false,
            commit_count: 0,
        },
    };
    let notice = super::zero_row_notice(&history, 0, false);

    assert!(
        !notice.to_ascii_lowercase().contains("shallow"),
        "AC-16(i): zero_row_notice for zero-commit history must NOT contain 'shallow'; \
         got: {notice:?}"
    );
    assert!(
        !notice.to_ascii_lowercase().contains("unshallow"),
        "AC-16(i): zero_row_notice for zero-commit history must NOT contain 'unshallow'; \
         got: {notice:?}"
    );
    // Notice must be non-empty — a deleted zero_row_notice would produce "".
    assert!(
        !notice.is_empty(),
        "AC-16(i): zero_row_notice must return a non-empty string for zero-commit history"
    );
}

/// AC-16 text contract — Case (ii): shallow + 0 pre_ghost_hotspot → notice
/// contains "shallow" wording.
///
/// When `is_shallow == true` AND `pre_ghost_hotspot == 0`, `zero_row_notice`
/// must produce text that names shallow clones as the attribution — the caller
/// (the user) needs to know the cause is the shallow state, not missing history.
#[test]
fn test_zero_row_notice_case_ii_has_shallow_text() {
    let history = HistoryResult {
        commits: vec![make_commit(
            "llll0001",
            1_000_000,
            "feat: initial (shallow)",
            &[], // no changed files — typical in shallow-fetch scenarios
        )],
        metadata: rskim_search::TemporalMetadata {
            is_shallow: true,
            commit_count: 1,
        },
    };
    let notice = super::zero_row_notice(&history, 0, true);

    assert!(
        notice.to_ascii_lowercase().contains("shallow"),
        "AC-16(ii): zero_row_notice for shallow + 0 pre_ghost_hotspot must contain 'shallow'; \
         got: {notice:?}"
    );
    assert!(
        !notice.is_empty(),
        "AC-16(ii): zero_row_notice must return a non-empty string"
    );
}

/// AC-16 text contract — Case (iii): pre_ghost_hotspot > 0 → notice has no
/// "shallow" wording.
///
/// When rows were computed but the ghost filter dropped all of them, the
/// attribution is the ghost filter, NOT the shallow state.  The notice must
/// NOT contain "shallow" or "unshallow" even when `is_shallow` happens to be
/// true (ghost filter is the proximate cause).
#[test]
fn test_zero_row_notice_case_iii_no_shallow_text() {
    let history = HistoryResult {
        commits: vec![
            make_commit("mmmm0001", 1_000_000, "feat: initial", &["phantom.rs"]),
            make_commit("mmmm0002", 1_000_001, "feat: update", &["phantom.rs"]),
        ],
        metadata: rskim_search::TemporalMetadata {
            is_shallow: false,
            commit_count: 2,
        },
    };
    // pre_ghost_hotspot = 1 → case (iii) fires regardless of is_shallow.
    let notice = super::zero_row_notice(&history, 1, false);

    assert!(
        !notice.to_ascii_lowercase().contains("shallow"),
        "AC-16(iii): zero_row_notice for ghost-filter-zero must NOT contain 'shallow'; \
         got: {notice:?}"
    );
    assert!(
        !notice.to_ascii_lowercase().contains("unshallow"),
        "AC-16(iii): zero_row_notice for ghost-filter-zero must NOT contain 'unshallow'; \
         got: {notice:?}"
    );
    assert!(
        !notice.is_empty(),
        "AC-16(iii): zero_row_notice must return a non-empty string"
    );
}

/// AC-16 single-line contract: all three cases must produce exactly one line
/// (no embedded newlines).
///
/// A multi-line notice (or a notice containing '\n') would violate the
/// "exactly one stderr line" contract of AC-16.
#[test]
fn test_zero_row_notice_all_cases_single_line() {
    // Case (i): zero commits
    let history_i = HistoryResult {
        commits: vec![],
        metadata: rskim_search::TemporalMetadata {
            is_shallow: false,
            commit_count: 0,
        },
    };
    let notice_i = super::zero_row_notice(&history_i, 0, false);
    assert!(
        !notice_i.contains('\n'),
        "AC-16(i): zero_row_notice must be a single line (no '\\n'); got: {notice_i:?}"
    );

    // Case (ii): shallow, no changed files
    let history_ii = HistoryResult {
        commits: vec![make_commit("nnnn0001", 1_000_000, "feat: shallow", &[])],
        metadata: rskim_search::TemporalMetadata {
            is_shallow: true,
            commit_count: 1,
        },
    };
    let notice_ii = super::zero_row_notice(&history_ii, 0, true);
    assert!(
        !notice_ii.contains('\n'),
        "AC-16(ii): zero_row_notice must be a single line (no '\\n'); got: {notice_ii:?}"
    );

    // Case (iii): ghost filter drops all
    let history_iii = HistoryResult {
        commits: vec![make_commit(
            "oooo0001",
            1_000_000,
            "feat: phantom",
            &["phantom.rs"],
        )],
        metadata: rskim_search::TemporalMetadata {
            is_shallow: false,
            commit_count: 1,
        },
    };
    let notice_iii = super::zero_row_notice(&history_iii, 1, false);
    assert!(
        !notice_iii.contains('\n'),
        "AC-16(iii): zero_row_notice must be a single line (no '\\n'); got: {notice_iii:?}"
    );
}

/// AC-16 case (ii) FX-SHALLOW integration cross-check: a real `git clone
/// --depth 1` produces a shallow repo where `META_IS_SHALLOW = "1"` and
/// `top_hotspots` returns zero rows (case (ii) state confirmed at the DB level).
///
/// This is the "CLI cross-check of (ii) on FX-SHALLOW" that the plan requires
/// (T-16 row): it drives `rebuild_temporal` with a real shallow clone to confirm
/// the `is_shallow` metadata survives the full `parse_history` → `sync()` path,
/// not just the `FixedSource` shim.  The stderr text is pinned separately by
/// `test_zero_row_notice_case_ii_has_shallow_text` (above).
///
/// Skipped automatically when `git` is not available on PATH.
#[test]
fn test_t16_case_ii_fx_shallow_integration() {
    // Guard: git must be available.
    let git_check = Command::new("git").arg("--version").output();
    if git_check.map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("SKIP test_t16_case_ii_fx_shallow_integration: git not available");
        return;
    }

    // Create a real source git repo with at least one commit.
    let source_dir = tempdir().unwrap();
    let source_head = create_real_git_repo(
        source_dir.path(),
        &[
            ("feat: first", &[("src/lib.rs", "pub fn a() {}")]),
            ("feat: second", &[("src/lib.rs", "pub fn b() {}")]),
        ],
    );
    if source_head.is_empty() {
        eprintln!(
            "SKIP test_t16_case_ii_fx_shallow_integration: git commit failed \
             (no identity configured?)"
        );
        return;
    }

    // Shallow-clone the source repo with depth=1 (FX-SHALLOW fixture).
    let clone_dir = tempdir().unwrap();
    let clone_path = clone_dir.path().join("clone");
    let clone_status = Command::new("git")
        .args([
            "clone",
            "--quiet",
            "--depth",
            "1",
            // file:// URL disables local-clone optimisation so git writes .git/shallow.
            &format!("file://{}", source_dir.path().display()),
            clone_path.to_str().expect("clone path must be valid UTF-8"),
        ])
        .status();
    match clone_status {
        Ok(s) if !s.success() => {
            eprintln!(
                "SKIP test_t16_case_ii_fx_shallow_integration: \
                 git clone --depth 1 failed (exit {s})"
            );
            return;
        }
        Err(e) => {
            eprintln!(
                "SKIP test_t16_case_ii_fx_shallow_integration: \
                 git clone --depth 1 could not be executed ({e})"
            );
            return;
        }
        Ok(_) => {}
    }

    // Verify .git/shallow exists (confirms this is a genuine shallow clone).
    let shallow_file = clone_path.join(".git").join("shallow");
    if !shallow_file.exists() {
        eprintln!(
            "SKIP test_t16_case_ii_fx_shallow_integration: \
             .git/shallow not created by clone (unexpected git behaviour)"
        );
        return;
    }

    // Read the HEAD of the shallow clone.
    let head_out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&clone_path)
        .output()
        .expect("FX-SHALLOW: git rev-parse HEAD must succeed");
    assert!(
        head_out.status.success(),
        "FX-SHALLOW: git rev-parse HEAD failed"
    );
    let clone_head = String::from_utf8_lossy(&head_out.stdout).trim().to_string();
    assert_eq!(
        clone_head.len(),
        40,
        "FX-SHALLOW: HEAD must be a 40-char SHA"
    );

    // Run rebuild_temporal on the FX-SHALLOW clone.
    let cache_dir = clone_dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let now = super::current_epoch_secs();
    let result = rebuild_temporal(&clone_path, &cache_dir, &clone_head, now);
    assert!(
        result.is_ok(),
        "FX-SHALLOW: rebuild_temporal must return Ok(()) on a shallow clone; got {result:?}"
    );

    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path)
        .expect("FX-SHALLOW: temporal.db must be openable after rebuild");

    // Discriminating: META_IS_SHALLOW must be "1" (gix detects the shallow state).
    let stored_shallow = db
        .get_meta(rskim_search::META_IS_SHALLOW)
        .expect("FX-SHALLOW: get_meta(META_IS_SHALLOW) must succeed")
        .expect("FX-SHALLOW: META_IS_SHALLOW must be present after rebuild on shallow clone");
    assert_eq!(
        stored_shallow, "1",
        "FX-SHALLOW: META_IS_SHALLOW must be '1' for a git clone --depth 1 shallow clone \
         (gix must detect is_shallow=true via .git/shallow)"
    );

    // META_GIT_HEAD must be set (prevents retry loop).
    let stored_head = db
        .get_meta(rskim_search::META_GIT_HEAD)
        .expect("FX-SHALLOW: get_meta(META_GIT_HEAD) must succeed")
        .expect("FX-SHALLOW: META_GIT_HEAD must be present");
    assert_eq!(
        stored_head, clone_head,
        "FX-SHALLOW: META_GIT_HEAD must equal the clone HEAD"
    );

    // Zero hotspot rows: the single commit in a depth-1 clone has no accessible
    // parent, so changed_files == [] for all commits (case (ii) condition).
    let hotspots = db
        .top_hotspots(20)
        .expect("FX-SHALLOW: top_hotspots must succeed");
    assert!(
        hotspots.is_empty(),
        "FX-SHALLOW: temporal.db must have zero hotspot rows for a depth-1 shallow clone \
         (changed_files is empty because the parent commit is not in the shallow history); \
         got {} rows",
        hotspots.len()
    );
}
