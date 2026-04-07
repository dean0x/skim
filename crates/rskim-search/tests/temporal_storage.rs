//! Integration tests for `temporal::storage::TemporalDb`.
//!
//! All tests build real SQLite databases in `TempDir`s. Tests that exercise the
//! full build pipeline create real git repos via the git CLI using
//! `temporal_test_helpers`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod temporal_test_helpers;

use rskim_search::temporal::{ScoreKind, TemporalDb};
use rskim_search::SearchError;
use std::path::PathBuf;
use tempfile::TempDir;
use temporal_test_helpers::{build_fixture_repo, FixtureCommit};

// ============================================================================
// Helpers
// ============================================================================

/// Unix epoch seconds for recent timestamps (avoids 90-day exclusion window).
fn recent_ts(days_ago: i64) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_secs() as i64;
    now - days_ago * 86_400
}

/// Build a 4-commit fixture repo where a.rs and b.rs always change together.
///
/// Commits 1-4: a.rs + b.rs change together.
/// Commit 3 has "fix:" prefix to populate the risk table.
fn build_cochange_fixture(dir: &std::path::Path) {
    build_fixture_repo(
        dir,
        &[
            FixtureCommit {
                message: "feat: add a and b",
                changes: vec![("a.rs", "fn a() {}"), ("b.rs", "fn b() {}")],
                timestamp_override: Some(recent_ts(20)),
            },
            FixtureCommit {
                message: "refactor: update a and b",
                changes: vec![("a.rs", "fn a() { 1 }"), ("b.rs", "fn b() { 2 }")],
                timestamp_override: Some(recent_ts(15)),
            },
            FixtureCommit {
                message: "fix: bug in a and b",
                changes: vec![("a.rs", "fn a() { 2 }"), ("b.rs", "fn b() { 3 }")],
                timestamp_override: Some(recent_ts(10)),
            },
            FixtureCommit {
                message: "chore: cleanup a and b",
                changes: vec![("a.rs", "fn a() { 3 }"), ("b.rs", "fn b() { 4 }")],
                timestamp_override: Some(recent_ts(5)),
            },
        ],
    );
}

// ============================================================================
// 1. Schema creation
// ============================================================================

#[test]
fn storage_open_creates_schema() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("temporal.db");

    let _db = TemporalDb::open(&db_path).expect("open");

    // Verify all expected tables exist via sqlite_master.
    let conn = rusqlite::Connection::open(&db_path).expect("reopen for verification");
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .expect("prepare");
    let names: Vec<String> = stmt
        .query_map([], |row: &rusqlite::Row<'_>| row.get(0))
        .expect("query_map")
        .map(|r| r.expect("row"))
        .collect();

    assert!(
        names.contains(&"file_paths".to_string()),
        "missing file_paths; got {names:?}"
    );
    assert!(
        names.contains(&"cochange".to_string()),
        "missing cochange; got {names:?}"
    );
    assert!(
        names.contains(&"hotspot".to_string()),
        "missing hotspot; got {names:?}"
    );
    assert!(
        names.contains(&"risk".to_string()),
        "missing risk; got {names:?}"
    );
    assert!(
        names.contains(&"meta".to_string()),
        "missing meta; got {names:?}"
    );
}

// ============================================================================
// 2. Migration idempotency
// ============================================================================

#[test]
fn storage_migration_runs_once() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("temporal.db");

    // Open twice — second open must not fail or re-run migrations.
    let _db1 = TemporalDb::open(&db_path).expect("first open");
    let _db2 = TemporalDb::open(&db_path).expect("second open");

    let conn = rusqlite::Connection::open(&db_path).expect("reopen");
    let version: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("user_version");
    assert_eq!(version, 1, "schema version must be 1 after two opens");
}

// ============================================================================
// 3. WAL mode
// ============================================================================

#[test]
fn storage_wal_mode_enabled() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("temporal.db");
    let _db = TemporalDb::open(&db_path).expect("open");

    // Re-open raw connection and verify journal_mode.
    let conn = rusqlite::Connection::open(&db_path).expect("reopen");
    let mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("journal_mode");
    assert_eq!(mode, "wal", "expected WAL mode, got {mode}");
}

// ============================================================================
// 4. Foreign key enforcement
// ============================================================================

#[test]
fn storage_foreign_keys_enforced() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("temporal.db");
    let _db = TemporalDb::open(&db_path).expect("open");

    // Re-open with FK enforcement and try to insert into cochange with invalid FK.
    let conn = rusqlite::Connection::open(&db_path).expect("reopen");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("fk pragma");

    let result = conn.execute(
        "INSERT INTO cochange (file_a, file_b, co_occurrences, jaccard) VALUES (999, 1000, 1, 0.5)",
        [],
    );
    assert!(
        result.is_err(),
        "inserting cochange with nonexistent file_a must fail with FK enforcement"
    );
}

