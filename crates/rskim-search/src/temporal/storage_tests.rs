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
fn schema_version_is_2() {
    let (_dir, db) = temp_db();
    assert_eq!(db.schema_version().unwrap(), 2);
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

// ============================================================================
// Group 7: Per-file lookup methods (Step 1)
// ============================================================================

#[test]
fn hotspot_for_file_returns_none_when_empty() {
    let (_dir, db) = temp_db();
    assert_eq!(db.hotspot_for_file("src/main.rs").unwrap(), None);
}

#[test]
fn hotspot_for_file_returns_row_when_present() {
    let (_dir, db) = temp_db();
    let row = HotspotRow {
        file_path: "src/auth.rs".to_string(),
        score: 0.85,
        changes_30d: 10,
        changes_90d: 25,
    };
    db.store_hotspots(&[row.clone()]).unwrap();
    let found = db.hotspot_for_file("src/auth.rs").unwrap().unwrap();
    assert_eq!(found, row);
}

#[test]
fn hotspot_for_file_returns_none_for_unknown_path() {
    let (_dir, db) = temp_db();
    db.store_hotspots(&[HotspotRow {
        file_path: "src/auth.rs".to_string(),
        score: 0.5,
        changes_30d: 1,
        changes_90d: 2,
    }])
    .unwrap();
    assert_eq!(db.hotspot_for_file("src/other.rs").unwrap(), None);
}

#[test]
fn risk_for_file_returns_none_when_empty() {
    let (_dir, db) = temp_db();
    assert_eq!(db.risk_for_file("src/main.rs").unwrap(), None);
}

#[test]
fn risk_for_file_returns_row_when_present() {
    let (_dir, db) = temp_db();
    let row = RiskRow {
        file_path: "src/engine.rs".to_string(),
        risk_score: 0.72,
        total_commits: 30,
        fix_commits: 8,
        fix_density: 0.267,
    };
    db.store_risks(&[row.clone()]).unwrap();
    let found = db.risk_for_file("src/engine.rs").unwrap().unwrap();
    assert_eq!(found, row);
}

#[test]
fn cochanges_for_file_returns_empty_when_no_pairs() {
    let (_dir, db) = temp_db();
    assert!(db.cochanges_for_file("src/auth.rs").unwrap().is_empty());
}

#[test]
fn cochanges_for_file_returns_both_directions() {
    let (_dir, db) = temp_db();
    // Canonical ordering: file_a < file_b lexically.
    // "src/a.rs" < "src/b.rs" so file_a = "src/a.rs", file_b = "src/b.rs".
    let row = CochangeRow {
        file_a: "src/a.rs".to_string(),
        file_b: "src/b.rs".to_string(),
        count: 5,
        jaccard: 0.6,
    };
    db.store_cochanges(&[row.clone()]).unwrap();

    // Query for file_a should find it
    let from_a = db.cochanges_for_file("src/a.rs").unwrap();
    assert_eq!(from_a.len(), 1);
    assert_eq!(from_a[0], row);

    // Query for file_b should also find it
    let from_b = db.cochanges_for_file("src/b.rs").unwrap();
    assert_eq!(from_b.len(), 1);
    assert_eq!(from_b[0], row);
}

#[test]
fn cochanges_for_file_returns_multiple_sorted_by_jaccard() {
    let (_dir, db) = temp_db();
    let rows = vec![
        CochangeRow {
            file_a: "src/a.rs".to_string(),
            file_b: "src/c.rs".to_string(),
            count: 3,
            jaccard: 0.3,
        },
        CochangeRow {
            file_a: "src/a.rs".to_string(),
            file_b: "src/b.rs".to_string(),
            count: 7,
            jaccard: 0.8,
        },
    ];
    db.store_cochanges(&rows).unwrap();
    let results = db.cochanges_for_file("src/a.rs").unwrap();
    assert_eq!(results.len(), 2);
    // Should be sorted descending by jaccard
    assert!(
        results[0].jaccard >= results[1].jaccard,
        "results should be sorted by jaccard desc"
    );
    assert!((results[0].jaccard - 0.8).abs() < f64::EPSILON);
}

#[test]
fn cochanges_for_file_respects_canonical_ordering() {
    let (_dir, db) = temp_db();
    // Only "src/auth.rs" is involved; "src/middleware.rs" > "src/auth.rs" lexically
    // so auth.rs is stored as file_a.
    let row = CochangeRow {
        file_a: "src/auth.rs".to_string(),
        file_b: "src/middleware.rs".to_string(),
        count: 12,
        jaccard: 0.72,
    };
    db.store_cochanges(&[row.clone()]).unwrap();
    // Both directions should find the same row
    let via_a = db.cochanges_for_file("src/auth.rs").unwrap();
    let via_b = db.cochanges_for_file("src/middleware.rs").unwrap();
    assert_eq!(via_a, vec![row.clone()]);
    assert_eq!(via_b, vec![row]);
}

/// Regression: cochanges_for_file must return results ordered by jaccard DESC.
/// With many rows the LIMIT 10000 keeps memory bounded while preserving the
/// highest-jaccard partners at the front of the result.
#[test]
fn cochanges_for_file_returns_highest_jaccard_first_with_many_rows() {
    let (_dir, db) = temp_db();

    // Insert 20 co-change rows for "src/hub.rs" with varying jaccard values.
    // The pair with jaccard=1.00 should always appear first regardless of
    // insertion order.
    let rows: Vec<CochangeRow> = (0..20_u32)
        .map(|i| CochangeRow {
            // "src/hub.rs" < "src/partner_NN.rs" lexically so file_a = hub.
            file_a: "src/hub.rs".to_string(),
            file_b: format!("src/partner_{i:02}.rs"),
            count: i + 1,
            jaccard: (i as f64 + 1.0) / 20.0, // 0.05 .. 1.00
        })
        .collect();
    db.store_cochanges(&rows).unwrap();

    let results = db.cochanges_for_file("src/hub.rs").unwrap();
    assert_eq!(results.len(), 20, "all 20 rows should be returned");
    // Verify the result is sorted descending by jaccard.
    for window in results.windows(2) {
        assert!(
            window[0].jaccard >= window[1].jaccard,
            "results must be sorted by jaccard DESC: {} < {}",
            window[0].jaccard,
            window[1].jaccard
        );
    }
    // The first result should be the highest-jaccard pair.
    assert!(
        (results[0].jaccard - 1.0).abs() < f64::EPSILON,
        "highest jaccard partner must be first, got {}",
        results[0].jaccard
    );
}

// ============================================================================
// Group 8: Top-N query methods (Step 2)
// ============================================================================

#[test]
fn top_hotspots_sorted_descending() {
    let (_dir, db) = temp_db();
    db.store_hotspots(&[
        HotspotRow {
            file_path: "b.rs".to_string(),
            score: 0.5,
            changes_30d: 2,
            changes_90d: 5,
        },
        HotspotRow {
            file_path: "a.rs".to_string(),
            score: 0.9,
            changes_30d: 8,
            changes_90d: 20,
        },
        HotspotRow {
            file_path: "c.rs".to_string(),
            score: 0.3,
            changes_30d: 1,
            changes_90d: 3,
        },
    ])
    .unwrap();
    let results = db.top_hotspots(10).unwrap();
    assert_eq!(results.len(), 3);
    // Scores should be in descending order
    for i in 0..results.len() - 1 {
        assert!(results[i].score >= results[i + 1].score);
    }
    assert!((results[0].score - 0.9).abs() < f64::EPSILON);
}

#[test]
fn top_hotspots_respects_limit() {
    let (_dir, db) = temp_db();
    db.store_hotspots(&[
        HotspotRow {
            file_path: "a.rs".to_string(),
            score: 0.9,
            changes_30d: 1,
            changes_90d: 2,
        },
        HotspotRow {
            file_path: "b.rs".to_string(),
            score: 0.7,
            changes_30d: 1,
            changes_90d: 2,
        },
        HotspotRow {
            file_path: "c.rs".to_string(),
            score: 0.5,
            changes_30d: 1,
            changes_90d: 2,
        },
    ])
    .unwrap();
    let results = db.top_hotspots(2).unwrap();
    assert_eq!(results.len(), 2, "limit should cap at 2 rows");
    assert!((results[0].score - 0.9).abs() < f64::EPSILON);
}

#[test]
fn top_hotspots_empty_table_returns_empty() {
    let (_dir, db) = temp_db();
    assert!(db.top_hotspots(10).unwrap().is_empty());
}

#[test]
fn top_risks_sorted_descending() {
    let (_dir, db) = temp_db();
    db.store_risks(&[
        RiskRow {
            file_path: "a.rs".to_string(),
            risk_score: 0.3,
            total_commits: 10,
            fix_commits: 1,
            fix_density: 0.1,
        },
        RiskRow {
            file_path: "b.rs".to_string(),
            risk_score: 0.8,
            total_commits: 20,
            fix_commits: 6,
            fix_density: 0.3,
        },
    ])
    .unwrap();
    let results = db.top_risks(10).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0].risk_score >= results[1].risk_score);
    assert!((results[0].risk_score - 0.8).abs() < f64::EPSILON);
}

