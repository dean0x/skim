# Security Review Report

**Branch**: feature/185-temporal-metadata-storage -> main
**Date**: 2026-05-26T13:09

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`unchecked_transaction` safety relies on convention, not enforcement** - `storage_ops.rs:110,131,152,326` (Confidence: 65%) — The SAFETY comments assert that `TemporalDb` is not `Send`/`Sync` so no nested transactions can be active. This is currently true because `rusqlite::Connection` is `!Sync`, but `TemporalDb` does not carry an explicit negative trait bound or marker. If a future refactor wraps `conn` in an `Arc<Mutex<Connection>>` (to enable sharing), the `unchecked_transaction` calls would become unsound without any compile-time diagnostic. The current implementation is safe today — this is a future-proofing observation.

- **`PRAGMA synchronous=NORMAL` trades durability for speed** - `storage.rs:192` (Confidence: 60%) — WAL mode with `synchronous=NORMAL` means a power loss during a commit can lose the most recent transaction (SQLite docs confirm this). For a cache that can be rebuilt from git history this is an acceptable trade-off, but it is worth noting explicitly in documentation that the database is not crash-proof at the individual-transaction level. The PR description positions this as a cache, so this is informational.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

## Detailed Security Analysis

The following areas were examined and found to be well-implemented:

### SQL Injection Prevention
All SQL statements use parameterized queries (`?1`, `?2`, etc.) via rusqlite's `params!` macro. No string interpolation is used for query construction. The only dynamic SQL is the `PRAGMA user_version` read, which takes no user input. The `execute_batch` call in `run_migrations` uses a compile-time constant string literal. This is correct and complete.

### File Permission Hardening
`storage.rs:175-187` sets database file permissions to `0o600` (owner read/write only) on Unix targets immediately after creation. The permission change is in a race-free position (immediately after `Connection::open` creates the file). On non-Unix targets the permission restriction is silently skipped, which is the correct approach for cross-platform code.

### Capacity Bounds / Denial of Service
Every store and sync method enforces a `MAX_ROWS_PER_TABLE = 500_000` limit before beginning any INSERT loop. This prevents unbounded CPU and memory consumption from unexpectedly large datasets. The check runs before the transaction begins, avoiding wasted work.

### Error Handling / Information Leakage
All `rusqlite` errors are converted to opaque `SearchError::Database(String)` via the `db_err` helper. No rusqlite types leak into the public API. The error messages include the rusqlite description (which is useful for debugging) but do not expose internal file paths, connection strings, or stack traces.

### Transaction Safety
Transactions use DELETE + batch INSERT (replace-all pattern). The `sync` method wraps all four table operations in a single transaction, ensuring atomicity. On error, the transaction is rolled back automatically by rusqlite's `Drop` implementation.

### Forward Compatibility Guard
`run_migrations` rejects databases with `user_version > CURRENT_VERSION`, preventing silent data corruption if an older binary opens a database created by a newer version.

### Schema Design
Tables use `TEXT PRIMARY KEY` for file paths, which prevents duplicate entries. The `meta` table uses `INSERT OR REPLACE` for upsert semantics. No user-facing input reaches the database — all data originates from git history parsing within the same process.

### Dependency Assessment
`rusqlite 0.31` with `bundled` feature compiles SQLite from source, avoiding dependency on system SQLite version. This is the recommended approach for reproducible builds and eliminates risks from outdated system SQLite installations.

### Timestamp Handling
`compute_file_temporal_stats` in `scoring.rs:231` clamps negative timestamps to 0 before casting to `u64`, preventing underflow. Future-dated commits are handled by treating elapsed time as 0. The `sync` method uses `SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO)` to safely handle the (theoretically impossible) case where system time is before epoch.
