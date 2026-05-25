//! Tests for [`TemporalDb`] — schema, CRUD, sync, persistence, and performance.
//!
//! Uses `tempfile::TempDir` for isolation so every test gets a fresh database.
//! Performance tests enforce the 10k-row acceptance criteria from the plan.

#![allow(clippy::unwrap_used)]

use std::time::Instant;

use tempfile::TempDir;

use super::{
    CochangeRow, HotspotRow, META_GIT_HEAD, META_LAST_UPDATED, RiskRow, TemporalDb,
};
use crate::types::SearchError;

// ============================================================================
// Helper utilities
// ============================================================================

fn temp_db() -> (TempDir, TemporalDb) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("temporal.db");
    let db = TemporalDb::open(&path).unwrap();
    (dir, db)
}

fn make_hotspot(n: usize) -> HotspotRow {
    HotspotRow {
        file_path: format!("src/file_{n}.rs"),
        score: n as f64 / 10_000.0,
        changes_30d: i64::try_from(n % 100).unwrap(),
        changes_90d: i64::try_from(n % 200).unwrap(),
    }
}

fn make_risk(n: usize) -> RiskRow {
    RiskRow {
        file_path: format!("src/file_{n}.rs"),
        risk_score: n as f64 / 10_000.0,
        total_commits: i64::try_from(n + 1).unwrap(),
        fix_commits: i64::try_from(n % 5).unwrap(),
        fix_density: (n % 5) as f64 / (n + 1) as f64,
    }
}

fn make_cochange(n: usize) -> CochangeRow {
    CochangeRow {
        file_a: format!("src/file_{n}.rs"),
        file_b: format!("src/file_{}.rs", n + 1),
        count: i64::try_from(n + 1).unwrap(),
        jaccard: n as f64 / 10_000.0,
    }
}

// ============================================================================
// Group 1: Schema & Lifecycle
// ============================================================================

#[test]
fn open_creates_all_tables() {
    let (_dir, db) = temp_db();
    // If the tables don't exist, load_* would return an error.
    assert!(db.load_hotspots().unwrap().is_empty());
    assert!(db.load_risks().unwrap().is_empty());
    assert!(db.load_cochanges().unwrap().is_empty());
}

#[test]
fn wal_mode_enabled() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("wal.db");
    let db = TemporalDb::open(&path).unwrap();
    // WAL mode produces a -wal sidecar file after the first write.
    db.set_meta("k", "v").unwrap();
    let wal_path = dir.path().join("wal.db-wal");
    assert!(
        wal_path.exists(),
        "WAL mode should create a -wal sidecar file"
    );
}

#[test]
fn schema_version_is_1() {
    let (_dir, db) = temp_db();
    assert_eq!(db.schema_version().unwrap(), 1);
}

#[test]
fn open_idempotent() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("idempotent.db");
    // Open twice — migrations should not fail or duplicate tables.
    let db1 = TemporalDb::open(&path).unwrap();
    db1.set_meta("x", "1").unwrap();
    drop(db1);
    let db2 = TemporalDb::open(&path).unwrap();
    assert_eq!(db2.get_meta("x").unwrap(), Some("1".to_string()));
}

#[cfg(unix)]
#[test]
fn permissions_unix() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("perms.db");
    let _db = TemporalDb::open(&path).unwrap();
    let meta = std::fs::metadata(&path).unwrap();
    // Mask to low 9 bits (rwxrwxrwx).
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "db file should be owner-only (0600)");
}

#[test]
fn rejects_future_schema_version() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("future.db");

    // Create a database with a higher user_version to simulate a future schema.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version = 999;").unwrap();
    }

    let result = TemporalDb::open(&path);
    assert!(
        result.is_err(),
        "opening a future-version database should fail"
    );
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("999"),
        "error should mention the unexpected version, got: {msg}"
    );
}

// ============================================================================
// Group 2: Hotspot CRUD
// ============================================================================

