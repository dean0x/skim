# Testing Review Report

**Branch**: feature/185-temporal-metadata-storage -> main
**Date**: 2026-05-26T13:09

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Missing test coverage for `CapacityExceeded` error path** - `storage_ops.rs:100-106`, `storage_ops.rs:123-129`, `storage_ops.rs:144-150`, `storage_ops.rs:310-321`
**Confidence**: 90%
- Problem: The `store_hotspots`, `store_risks`, `store_cochanges`, and `sync` methods all have capacity-guard branches that return `SearchError::CapacityExceeded` when the input exceeds `MAX_ROWS_PER_TABLE` (500,000). None of these four error branches have any test coverage. These are important safety rails documented in the PR description as "error handling" tests, yet the actual capacity-exceeded behavior is never exercised.
- Fix: Add at least one test that passes a slice exceeding the limit and asserts the correct error variant is returned. A single parameterized-style test covering `store_hotspots` plus a brief `sync` capacity test would be sufficient since the implementations are structurally identical:
  ```rust
  #[test]
  fn store_hotspots_rejects_over_capacity() {
      let (_dir, db) = temp_db();
      // Build a Vec with exactly MAX_ROWS_PER_TABLE + 1 entries.
      // Using a stub with empty content avoids real memory pressure.
      let rows: Vec<HotspotRow> = (0..=500_000)
          .map(|n| HotspotRow {
              file_path: format!("{n}"),
              score: 0.0,
              changes_30d: 0,
              changes_90d: 0,
          })
          .collect();
      let err = db.store_hotspots(&rows).unwrap_err();
      assert!(
          matches!(err, SearchError::CapacityExceeded(_)),
          "expected CapacityExceeded, got {err:?}"
      );
  }
  ```

**Missing test for `sync` atomicity on second call (idempotent replace)** - `storage_perf_tests.rs:85-113`
**Confidence**: 82%
- Problem: The `sync_writes_all_tables_atomically` test only calls `sync` once and verifies the data is present. It does not verify the atomic *replacement* behavior -- that a second `sync` call replaces the previous data rather than appending to it. The individual `store_*_replaces_existing` tests cover this for each table in isolation, but `sync` is a distinct code path (single transaction, multiple tables), and its replace-on-second-call behavior is untested.
- Fix: Add assertions after a second `sync` call:
  ```rust
  #[test]
  fn sync_replaces_on_second_call() {
      let (_dir, db) = temp_db();
      // First sync with 1 row per table.
      db.sync(&[hotspot_a], &[risk_a], &[cochange_a], "sha1").unwrap();
      // Second sync with different data.
      db.sync(&[hotspot_b], &[risk_b], &[cochange_b], "sha2").unwrap();
      assert_eq!(db.load_hotspots().unwrap().len(), 1);
      assert_eq!(db.load_hotspots().unwrap()[0].file_path, "b.rs");
      assert_eq!(db.get_meta(META_GIT_HEAD).unwrap(), Some("sha2".to_string()));
  }
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Duplicated `temp_db()` helper across two test files** - `storage_tests.rs:17-22`, `storage_perf_tests.rs:22-27`
**Confidence**: 85%
- Problem: The identical `temp_db()` function is defined in both `storage_tests.rs` and `storage_perf_tests.rs`. While these are separate `#[cfg(test)]` modules and cannot share code via normal Rust imports (they are parallel sibling modules under `storage`), the duplication means a change to the setup pattern (e.g., adding a config option to `TemporalDb::open`) would need to be applied in two places.
- Fix: This is an inherent limitation of the `#[path = ...]` test module pattern. The duplication is minimal (5 lines) and acceptable given the module structure. Acknowledge with a brief comment in one file referencing the other, or extract a shared test harness module if more helpers accumulate in the future.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Performance tests are timing-based, not property-based** - `storage_perf_tests.rs:137-240` (Confidence: 65%) -- The debug-aware thresholds are a good fix from the prior review cycle, but timing-based assertions remain inherently environment-sensitive. Consider whether Criterion benchmarks (already used elsewhere in the project) could supplement these with regression detection rather than absolute thresholds.

- **No test for `sync` rollback on mid-transaction failure** - `storage_ops.rs:304-350` (Confidence: 70%) -- The `sync` method's atomicity guarantee (either all tables commit or none do) is only tested on the success path. Injecting a failure mid-transaction (e.g., via a corrupt cochange row that violates a constraint) and verifying the DB remains unchanged would strengthen the atomicity contract test. This is hard to trigger cleanly without test hooks, so the complexity may not be justified for now.

- **`temporal_stats_dedup_within_commit` builds `CommitInfo` manually instead of reusing `make_commit`** - `scoring_tests.rs:737-764` (Confidence: 62%) -- The test necessarily bypasses `make_commit` to inject duplicate file paths, so this is intentional. A small builder or `make_commit_with_raw_files` helper could reduce the boilerplate, but the explicitness has value for clarity.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The test suite is well-structured and thorough. 47 new tests cover the core behaviors: schema lifecycle, CRUD roundtrips, meta operations, persistence across close/reopen, sync atomicity, performance at 10k scale, boundary conditions for temporal windows, and error handling. Tests follow clear AAA structure, use temp directories for isolation, and names describe expected behavior. The debug-aware performance thresholds (fixed in cycle 1) are a sensible approach.

The two blocking MEDIUM items are coverage gaps for documented error paths (capacity guards) and sync replacement behavior. Both are straightforward to add and would close the remaining untested branches in the storage layer.
