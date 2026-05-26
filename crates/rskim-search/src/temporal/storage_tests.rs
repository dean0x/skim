//! Tests for [`TemporalDb`] — schema, CRUD, and meta operations.
//!
//! Uses `tempfile::TempDir` for isolation so every test gets a fresh database.
//! Performance and persistence tests live in `storage_perf_tests.rs`.

use tempfile::TempDir;

use super::{
    TemporalDb,
    storage_types::{CochangeRow, HotspotRow, RiskRow},
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
// Group 6: Capacity rejection
// ============================================================================

#[test]
fn store_hotspots_rejects_over_capacity() {
    let (_dir, db) = temp_db();
    let rows: Vec<HotspotRow> = (0..500_001)
        .map(|n| HotspotRow {
            file_path: format!("{n}"),
            score: 0.0,
            changes_30d: 0,
            changes_90d: 0,
        })
        .collect();
    let err = db.store_hotspots(&rows).unwrap_err();
    assert!(matches!(err, SearchError::CapacityExceeded(_)));
}

#[test]
fn sync_rejects_over_capacity() {
    let (_dir, db) = temp_db();
    let big: Vec<HotspotRow> = (0..500_001)
        .map(|n| HotspotRow {
            file_path: format!("{n}"),
            score: 0.0,
            changes_30d: 0,
            changes_90d: 0,
        })
        .collect();
    let err = db.sync(&big, &[], &[], "head").unwrap_err();
    assert!(matches!(err, SearchError::CapacityExceeded(_)));
}