#[test]
fn store_and_load_hotspots_roundtrip() {
    let (_dir, db) = temp_db();
    let rows = vec![
        HotspotRow {
            file_path: "src/main.rs".to_string(),
            score: 0.9,
            changes_30d: 5,
            changes_90d: 12,
        },
        HotspotRow {
            file_path: "src/lib.rs".to_string(),
            score: 0.4,
            changes_30d: 2,
            changes_90d: 7,
        },
    ];
    db.store_hotspots(&rows).unwrap();
    let mut loaded = db.load_hotspots().unwrap();
    loaded.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    let mut expected = rows.clone();
    expected.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    assert_eq!(loaded, expected);
}

#[test]
fn store_hotspots_replaces_existing() {
    let (_dir, db) = temp_db();
    let first = vec![HotspotRow {
        file_path: "a.rs".to_string(),
        score: 0.5,
        changes_30d: 1,
        changes_90d: 2,
    }];
    db.store_hotspots(&first).unwrap();

    let second = vec![HotspotRow {
        file_path: "b.rs".to_string(),
        score: 0.8,
        changes_30d: 3,
        changes_90d: 6,
    }];
    db.store_hotspots(&second).unwrap();

    let loaded = db.load_hotspots().unwrap();
    assert_eq!(loaded.len(), 1, "second store should replace first");
    assert_eq!(loaded[0].file_path, "b.rs");
}

#[test]
fn load_hotspots_empty_db() {
    let (_dir, db) = temp_db();
    assert!(db.load_hotspots().unwrap().is_empty());
}

#[test]
fn store_hotspots_empty_slice() {
    let (_dir, db) = temp_db();
    // Pre-populate then wipe.
    let rows = vec![HotspotRow {
        file_path: "z.rs".to_string(),
        score: 1.0,
        changes_30d: 1,
        changes_90d: 1,
    }];
    db.store_hotspots(&rows).unwrap();
    db.store_hotspots(&[]).unwrap();
    assert!(db.load_hotspots().unwrap().is_empty());
}

#[test]
fn hotspot_float_precision() {
    let (_dir, db) = temp_db();
    let score = std::f64::consts::PI;
    let rows = vec![HotspotRow {
        file_path: "pi.rs".to_string(),
        score,
        changes_30d: 0,
        changes_90d: 0,
    }];
    db.store_hotspots(&rows).unwrap();
    let loaded = db.load_hotspots().unwrap();
    assert!(
        (loaded[0].score - score).abs() < f64::EPSILON,
        "float score should survive SQLite REAL roundtrip"
    );
}

// ============================================================================
// Group 3: Risk CRUD
// ============================================================================

#[test]
fn store_and_load_risks_roundtrip() {
    let (_dir, db) = temp_db();
    let rows = vec![RiskRow {
        file_path: "src/engine.rs".to_string(),
        risk_score: 0.7,
        total_commits: 20,
        fix_commits: 5,
        fix_density: 0.25,
    }];
    db.store_risks(&rows).unwrap();
    let loaded = db.load_risks().unwrap();
    assert_eq!(loaded, rows);
}

#[test]
fn store_risks_replaces_existing() {
    let (_dir, db) = temp_db();
    let first = vec![RiskRow {
        file_path: "old.rs".to_string(),
        risk_score: 0.1,
        total_commits: 1,
        fix_commits: 0,
        fix_density: 0.0,
    }];
    db.store_risks(&first).unwrap();

    let second = vec![RiskRow {
        file_path: "new.rs".to_string(),
        risk_score: 0.9,
        total_commits: 50,
        fix_commits: 10,
        fix_density: 0.2,
    }];
    db.store_risks(&second).unwrap();

    let loaded = db.load_risks().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].file_path, "new.rs");
}

#[test]
fn load_risks_empty_db() {
    let (_dir, db) = temp_db();
    assert!(db.load_risks().unwrap().is_empty());
}

// ============================================================================
// Group 4: Cochange CRUD
// ============================================================================

