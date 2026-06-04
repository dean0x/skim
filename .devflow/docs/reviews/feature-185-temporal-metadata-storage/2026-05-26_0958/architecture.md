# Architecture Review Report

**Branch**: feature-185-temporal-metadata-storage -> main
**Date**: 2026-05-26T09:58
**PR**: #253

## Issues in Your Changes (BLOCKING)

### HIGH

**Code duplication between individual `store_*` methods and `sync`** - `storage_ops.rs:29-98` and `storage_ops.rs:250-325`
**Confidence**: 85%
- Problem: The `sync` method reimplements the exact same DELETE + batch INSERT logic found in `store_hotspots`, `store_risks`, and `store_cochanges`. Each table's INSERT SQL and parameter binding appears twice — once in the individual method and once inline in `sync`. This violates DRY and creates a maintenance risk: if the schema evolves (e.g., adding a column to `hotspot`), the developer must update the SQL in two places or the individual method and `sync` will silently diverge.
- Fix: Extract private helper functions for each table operation that accept a transaction reference, then have both the individual `store_*` methods and `sync` delegate to them:
  ```rust
  fn insert_hotspots_in_tx(tx: &rusqlite::Transaction, rows: &[HotspotRow]) -> Result<()> {
      tx.execute("DELETE FROM hotspot", []).map_err(db_err)?;
      let mut stmt = tx.prepare_cached(
          "INSERT INTO hotspot (file_path, score, changes_30d, changes_90d)
           VALUES (?1, ?2, ?3, ?4)"
      ).map_err(db_err)?;
      for row in rows {
          stmt.execute(params![row.file_path, row.score, row.changes_30d, row.changes_90d])
              .map_err(db_err)?;
      }
      drop(stmt);
      Ok(())
  }
  ```
  Then `sync` calls `insert_hotspots_in_tx(&tx, hotspots)?;` etc., and `store_hotspots` opens a transaction and delegates to the same helper.

### MEDIUM

**`META_LAST_UPDATED` doc comment says ISO-8601 but stores Unix epoch seconds** - `storage.rs:52-53` and `storage_ops.rs:308-312`
**Confidence**: 90%
- Problem: The doc comment on `META_LAST_UPDATED` reads "Key storing the ISO-8601 UTC timestamp", but `sync` stores `SystemTime::now().duration_since(UNIX_EPOCH).as_secs().to_string()` — a plain Unix epoch integer as a string, not ISO-8601 format. The test `sync_sets_meta_keys` also parses the value as `u64`, confirming the actual format is epoch seconds. This documentation-code mismatch will confuse consumers who try to parse the value as ISO-8601.
- Fix: Either change the doc comment to match reality:
  ```rust
  /// Key storing the Unix epoch timestamp (seconds) of the last successful [`TemporalDb::sync`].
  pub const META_LAST_UPDATED: &str = "last_updated";
  ```
  Or change `sync` to actually store ISO-8601:
  ```rust
  let now_iso = chrono::Utc::now().to_rfc3339();
  ```
  The former is simpler and avoids adding a `chrono` dependency.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`db_err` helper visibility is `pub(super)` but only used within the `storage` submodule tree** - `storage.rs:65`
**Confidence**: 80%
- Problem: `db_err` is `pub(super)` which exposes it to the entire `temporal` module. Since `storage_ops` is a child module of `storage` (via `#[path]`), `pub(crate)` or even `pub(in crate::temporal::storage)` would restrict the blast radius more precisely. Currently any code in `temporal/mod.rs` or `temporal/scoring.rs` could call `db_err` if it imported it, which would violate the "no rusqlite outside storage" boundary documented in the feature knowledge anti-patterns.
- Fix: The current `pub(super)` is acceptable given the module structure (`storage_ops` is `mod storage_ops` inside `storage.rs`, making `super` = `storage`). Verify that the `#[path]` includes make `super` resolve to `storage`, not `temporal`. If so, the visibility is correct. Add a brief inline comment clarifying the resolution for future readers.

## Pre-existing Issues (Not Blocking)

(none found at CRITICAL severity)

## Suggestions (Lower Confidence)

- **Consider a trait for the storage boundary** - `storage_ops.rs` (Confidence: 65%) -- A `TemporalStore` trait abstracting `store_*`/`load_*`/`sync` would enable mock-based testing of code that depends on `TemporalDb` without needing a real SQLite file. The current design couples consumers directly to the concrete `TemporalDb` struct. The feature knowledge notes that this is a persistence layer for ranking pipelines, and those pipelines may benefit from dependency injection.

- **`unchecked_transaction` bypasses thread-safety checks** - `storage_ops.rs:30,54,84,257` (Confidence: 70%) -- `unchecked_transaction()` is used instead of `transaction()` throughout. The `unchecked` variant skips the runtime borrow check that rusqlite uses to prevent aliased transactions. This is safe here because the `&self` receiver means only one `sync`/`store` call can be active per `TemporalDb` at a time (Rust's borrow rules). However, if `TemporalDb` ever gains interior mutability or async methods, `unchecked_transaction` could silently allow nested transactions.

- **`compute_file_temporal_stats` allocates `String` per file per commit in the dedup set** - `scoring.rs:245` (Confidence: 65%) -- `file.path_str().into_owned()` clones the path for each file in each commit's dedup `HashSet`. For large histories (100K+ commits) this creates significant short-lived allocation pressure. A `HashSet<&str>` borrowing from `commit.changed_files` would eliminate these allocations but requires lifetime management.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The architecture is clean and well-layered. The three-file split (`storage.rs` / `storage_types.rs` / `storage_ops.rs`) mirrors the established pattern from the `cochange` module (format/builder/reader). The `SearchError::Database` variant correctly converts all rusqlite errors to strings at the boundary so no SQLite types leak into the public API. The atomic `sync` method wrapping all four tables in a single transaction is the right pattern for consistent multi-table updates. The forward-compat migration guard and WAL-mode configuration are production-grade.

Conditions for approval:
1. Fix the doc comment / implementation mismatch on `META_LAST_UPDATED` (stores epoch seconds, not ISO-8601).
2. Consider extracting shared INSERT logic to eliminate the duplicated SQL between individual `store_*` methods and `sync` -- this is the most architecturally significant issue, as it creates a maintenance burden that will compound as the schema evolves.
