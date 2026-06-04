# Security Review Report

**Branch**: feature-185-temporal-metadata-storage -> main
**Date**: 2026-05-26T09:58

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`unchecked_transaction` bypasses Rust borrow-checker safety for nested transactions** - `storage_ops.rs:30,54,84,257`
**Confidence**: 82%
- Problem: `unchecked_transaction()` is used in all four transaction sites (`store_hotspots`, `store_risks`, `store_cochanges`, `sync`). Unlike `transaction()`, `unchecked_transaction()` does not take `&mut self` -- it takes `&self`, which means the Rust borrow checker cannot prevent a caller from starting a second transaction while one is already active. If a future caller (or a concurrent code path in async context) begins a second `unchecked_transaction` on the same connection while one is inflight, the inner call silently promotes to a `SAVEPOINT` rather than a true transaction, and error-handling/rollback semantics change in unexpected ways. Since `TemporalDb` takes `&self` (not `&mut self`) for all store/sync methods, there is no compile-time guard against concurrent or nested calls on the same instance.
- Fix: Change `store_hotspots`, `store_risks`, `store_cochanges`, and `sync` signatures to take `&mut self` instead of `&self`, and use `self.conn.transaction()` instead of `self.conn.unchecked_transaction()`. This leverages the borrow checker to guarantee exclusive access during mutations. If `&self` is architecturally required (e.g., shared behind `Arc`), add a comment documenting the invariant and why nested transactions cannot occur.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Silent permission failure on `set_permissions` leaves database world-readable** - `storage.rs:180`
**Confidence**: 85%
- Problem: The `let _ = std::fs::set_permissions(db_path, perms);` pattern silently discards the error. On filesystems where `set_permissions` fails (NFS, FUSE, some Docker volume mounts), the database file retains whatever default permissions the OS assigned -- potentially group- or world-readable. The KNOWLEDGE.md already documents this as a gotcha, but from a defense-in-depth perspective, a warning should be emitted at minimum.
- Fix: Log or propagate the error. At minimum, use `eprintln!` or a structured logging mechanism:
  ```rust
  if let Err(e) = std::fs::set_permissions(db_path, perms) {
      eprintln!("[skim-search] warning: could not set database permissions to 0o600: {e}");
  }
  ```
  Alternatively, if the project adopts `SKIM_DEBUG` for this crate, gate behind that flag.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`META_LAST_UPDATED` stores epoch seconds as a string, not ISO-8601 as documented** - `storage_ops.rs:308-312` (Confidence: 70%) -- The doc comment on `META_LAST_UPDATED` in `storage.rs:52` says "ISO-8601 UTC timestamp", but `sync()` stores `SystemTime::now().duration_since(UNIX_EPOCH)...as_secs().to_string()` which is a raw epoch integer string (e.g., `"1748217600"`), not ISO-8601 (e.g., `"2026-05-25T12:00:00Z"`). This is a documentation-vs-implementation mismatch that could mislead future readers or consumers who expect ISO-8601 format.

- **No input validation on `file_path` values before database insertion** - `storage_ops.rs:39,63,92` (Confidence: 65%) -- File paths from git history are stored directly into SQLite TEXT columns with no length or character validation. While SQL injection is not possible (parameterized queries are used correctly), extremely long paths or paths containing control characters could cause display/parsing issues downstream. This is low risk given the data source (git history), but a length cap on `file_path` would be a defense-in-depth measure.

- **`unchecked_transaction` in `sync()` does not set `IMMEDIATE` isolation** - `storage_ops.rs:257` (Confidence: 62%) -- The default transaction mode in SQLite is `DEFERRED`, meaning the transaction does not acquire a write lock until the first write statement. In WAL mode with concurrent readers, this is usually fine. However, `sync()` always performs writes (DELETE + INSERT). Using `TransactionBehavior::Immediate` would acquire the write lock upfront, providing a clearer failure mode if another writer holds the lock (immediate `SQLITE_BUSY` rather than a mid-transaction lock escalation).

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Positive Security Observations

1. **Parameterized queries throughout** -- All SQL statements use `params![]` macro with positional placeholders. No string interpolation in SQL. Injection risk is effectively zero.
2. **Schema version forward-compat guard** -- `run_migrations` rejects databases with a schema version higher than `CURRENT_VERSION`, preventing silent corruption when a newer writer creates tables this version does not understand.
3. **Error boundary enforcement** -- All `rusqlite::Error` values are converted to `SearchError::Database(String)` via `db_err()`, preventing rusqlite types from leaking into the public API. This is clean and consistent.
4. **WAL mode with busy timeout** -- 5-second busy timeout handles transient lock contention gracefully. WAL mode allows concurrent readers without blocking.
5. **File permissions set to `0o600`** -- Owner-only read/write on Unix, with a test (`permissions_unix`) verifying the invariant.
6. **Atomic sync via single transaction** -- `sync()` wraps all four table mutations in one transaction, so partial state is never visible to readers.
7. **No hardcoded secrets or credentials** -- Database path is caller-supplied; no embedded tokens or API keys.
8. **Bundled SQLite (`features = ["bundled"]`)** -- Avoids system SQLite version skew and ensures a known-good SQLite version with security patches.

### Conditions for Approval

1. Address the `unchecked_transaction` concern: either switch to `&mut self` + `transaction()`, or add explicit documentation explaining why `unchecked_transaction` is safe in this context (i.e., that `TemporalDb` is never shared across threads and callers never nest calls).