#[test]
fn store_and_load_cochanges_roundtrip() {
    let (_dir, db) = temp_db();
    let rows = vec![CochangeRow {
        file_a: "src/a.rs".to_string(),
        file_b: "src/b.rs".to_string(),
        count: 7,
        jaccard: 0.5,
    }];
    db.store_cochanges(&rows).unwrap();
    let loaded = db.load_cochanges().unwrap();
    assert_eq!(loaded, rows);
}

#[test]
fn store_cochanges_replaces_existing() {
    let (_dir, db) = temp_db();
    let first = vec![CochangeRow {
        file_a: "x.rs".to_string(),
        file_b: "y.rs".to_string(),
        count: 1,
        jaccard: 0.1,
    }];
    db.store_cochanges(&first).unwrap();

    let second = vec![CochangeRow {
        file_a: "p.rs".to_string(),
        file_b: "q.rs".to_string(),
        count: 3,
        jaccard: 0.9,
    }];
    db.store_cochanges(&second).unwrap();

    let loaded = db.load_cochanges().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].file_a, "p.rs");
}

#[test]
fn load_cochanges_empty_db() {
    let (_dir, db) = temp_db();
    assert!(db.load_cochanges().unwrap().is_empty());
}

// ============================================================================
// Group 5: Meta CRUD
// ============================================================================

#[test]
fn set_and_get_meta() {
    let (_dir, db) = temp_db();
    db.set_meta("version", "42").unwrap();
    assert_eq!(db.get_meta("version").unwrap(), Some("42".to_string()));
}

#[test]
fn get_meta_missing_key() {
    let (_dir, db) = temp_db();
    assert_eq!(db.get_meta("nonexistent").unwrap(), None);
}

#[test]
fn set_meta_overwrites() {
    let (_dir, db) = temp_db();
    db.set_meta("k", "first").unwrap();
    db.set_meta("k", "second").unwrap();
    assert_eq!(db.get_meta("k").unwrap(), Some("second".to_string()));
}

// ============================================================================
// Group 6: Persistence & Sync
// ============================================================================

#[test]
fn data_survives_close_reopen() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("persist.db");

    {
        let db = TemporalDb::open(&path).unwrap();
        let rows = vec![HotspotRow {
            file_path: "persist.rs".to_string(),
            score: 0.77,
            changes_30d: 3,
            changes_90d: 9,
        }];
        db.store_hotspots(&rows).unwrap();
    } // db dropped here — connection closed

    let db2 = TemporalDb::open(&path).unwrap();
    let loaded = db2.load_hotspots().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].file_path, "persist.rs");
    assert!((loaded[0].score - 0.77).abs() < f64::EPSILON);
}

#[test]
fn sync_writes_all_tables_atomically() {
    let (_dir, db) = temp_db();

    let hotspots = vec![HotspotRow {
        file_path: "h.rs".to_string(),
        score: 1.0,
        changes_30d: 1,
        changes_90d: 2,
    }];
    let risks = vec![RiskRow {
        file_path: "r.rs".to_string(),
        risk_score: 0.5,
        total_commits: 10,
        fix_commits: 2,
        fix_density: 0.2,
    }];
    let cochanges = vec![CochangeRow {
        file_a: "c_a.rs".to_string(),
        file_b: "c_b.rs".to_string(),
        count: 3,
        jaccard: 0.3,
    }];

    db.sync(&hotspots, &risks, &cochanges, "abc123").unwrap();

    assert_eq!(db.load_hotspots().unwrap().len(), 1);
    assert_eq!(db.load_risks().unwrap().len(), 1);
    assert_eq!(db.load_cochanges().unwrap().len(), 1);
}

#[test]
fn sync_sets_meta_keys() {
    let (_dir, db) = temp_db();
    db.sync(&[], &[], &[], "deadbeef").unwrap();

    let head = db.get_meta(META_GIT_HEAD).unwrap();
    let updated = db.get_meta(META_LAST_UPDATED).unwrap();

    assert_eq!(head, Some("deadbeef".to_string()));
    assert!(
        updated.is_some(),
        "META_LAST_UPDATED should be set after sync"
    );
    // Timestamp should be a plausible Unix epoch (> year 2020 = 1577836800).
    let ts: u64 = updated.unwrap().parse().unwrap();
    assert!(ts > 1_577_836_800, "timestamp should be after 2020, got {ts}");
}

