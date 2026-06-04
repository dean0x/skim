# Rust Review Report

**Branch**: feature/185-temporal-metadata-storage -> main
**Date**: 2026-05-26T13:09

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Migration `execute_batch` is not atomic** - `storage.rs:97-128`
**Confidence**: 82%
- Problem: `run_migrations` uses `execute_batch` to run four `CREATE TABLE` statements followed by `PRAGMA user_version = 1`. `execute_batch` is not transactional -- if an intermediate statement fails (e.g., disk full after the third CREATE TABLE), the schema will be partially created but `user_version` will remain 0. On next open, the `version < 1` guard re-enters the block, but since `CREATE TABLE IF NOT EXISTS` is used, the tables that already exist are silently skipped. This means the migration is **idempotent in practice**, which mitigates the risk significantly. However, if a future migration (version 2) adds `ALTER TABLE` statements, partial application without a transaction could leave the database in an unrecoverable state.
- Fix: Wrap the migration block in an explicit transaction:
  ```rust
  if version < 1 {
      conn.execute_batch(
          "BEGIN;
           CREATE TABLE IF NOT EXISTS hotspot ( ... );
           CREATE TABLE IF NOT EXISTS risk ( ... );
           CREATE TABLE IF NOT EXISTS cochange ( ... );
           CREATE TABLE IF NOT EXISTS meta ( ... );
           PRAGMA user_version = 1;
           COMMIT;",
      )
      .map_err(db_err)?;
  }
  ```

**SAFETY comments on `unchecked_transaction` are slightly inaccurate** - `storage_ops.rs:107-109`
**Confidence**: 85%
- Problem: The SAFETY comments state "TemporalDb is not Send/Sync". In fact, `rusqlite::Connection` is `Send` (but not `Sync`), which means `TemporalDb` IS `Send` -- it can be moved to another thread. The safety argument is that only one thread can call methods at a time, which follows from not being `Sync`, not from not being `Send`. The distinction matters for correctness of the invariant documentation.
- Fix: Update the four SAFETY comments (lines 107-109, 130, 151, 323-325) from:
  ```rust
  // SAFETY: `TemporalDb` is not `Send`/`Sync` and holds a single
  // connection. Callers cannot share it across threads, so no nested
  // transaction can be active when this method is called.
  ```
  to:
  ```rust
  // SAFETY: `TemporalDb` is `Send` but not `Sync` — it can be moved
  // to another thread but cannot be shared. Since `&self` methods cannot
  // be called concurrently, no nested transaction can be active.
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`load_*` methods have no row-count guard against corrupted databases** - `storage_ops.rs:183-258`
**Confidence**: 80%
- Problem: The `store_*` methods enforce `MAX_ROWS_PER_TABLE = 500_000`, but the `load_*` methods perform unbounded `SELECT` with `.collect::<Vec<_>>()`. If the database file was externally modified (manual INSERT, corruption, or a future code path that bypasses the store cap), a load could allocate an arbitrarily large vector. This is inconsistent with the crate's reliability posture where every capacity is bounded.
- Fix: Add a `LIMIT` clause or a post-load count check. A lightweight approach:
  ```rust
  pub fn load_hotspots(&self) -> Result<Vec<HotspotRow>> {
      let mut stmt = self
          .conn
          .prepare("SELECT file_path, score, changes_30d, changes_90d FROM hotspot LIMIT 500001")
          .map_err(db_err)?;
      let rows = stmt
          .query_map([], |row| { /* ... */ })
          .map_err(db_err)?
          .collect::<std::result::Result<Vec<_>, _>>()
          .map_err(db_err)?;
      if rows.len() > MAX_ROWS_PER_TABLE {
          return Err(SearchError::CapacityExceeded(
              format!("load_hotspots: {} rows exceeds limit of {MAX_ROWS_PER_TABLE}", rows.len())
          ));
      }
      Ok(rows)
  }
  ```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`seen_in_commit` could use `HashSet<&str>` to avoid per-commit String allocations** - `scoring.rs:225` (Confidence: 65%) -- The dedup set currently owns `String` values. If `path_str()` returns a `Cow::Borrowed` (common case for valid UTF-8 paths), the `into_owned()` call allocates unnecessarily for the dedup set. A `HashSet<&str>` borrowing from the commit's `changed_files` would avoid these allocations entirely, though it requires careful lifetime management.

- **`prepare_cached` in `load_*` methods could improve repeated-load performance** - `storage_ops.rs:184-186` (Confidence: 62%) -- The `load_*` methods use `prepare` while the `insert_*_in_tx` helpers and `sync` use `prepare_cached`. If loads are called in a loop (e.g., polling for freshness), cached statement preparation would save repeated SQLite compilation.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code demonstrates strong Rust idioms overall: proper `Result` propagation via `?` and `map_err`, no `.unwrap()` in library code, borrow-first allocation patterns (cited in feature knowledge), `saturating_add` for u32 counters, `#[non_exhaustive]` on `SearchError`, clean boundary between rusqlite types and the public API via `db_err`, and good use of `thiserror` for typed error variants. The `TemporalDb` struct correctly avoids `Sync` (inherits from `Connection`), and WAL mode with busy timeout is well-suited for the concurrent-readers use case.

The two blocking MEDIUM items are: (1) wrapping the migration in a transaction to prevent partial schema application in future version bumps, and (2) correcting the SAFETY documentation on `unchecked_transaction` to accurately reflect the `Send`-but-not-`Sync` nature of the type.