#[test]
fn top_coldspots_sorted_ascending() {
    let (_dir, db) = temp_db();
    db.store_hotspots(&[
        HotspotRow {
            file_path: "hot.rs".to_string(),
            score: 0.95,
            changes_30d: 20,
            changes_90d: 50,
        },
        HotspotRow {
            file_path: "cold.rs".to_string(),
            score: 0.05,
            changes_30d: 0,
            changes_90d: 1,
        },
        HotspotRow {
            file_path: "medium.rs".to_string(),
            score: 0.5,
            changes_30d: 5,
            changes_90d: 12,
        },
    ])
    .unwrap();
    let results = db.top_coldspots(10).unwrap();
    assert_eq!(results.len(), 3);
    // Ascending: coldest first
    for i in 0..results.len() - 1 {
        assert!(results[i].score <= results[i + 1].score);
    }
    assert!((results[0].score - 0.05).abs() < f64::EPSILON);
}

#[test]
fn top_risks_empty_returns_empty() {
    let (_dir, db) = temp_db();
    assert!(db.top_risks(10).unwrap().is_empty());
}

// ============================================================================
// Group 9: Regression — UNION ALL indexed cochange query (storage_ops:156)
// ============================================================================

/// Regression: cochanges_for_file must find rows where the queried file
/// appears in file_b (not just file_a). The UNION ALL rewrite enables SQLite
/// to hit the secondary index on file_b instead of scanning with OR.
#[test]
fn cochanges_for_file_union_all_hits_file_b_index() {
    let (_dir, db) = temp_db();
    // "src/z.rs" > "src/a.rs" lexically, so canonical storage is
    // file_a = "src/a.rs", file_b = "src/z.rs".
    // Querying by the file_b value ("src/z.rs") must still return the row.
    let row = CochangeRow {
        file_a: "src/a.rs".to_string(),
        file_b: "src/z.rs".to_string(),
        count: 9,
        jaccard: 0.77,
    };
    db.store_cochanges(&[row.clone()]).unwrap();

    let results = db.cochanges_for_file("src/z.rs").unwrap();
    assert_eq!(results.len(), 1, "file_b lookup must return the row");
    assert_eq!(results[0], row);
}