// ============================================================================
// 5. CHECK constraint (file_a < file_b)
// ============================================================================

#[test]
fn storage_cochange_check_constraint() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("temporal.db");
    let _db = TemporalDb::open(&db_path).expect("open");

    let conn = rusqlite::Connection::open(&db_path).expect("reopen");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("fk pragma");

    // Insert two valid file_paths first.
    conn.execute("INSERT INTO file_paths (path) VALUES ('x.rs')", [])
        .expect("insert x.rs");
    conn.execute("INSERT INTO file_paths (path) VALUES ('y.rs')", [])
        .expect("insert y.rs");

    // file_a = 2, file_b = 1 → violates CHECK (file_a < file_b).
    let result = conn.execute(
        "INSERT INTO cochange (file_a, file_b, co_occurrences, jaccard) VALUES (2, 1, 2, 0.5)",
        [],
    );
    assert!(
        result.is_err(),
        "cochange with file_a >= file_b must be rejected by CHECK constraint"
    );
}

// ============================================================================
// 6. Build from empty repo
// ============================================================================

#[test]
fn storage_build_from_empty_repo() {
    let repo_dir = TempDir::new().expect("repo tempdir");
    let db_dir = TempDir::new().expect("db tempdir");

    // Initialize an empty git repo.
    build_fixture_repo(repo_dir.path(), &[]);

    let db_path = db_dir.path().join("temporal.db");
    let db = TemporalDb::build(repo_dir.path(), &db_path, 365).expect("build empty repo");

    let hotspots = db.load_hotspots(10).expect("load_hotspots");
    assert!(hotspots.is_empty(), "empty repo must yield no hotspots");
}

// ============================================================================
// 7. Full roundtrip
// ============================================================================

#[test]
fn storage_build_full_roundtrip() {
    let repo_dir = TempDir::new().expect("repo tempdir");
    let db_dir = TempDir::new().expect("db tempdir");

    build_cochange_fixture(repo_dir.path());

    let db_path = db_dir.path().join("temporal.db");
    let db = TemporalDb::build(repo_dir.path(), &db_path, 365).expect("build");

    // --- blast radius ---
    let partners = db
        .load_blast_radius(&PathBuf::from("a.rs"), 10)
        .expect("blast radius for a.rs");
    assert_eq!(
        partners.len(),
        1,
        "a.rs should have exactly b.rs as partner; got {partners:?}"
    );
    let (partner_path, jaccard) = &partners[0];
    assert_eq!(
        partner_path,
        &PathBuf::from("b.rs"),
        "expected b.rs, got {partner_path:?}"
    );
    assert!(
        *jaccard > 0.0 && *jaccard <= 1.0,
        "jaccard must be in (0, 1]; got {jaccard}"
    );

    // --- hotspots ---
    let hotspots = db.load_hotspots(10).expect("load_hotspots");
    assert!(!hotspots.is_empty(), "hotspots must not be empty");
    for (_, score) in &hotspots {
        assert!(
            *score >= 0.0 && *score <= 1.0,
            "hotspot score out of [0,1]: {score}"
        );
    }

    // --- risk ---
    let risks = db.load_risk(10).expect("load_risk");
    assert!(!risks.is_empty(), "risk must not be empty");
    for (_, score) in &risks {
        assert!(
            *score >= 0.0 && *score <= 1.0,
            "risk score out of [0,1]: {score}"
        );
    }

    // --- meta ---
    let hash = db.meta("last_commit_hash").expect("meta hash");
    assert!(hash.is_some(), "last_commit_hash must be set");
    assert!(
        !hash.unwrap().is_empty(),
        "last_commit_hash must not be empty"
    );

    let count_str = db.meta("commits_analyzed").expect("meta count");
    let count: usize = count_str
        .expect("commits_analyzed must be set")
        .parse()
        .expect("parse commits_analyzed");
    assert_eq!(count, 4, "expected 4 commits analyzed");
}

// ============================================================================
// 8. Reproducible IDs
// ============================================================================

