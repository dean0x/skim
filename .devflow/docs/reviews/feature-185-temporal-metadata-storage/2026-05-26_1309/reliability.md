# Reliability Review Report

**Branch**: feature/185-temporal-metadata-storage -> main
**Date**: 2026-05-26T13:09

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Inaccurate SAFETY comment: TemporalDb IS Send (3 occurrences)** — Confidence: 92%
- `crates/rskim-search/src/temporal/storage_ops.rs:107`, `crates/rskim-search/src/temporal/storage_ops.rs:130`, `crates/rskim-search/src/temporal/storage_ops.rs:323`
- Problem: The SAFETY comments on `unchecked_transaction()` calls state "TemporalDb is not Send/Sync" but `rusqlite::Connection` implements `Send` (confirmed in rusqlite 0.31.0 source: `unsafe impl Send for Connection {}`). `TemporalDb` therefore auto-derives `Send`. The safety argument for `unchecked_transaction()` relies on the type not being _concurrently accessible_ from multiple threads, which is guaranteed by it not being `Sync` — not by it not being `Send`. `Send` means it can be transferred to another thread (which it can). If someone wraps `TemporalDb` in an `Arc<Mutex<>>` in the future, the current SAFETY comments would provide a false sense of security about the invariant being preserved.
- Fix: Change the SAFETY comments from "not `Send`/`Sync`" to "not `Sync`" and clarify that the guarantee is no _concurrent_ access to the same connection:
  ```rust
  // SAFETY: `TemporalDb` is `Send` but not `Sync` — it can be moved
  // to another thread but cannot be shared. No concurrent access to
  // the underlying connection can occur, so no nested transaction
  // can be active when this method is called.
  ```

**WAL mode activation not verified** — Confidence: 82%
- `crates/rskim-search/src/temporal/storage.rs:192`
- Problem: `PRAGMA journal_mode=WAL` is executed via `execute_batch`, which ignores the result set returned by the PRAGMA. If WAL mode fails to activate (e.g., on a read-only filesystem, NFS, or if the database is opened in immutable mode), SQLite silently falls back to DELETE journal mode. The code then proceeds as if WAL is active, but concurrent readers could block writers and the 5-second busy timeout may not behave as expected under DELETE mode. The documentation and the feature knowledge both state WAL mode as a design contract.
- Fix: Use `query_row` to verify the journal mode was actually set:
  ```rust
  let journal_mode: String = conn
      .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
      .map_err(db_err)?;
  if journal_mode.to_lowercase() != "wal" {
      return Err(SearchError::Database(format!(
          "failed to enable WAL mode; journal_mode is '{journal_mode}'"
      )));
  }
  conn.execute_batch("PRAGMA synchronous=NORMAL;")
      .map_err(db_err)?;
  ```

## Issues in Code You Touched (Should Fix)

_No issues found._

## Pre-existing Issues (Not Blocking)

_No issues found._

## Suggestions (Lower Confidence)

- **Load methods have no row-count bound** - `crates/rskim-search/src/temporal/storage_ops.rs:183` (Confidence: 62%) — `load_hotspots`, `load_risks`, `load_cochanges` collect all rows into a `Vec` without a `LIMIT` clause. Store methods guard at 500k rows, so an internally-written database is bounded. However, if an externally-modified database contains millions of rows, loads would allocate unbounded memory. Given the database is local and created by this process, this is low-risk but violates the bounded-resource principle.

- **Migration DDL and version pragma not wrapped in explicit transaction** - `crates/rskim-search/src/temporal/storage.rs:97` (Confidence: 65%) — `execute_batch` runs each statement in autocommit mode. If the process crashes after creating some tables but before `PRAGMA user_version = 1`, the next open will re-run the migration safely due to `CREATE TABLE IF NOT EXISTS`. The current behavior is idempotent and correct, but wrapping in `BEGIN/COMMIT` would provide stronger crash-recovery guarantees as an atomic unit.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The storage layer demonstrates strong reliability fundamentals: bounded iteration (capacity guards on all store paths), explicit error handling via Result types throughout, pre-sized allocations in `compute_file_temporal_stats`, atomic multi-table sync via transactions, forward-compat schema guard, 5-second busy timeout, and saturating arithmetic on u32 counters (confirmed fixed in prior cycle). The two blocking items are correctness of safety documentation and verification of a critical runtime assumption (WAL mode). Neither represents a data-loss or crash risk today, but both should be addressed to maintain the reliability contract as the codebase evolves.