/// Regression: a file that appears in both arms of the UNION ALL (once as
/// file_a and once as file_b across different rows) must not produce duplicate
/// results because the canonical ordering guarantee means no single row can
/// satisfy both `file_a = ?1` AND `file_b = ?1` simultaneously.
#[test]
fn cochanges_for_file_no_duplicates_across_union_arms() {
    let (_dir, db) = temp_db();
    // "src/hub.rs" appears as file_a in one row and file_b in another.
    // "src/aaa.rs" < "src/hub.rs" < "src/zzz.rs" lexically.
    let rows = vec![
        CochangeRow {
            file_a: "src/aaa.rs".to_string(),
            file_b: "src/hub.rs".to_string(),
            count: 3,
            jaccard: 0.4,
        },
        CochangeRow {
            file_a: "src/hub.rs".to_string(),
            file_b: "src/zzz.rs".to_string(),
            count: 7,
            jaccard: 0.8,
        },
    ];
    db.store_cochanges(&rows).unwrap();

    let results = db.cochanges_for_file("src/hub.rs").unwrap();
    assert_eq!(
        results.len(),
        2,
        "hub.rs appears in both arms; must return exactly 2 distinct rows"
    );
    // Results should still be sorted by jaccard DESC.
    assert!(
        results[0].jaccard >= results[1].jaccard,
        "results must be sorted by jaccard DESC"
    );
}