#[test]
fn storage_reproducible_ids() {
    let repo_dir = TempDir::new().expect("repo tempdir");
    build_cochange_fixture(repo_dir.path());

    let db_dir1 = TempDir::new().expect("db tempdir 1");
    let db_dir2 = TempDir::new().expect("db tempdir 2");

    let db_path1 = db_dir1.path().join("temporal.db");
    let db_path2 = db_dir2.path().join("temporal.db");

    TemporalDb::build(repo_dir.path(), &db_path1, 365).expect("build 1");
    TemporalDb::build(repo_dir.path(), &db_path2, 365).expect("build 2");

    // Read both id maps and compare.
    let load_ids = |path: &std::path::Path| {
        let conn = rusqlite::Connection::open(path).expect("open");
        let mut stmt = conn
            .prepare("SELECT temporal_file_id, path FROM file_paths ORDER BY temporal_file_id")
            .expect("prepare");
        stmt.query_map([], |row: &rusqlite::Row<'_>| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .expect("query_map")
        .map(|r| r.expect("row"))
        .collect::<Vec<_>>()
    };

    let ids1 = load_ids(&db_path1);
    let ids2 = load_ids(&db_path2);

    assert_eq!(
        ids1, ids2,
        "temporal_file_id assignments must be identical across two builds of the same repo"
    );
}

// ============================================================================
// 9. Rebuild overwrites old data
// ============================================================================

#[test]
fn storage_rebuild_overwrites_old_data() {
    let repo_dir = TempDir::new().expect("repo tempdir");
    let db_dir = TempDir::new().expect("db tempdir");

    build_cochange_fixture(repo_dir.path());
    let db_path = db_dir.path().join("temporal.db");

    // First build.
    TemporalDb::build(repo_dir.path(), &db_path, 365).expect("build 1");

    // Insert garbage directly into the first DB (simulating corruption).
    {
        let conn = rusqlite::Connection::open(&db_path).expect("open for corruption");
        conn.execute(
            "INSERT INTO file_paths (path) VALUES ('garbage_file_that_must_not_survive.rs')",
            [],
        )
        .expect("insert garbage");
    }

    // Rebuild — must start fresh.
    let db2 = TemporalDb::build(repo_dir.path(), &db_path, 365).expect("build 2");

    let hotspots = db2.load_hotspots(100).expect("load_hotspots after rebuild");
    let paths: Vec<_> = hotspots
        .iter()
        .map(|(p, _)| p.to_string_lossy().to_string())
        .collect();
    assert!(
        !paths.iter().any(|p| p.contains("garbage")),
        "garbage file must not survive rebuild; paths = {paths:?}"
    );
}

// ============================================================================
// 10. load_score_for missing file
// ============================================================================

#[test]
fn storage_load_score_for_missing_file() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("temporal.db");
    let db = TemporalDb::open(&db_path).expect("open");

    let result = db
        .load_score_for(&PathBuf::from("nonexistent.rs"), ScoreKind::Hotspot)
        .expect("load_score_for");
    assert!(result.is_none(), "missing file must return Ok(None)");
}

// ============================================================================
// 11. Schema version too new rejected
// ============================================================================

#[test]
fn storage_schema_version_too_new_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("temporal.db");

    // Create and populate schema normally.
    {
        let _db = TemporalDb::open(&db_path).expect("initial open");
    }

    // Manually bump user_version to a future version.
    {
        let conn = rusqlite::Connection::open(&db_path).expect("reopen for version bump");
        conn.pragma_update(None, "user_version", 99u32)
            .expect("set user_version");
    }

    // Re-open must fail with CorruptedIndex.
    let result = TemporalDb::open(&db_path);
    assert!(result.is_err(), "must fail with newer schema version");
    let err = result.unwrap_err();
    assert!(
        matches!(err, SearchError::CorruptedIndex { .. }),
        "expected CorruptedIndex, got {err:?}"
    );
}

// ============================================================================
// 12. Blast radius symmetry
// ============================================================================

#[test]
fn storage_blast_radius_symmetric() {
    let repo_dir = TempDir::new().expect("repo tempdir");
    let db_dir = TempDir::new().expect("db tempdir");

    build_cochange_fixture(repo_dir.path());

    let db_path = db_dir.path().join("temporal.db");
    let db = TemporalDb::build(repo_dir.path(), &db_path, 365).expect("build");

    let partners_a = db
        .load_blast_radius(&PathBuf::from("a.rs"), 10)
        .expect("blast radius a.rs");
    let partners_b = db
        .load_blast_radius(&PathBuf::from("b.rs"), 10)
        .expect("blast radius b.rs");

    // a.rs sees b.rs as partner.
    assert!(
        partners_a.iter().any(|(p, _)| p == &PathBuf::from("b.rs")),
        "a.rs blast radius must contain b.rs; got {partners_a:?}"
    );
    // b.rs sees a.rs as partner.
    assert!(
        partners_b.iter().any(|(p, _)| p == &PathBuf::from("a.rs")),
        "b.rs blast radius must contain a.rs; got {partners_b:?}"
    );
}
