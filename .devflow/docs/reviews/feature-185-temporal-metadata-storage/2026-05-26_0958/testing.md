# Testing Review Report

**Branch**: feature-185-temporal-metadata-storage -> main
**Date**: 2026-05-26T09:58

## Issues in Your Changes (BLOCKING)

### HIGH

**Performance tests use wall-clock timing assertions that may flake in CI** - `storage_perf_tests.rs:140-225`
**Confidence**: 85%
- Problem: Five performance tests (`load_10k_hotspots_under_100ms`, `load_10k_risks_under_100ms`, `load_10k_cochanges_under_100ms`, `store_10k_hotspots_under_200ms`, `sync_10k_each_under_500ms`) use `Instant::now()` with hard-coded millisecond ceilings (100ms, 200ms, 500ms). On CI runners under load, cold-start allocation, or debug-mode builds, these can exceed the thresholds, producing flaky failures.
- Fix: Gate timing assertions behind a cargo feature or `#[cfg]` flag, or use `#[ignore]` with a comment explaining they require `--release`. Alternatively, increase thresholds by 3-5x and add a `eprintln!` with the actual timing for observability without hard-failing:
```rust
#[test]
#[cfg_attr(not(feature = "perf-tests"), ignore)]
fn load_10k_hotspots_under_100ms() { ... }
```

### MEDIUM

**`sync_writes_all_tables_atomically` does not actually test atomicity** - `storage_perf_tests.rs:87-115`
**Confidence**: 82%
- Problem: The test name promises atomicity verification, but the test only writes data via `sync()` and then reads it back. It does not simulate a failure mid-transaction (e.g., injecting a constraint violation in one table after another succeeds) to verify that all-or-nothing semantics hold. This is a "happy path only" test for a property that only matters under failure.
- Fix: Add a test that stores initial data, then calls `sync` with data that will fail (e.g., a constraint violation or by corrupting the connection mid-transaction), and verifies that the original data is still intact:
```rust
#[test]
fn sync_rolls_back_on_partial_failure() {
    let (_dir, db) = temp_db();
    // Pre-populate with known data
    let initial = vec![HotspotRow { file_path: "initial.rs".into(), score: 0.5, changes_30d: 1, changes_90d: 2 }];
    db.store_hotspots(&initial).unwrap();
    
    // Attempt sync that should fail — e.g., drop a table to trigger an error mid-sync
    db.conn.execute("DROP TABLE risk", []).unwrap();
    let result = db.sync(&[], &[], &[], "bad");
    assert!(result.is_err(), "sync should fail when risk table is missing");
    
    // Verify hotspot data is unchanged (rollback worked)
    let loaded = db.load_hotspots().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].file_path, "initial.rs");
}
```

**Missing test for `store_cochanges` and `store_risks` with empty slice** - `storage_tests.rs`
**Confidence**: 83%
- Problem: `store_hotspots_empty_slice` (line 168) verifies that storing an empty slice clears the table, but no equivalent test exists for `store_risks` or `store_cochanges`. The three store methods share the same DELETE+INSERT pattern, but testing only one variant leaves the other two with less coverage of the "wipe" behavior.
- Fix: Add `store_risks_empty_slice` and `store_cochanges_empty_slice` tests following the same pattern as `store_hotspots_empty_slice`.

**Missing test for `sync` idempotency (calling sync twice)** - `storage_perf_tests.rs`
**Confidence**: 80%
- Problem: The `sync` method is tested once in `sync_writes_all_tables_atomically` and once for meta keys, but there is no test verifying that calling `sync` twice (with different data) correctly replaces all tables. Since `sync` uses DELETE+INSERT, a second call should completely replace the first call's data, but this is not verified.
- Fix: Add a test that calls `sync` with data set A, then calls `sync` with data set B, and verifies only B's data is present.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`HotspotRow` derives `PartialEq` with `f64` fields -- equality comparison is fragile** - `storage_types.rs:11`
**Confidence**: 80%
- Problem: `HotspotRow`, `RiskRow`, and `CochangeRow` all derive `PartialEq` and contain `f64` fields (`score`, `risk_score`, `fix_density`, `jaccard`). The storage tests use `assert_eq!` on these types (e.g., `storage_tests.rs:134`, `storage_tests.rs:216`, `storage_tests.rs:266`). While SQLite REAL roundtrips are lossless for IEEE 754 doubles, the `hotspot_float_precision` test (line 183) wisely uses epsilon comparison -- but all other CRUD roundtrip tests rely on `PartialEq` which does exact `f64` comparison. This works today but is brittle if serialization ever changes.
- Fix: This is acceptable for now given SQLite's exact IEEE 754 storage, but consider adding a comment to the `PartialEq` derive explaining why exact float equality is safe for these types (SQLite stores as 8-byte IEEE 754).

## Pre-existing Issues (Not Blocking)

_None identified at CRITICAL severity._

## Suggestions (Lower Confidence)

- **Missing concurrent-reader test for WAL mode** - `storage_tests.rs` (Confidence: 65%) -- The `wal_mode_enabled` test verifies the sidecar file exists, but does not test that two `TemporalDb` instances can read from the same file simultaneously, which is the primary benefit of WAL mode.

- **No test for `compute_file_temporal_stats` with large input (performance regression guard)** - `scoring_tests.rs` (Confidence: 70%) -- The storage layer has 10k-row performance tests, but `compute_file_temporal_stats` has no equivalent. A 10k-commit input test would guard against accidental O(n^2) regressions in the deduplication logic.

- **`temp_db` helper is duplicated across two test modules** - `storage_tests.rs:19` and `storage_perf_tests.rs:24` (Confidence: 72%) -- The identical helper could be extracted to a shared test utility to reduce duplication, though the cost is low given it is only 4 lines.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 3 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Assessment

The test suite is well-structured with clear grouping (schema lifecycle, CRUD roundtrips, persistence, performance, error handling, and `compute_file_temporal_stats` boundary conditions). Tests follow AAA structure, use `tempfile::TempDir` for proper isolation, and exercise boundary conditions (30-day/90-day window edges, negative timestamps, future commits, duplicate file deduplication). The coverage is thorough for the new functionality.

**Strengths**:
- Comprehensive boundary testing for `compute_file_temporal_stats` (30d/90d edges, future, negative)
- Good deduplication test (`temporal_stats_dedup_within_commit`)
- Schema lifecycle tests (idempotent open, future version rejection, Unix permissions)
- Performance acceptance criteria codified as tests
- Clean test names that describe expected behavior

**Conditions for merge**:
1. Address the flaky-timing risk in performance tests (HIGH) -- at minimum add `#[ignore]` or document that these require `--release`
2. Consider adding the atomicity-failure test for `sync` (MEDIUM) -- the test name promises atomicity but only tests the happy path
