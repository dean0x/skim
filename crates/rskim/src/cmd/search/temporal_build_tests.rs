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

use super::{build_cochange_rows, build_hotspot_rows, build_risk_rows, rebuild_temporal};

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

/// AC11: Joint presence — verify risk row field mapping.
#[test]
fn test_join_risk_row_field_mapping() {
    let mut risk_scores: HashMap<String, FileRiskScores> = HashMap::new();
    risk_scores.insert(
        "p.rs".to_string(),
        FileRiskScores {
            hotspot: 0.7,
            fix_density: 0.375, // 3/8
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
    assert!(
        (row.risk_score - 0.375).abs() < 1e-9,
        "risk_score must equal fix_density (0.375)"
    );
    assert_eq!(row.total_commits, 8, "total_commits from FileTemporalStats");
    assert_eq!(row.fix_commits, 3, "fix_commits from FileTemporalStats");
    assert!(
        (row.fix_density - 0.375).abs() < 1e-9,
        "fix_density from FileRiskScores"
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

/// Create a real git repository with commits via git subprocess.
///
/// `git init` + `git config` + `git add` + `git commit` for each commit entry.
/// `commit_files` is `(message, &[(filename, content)])`.
fn create_real_git_repo(dir: &std::path::Path, commit_files: &[(&str, &[(&str, &str)])]) -> String {
    init_git_repo(dir);

    for (msg, files) in commit_files {
        for (name, content) in *files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create dir");
            }
            std::fs::write(path, content).expect("write file");
            Command::new("git")
                .args(["add", name])
                .current_dir(dir)
                .output()
                .expect("git add");
        }
        Command::new("git")
            .args(["commit", "-m", msg])
            .current_dir(dir)
            .output()
            .expect("git commit");
    }

    // Return the current HEAD SHA.
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("git rev-parse HEAD");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
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
/// temporal.db is NOT created (no data to write on a non-git root).
#[test]
fn test_rebuild_temporal_nongit_returns_ok() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // No git repo here — GixSource::parse_history will fail.
    let now = super::current_epoch_secs();
    let result = rebuild_temporal(
        dir.path(),
        &cache_dir,
        "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        now,
    );

    assert!(
        result.is_ok(),
        "rebuild_temporal must return Ok(()) on non-git directory (AC7), got: {result:?}"
    );
    // temporal.db must NOT be created — parse_history fails before TemporalDb::open.
    let db_path = cache_dir.join("temporal.db");
    assert!(
        !db_path.exists(),
        "temporal.db must not be created on non-git root (AC7 postcondition)"
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

/// AC13 — 90-day lookback: changes_90d reflects only in-window commits.
///
/// Two recent commits (within 90d) and two old commits (set via commit date
/// manipulation — we use the git committer date env var).
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
fn test_rebuild_temporal_90d_cutoff() {
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
    // risk_score must still be the decay-weighted fix_density for ranking.
    assert!(
        (row.risk_score - 0.9).abs() < 1e-9,
        "risk_score must be decay-weighted fix_density (0.9 from FileRiskScores), got {:.4}",
        row.risk_score
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
