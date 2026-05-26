//! Persistence, sync, performance, and error-handling tests for [`TemporalDb`].
//!
//! Uses `tempfile::TempDir` for isolation so every test gets a fresh database.
//! Schema and CRUD tests live in `storage_tests.rs`.
//!
//! Performance tests enforce the 10k-row acceptance criteria from the plan.

use std::time::Instant;

use tempfile::TempDir;

use super::{
    storage_types::{CochangeRow, HotspotRow, RiskRow},
    META_GIT_HEAD, META_LAST_UPDATED, TemporalDb,
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
        changes_30d: (n % 100) as u32,
        changes_90d: (n % 200) as u32,
    }
}

fn make_risk(n: usize) -> RiskRow {
    RiskRow {
        file_path: format!("src/file_{n}.rs"),
        risk_score: n as f64 / 10_000.0,
        total_commits: (n + 1) as u32,
        fix_commits: (n % 5) as u32,
        fix_density: (n % 5) as f64 / (n + 1) as f64,
    }
}

fn make_cochange(n: usize) -> CochangeRow {
    CochangeRow {
        file_a: format!("src/file_{n}.rs"),
        file_b: format!("src/file_{}.rs", n + 1),
        count: (n + 1) as u32,
        jaccard: n as f64 / 10_000.0,
    }
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
fn sync_replaces_on_second_call() {
    let (_dir, db) = temp_db();
    let h1 = vec![HotspotRow {
        file_path: "a.rs".into(),
        score: 0.5,
        changes_30d: 1,
        changes_90d: 2,
    }];
    let r1 = vec![RiskRow {
        file_path: "a.rs".into(),
        risk_score: 0.3,
        total_commits: 5,
        fix_commits: 1,
        fix_density: 0.2,
    }];
    let c1 = vec![CochangeRow {
        file_a: "a.rs".into(),
        file_b: "b.rs".into(),
        count: 2,
        jaccard: 0.4,
    }];
    db.sync(&h1, &r1, &c1, "sha1").unwrap();

    let h2 = vec![HotspotRow {
        file_path: "x.rs".into(),
        score: 0.9,
        changes_30d: 3,
        changes_90d: 6,
    }];
    let r2 = vec![RiskRow {
        file_path: "x.rs".into(),
        risk_score: 0.8,
        total_commits: 20,
        fix_commits: 4,
        fix_density: 0.2,
    }];
    let c2 = vec![CochangeRow {
        file_a: "x.rs".into(),
        file_b: "y.rs".into(),
        count: 5,
        jaccard: 0.7,
    }];
    db.sync(&h2, &r2, &c2, "sha2").unwrap();

    let loaded_h = db.load_hotspots().unwrap();
    assert_eq!(loaded_h.len(), 1);
    assert_eq!(loaded_h[0].file_path, "x.rs");

    let loaded_r = db.load_risks().unwrap();
    assert_eq!(loaded_r.len(), 1);
    assert_eq!(loaded_r[0].file_path, "x.rs");

    let loaded_c = db.load_cochanges().unwrap();
    assert_eq!(loaded_c.len(), 1);
    assert_eq!(loaded_c[0].file_a, "x.rs");

    assert_eq!(
        db.get_meta(META_GIT_HEAD).unwrap(),
        Some("sha2".to_string())
    );
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

    // Debug builds run without optimisations and CI runners may be under load,
    // so give 5× headroom in debug mode while keeping the tight release ceiling.
    let threshold_ms: u128 = if cfg!(debug_assertions) { 500 } else { 100 };

    let start = Instant::now();
    let loaded = db.load_hotspots().unwrap();
    let elapsed = start.elapsed();

    assert_eq!(loaded.len(), 10_000);
    assert!(
        elapsed.as_millis() < threshold_ms,
        "load_hotspots 10k rows took {}ms, expected <{}ms",
        elapsed.as_millis(),
        threshold_ms,
    );
}

#[test]
fn load_10k_risks_under_100ms() {
    let (_dir, db) = temp_db();
    let rows: Vec<RiskRow> = (0..10_000).map(make_risk).collect();
    db.store_risks(&rows).unwrap();

    let threshold_ms: u128 = if cfg!(debug_assertions) { 500 } else { 100 };

    let start = Instant::now();
    let loaded = db.load_risks().unwrap();
    let elapsed = start.elapsed();

    assert_eq!(loaded.len(), 10_000);
    assert!(
        elapsed.as_millis() < threshold_ms,
        "load_risks 10k rows took {}ms, expected <{}ms",
        elapsed.as_millis(),
        threshold_ms,
    );
}

#[test]
fn load_10k_cochanges_under_100ms() {
    let (_dir, db) = temp_db();
    let rows: Vec<CochangeRow> = (0..10_000).map(make_cochange).collect();
    db.store_cochanges(&rows).unwrap();

    let threshold_ms: u128 = if cfg!(debug_assertions) { 500 } else { 100 };

    let start = Instant::now();
    let loaded = db.load_cochanges().unwrap();
    let elapsed = start.elapsed();

    assert_eq!(loaded.len(), 10_000);
    assert!(
        elapsed.as_millis() < threshold_ms,
        "load_cochanges 10k rows took {}ms, expected <{}ms",
        elapsed.as_millis(),
        threshold_ms,
    );
}

#[test]
fn store_10k_hotspots_under_200ms() {
    let (_dir, db) = temp_db();
    let rows: Vec<HotspotRow> = (0..10_000).map(make_hotspot).collect();

    let threshold_ms: u128 = if cfg!(debug_assertions) { 1_000 } else { 200 };

    let start = Instant::now();
    db.store_hotspots(&rows).unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_millis() < threshold_ms,
        "store_hotspots 10k rows took {}ms, expected <{}ms",
        elapsed.as_millis(),
        threshold_ms,
    );
}

#[test]
fn sync_10k_each_under_500ms() {
    let (_dir, db) = temp_db();
    let hotspots: Vec<HotspotRow> = (0..10_000).map(make_hotspot).collect();
    let risks: Vec<RiskRow> = (0..10_000).map(make_risk).collect();
    let cochanges: Vec<CochangeRow> = (0..10_000).map(make_cochange).collect();

    let threshold_ms: u128 = if cfg!(debug_assertions) { 2_500 } else { 500 };

    let start = Instant::now();
    db.sync(&hotspots, &risks, &cochanges, "perf_head").unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_millis() < threshold_ms,
        "sync 10k×3 rows took {}ms, expected <{}ms",
        elapsed.as_millis(),
        threshold_ms,
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