// ============================================================================
// Group 7: Performance (10k-row acceptance criteria)
// ============================================================================

#[test]
fn load_10k_hotspots_under_100ms() {
    let (_dir, db) = temp_db();
    let rows: Vec<HotspotRow> = (0..10_000).map(make_hotspot).collect();
    db.store_hotspots(&rows).unwrap();

    let start = Instant::now();
    let loaded = db.load_hotspots().unwrap();
    let elapsed = start.elapsed();

    assert_eq!(loaded.len(), 10_000);
    assert!(
        elapsed.as_millis() < 100,
        "load_hotspots 10k rows took {}ms, expected <100ms",
        elapsed.as_millis()
    );
}

#[test]
fn load_10k_risks_under_100ms() {
    let (_dir, db) = temp_db();
    let rows: Vec<RiskRow> = (0..10_000).map(make_risk).collect();
    db.store_risks(&rows).unwrap();

    let start = Instant::now();
    let loaded = db.load_risks().unwrap();
    let elapsed = start.elapsed();

    assert_eq!(loaded.len(), 10_000);
    assert!(
        elapsed.as_millis() < 100,
        "load_risks 10k rows took {}ms, expected <100ms",
        elapsed.as_millis()
    );
}

#[test]
fn load_10k_cochanges_under_100ms() {
    let (_dir, db) = temp_db();
    let rows: Vec<CochangeRow> = (0..10_000).map(make_cochange).collect();
    db.store_cochanges(&rows).unwrap();

    let start = Instant::now();
    let loaded = db.load_cochanges().unwrap();
    let elapsed = start.elapsed();

    assert_eq!(loaded.len(), 10_000);
    assert!(
        elapsed.as_millis() < 100,
        "load_cochanges 10k rows took {}ms, expected <100ms",
        elapsed.as_millis()
    );
}

#[test]
fn store_10k_hotspots_under_200ms() {
    let (_dir, db) = temp_db();
    let rows: Vec<HotspotRow> = (0..10_000).map(make_hotspot).collect();

    let start = Instant::now();
    db.store_hotspots(&rows).unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_millis() < 200,
        "store_hotspots 10k rows took {}ms, expected <200ms",
        elapsed.as_millis()
    );
}

#[test]
fn sync_10k_each_under_500ms() {
    let (_dir, db) = temp_db();
    let hotspots: Vec<HotspotRow> = (0..10_000).map(make_hotspot).collect();
    let risks: Vec<RiskRow> = (0..10_000).map(make_risk).collect();
    let cochanges: Vec<CochangeRow> = (0..10_000).map(make_cochange).collect();

    let start = Instant::now();
    db.sync(&hotspots, &risks, &cochanges, "perf_head").unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_millis() < 500,
        "sync 10k×3 rows took {}ms, expected <500ms",
        elapsed.as_millis()
    );
}

// ============================================================================
// Group 8: Error Handling
// ============================================================================

#[test]
fn open_invalid_path() {
    let result = TemporalDb::open(std::path::Path::new(
        "/nonexistent/deeply/nested/path/temporal.db",
    ));
    assert!(
        result.is_err(),
        "opening an invalid path should return an error"
    );
}

#[test]
fn database_error_display() {
    let err = SearchError::Database("something went wrong".to_string());
    let display = err.to_string();
    assert_eq!(display, "Database error: something went wrong");
}

#[test]
fn database_error_variant_matchable() {
    let err = SearchError::Database("test".to_string());
    // Verify the variant is exhaustively matchable.
    let matched = match err {
        SearchError::Database(msg) => msg,
        _ => panic!("wrong variant"),
    };
    assert_eq!(matched, "test");
}