// ============================================================================
// Group 10: Regression — top-N limit overflow (storage_ops:187)
// ============================================================================

/// Regression: top_hotspots must not produce an i64 overflow when limit is
/// usize::MAX. The clamp to MAX_ROWS_PER_TABLE must happen before the cast.
#[test]
fn top_hotspots_usize_max_does_not_overflow() {
    let (_dir, db) = temp_db();
    db.store_hotspots(&[HotspotRow {
        file_path: "x.rs".to_string(),
        score: 0.5,
        changes_30d: 1,
        changes_90d: 2,
    }])
    .unwrap();
    // usize::MAX would wrap to a large negative i64 without the clamp.
    let results = db.top_hotspots(usize::MAX).unwrap();
    assert_eq!(results.len(), 1, "usize::MAX limit must be clamped safely");
}

/// Regression: top_risks must not produce an i64 overflow when limit is
/// usize::MAX.
#[test]
fn top_risks_usize_max_does_not_overflow() {
    let (_dir, db) = temp_db();
    db.store_risks(&[RiskRow {
        file_path: "x.rs".to_string(),
        risk_score: 0.5,
        total_commits: 10,
        fix_commits: 1,
        fix_density: 0.1,
    }])
    .unwrap();
    let results = db.top_risks(usize::MAX).unwrap();
    assert_eq!(results.len(), 1, "usize::MAX limit must be clamped safely");
}

/// Regression: top_coldspots must not produce an i64 overflow when limit is
/// usize::MAX.
#[test]
fn top_coldspots_usize_max_does_not_overflow() {
    let (_dir, db) = temp_db();
    db.store_hotspots(&[HotspotRow {
        file_path: "cold.rs".to_string(),
        score: 0.1,
        changes_30d: 0,
        changes_90d: 1,
    }])
    .unwrap();
    let results = db.top_coldspots(usize::MAX).unwrap();
    assert_eq!(results.len(), 1, "usize::MAX limit must be clamped safely");
}

// ============================================================================
// Group 11: Schema v2 migration (Step 3)
// ============================================================================

#[test]
fn v1_database_migrates_to_v2_on_reopen() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("migrate.db");

    // Create a v1 database manually.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "BEGIN;
            CREATE TABLE IF NOT EXISTS hotspot (
                file_path TEXT PRIMARY KEY, score REAL NOT NULL,
                changes_30d INTEGER NOT NULL, changes_90d INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS risk (
                file_path TEXT PRIMARY KEY, risk_score REAL NOT NULL,
                total_commits INTEGER NOT NULL, fix_commits INTEGER NOT NULL,
                fix_density REAL NOT NULL
            );
            CREATE TABLE IF NOT EXISTS cochange (
                file_a TEXT NOT NULL, file_b TEXT NOT NULL,
                count INTEGER NOT NULL, jaccard REAL NOT NULL,
                PRIMARY KEY (file_a, file_b)
            );
            CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            PRAGMA user_version = 1;
            COMMIT;",
        )
        .unwrap();
    }

    // Reopen via TemporalDb — should migrate to v2.
    let db = TemporalDb::open(&path).unwrap();
    assert_eq!(
        db.schema_version().unwrap(),
        2,
        "v1 database should be migrated to v2 on reopen"
    );
}
