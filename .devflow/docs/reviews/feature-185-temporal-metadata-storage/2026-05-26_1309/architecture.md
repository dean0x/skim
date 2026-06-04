# Architecture Review Report

**Branch**: feature/185-temporal-metadata-storage -> main
**Date**: 2026-05-26T13:09

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**`TemporalDb` safety comments inaccurately claim "not Send/Sync"** - `storage_ops.rs:107`, `storage_ops.rs:130`, `storage_ops.rs:323`
**Confidence**: 82%
- Problem: The safety comments justifying `unchecked_transaction()` state "TemporalDb is not Send/Sync and holds a single connection. Callers cannot share it across threads..." However, `rusqlite::Connection` is `Send` (but not `Sync`), so `TemporalDb` IS `Send`. The type can be moved to another thread. The actual safety guarantee that prevents nested transactions is that `TemporalDb` is not `Sync` (cannot be shared across threads concurrently), so only one thread can call methods at a time. The "not Send" part of the comment is incorrect.
- Fix: Update the three safety comments to say `TemporalDb` is `Send` but not `Sync`:
  ```rust
  // SAFETY: `TemporalDb` wraps `rusqlite::Connection` which is `Send`
  // but not `Sync`. A single connection can only be accessed from one
  // thread at a time, so no nested transaction can be active here.
  ```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Type mismatch between `FileTemporalStats` (u32) and row types (i64)** - `storage_types.rs:18-20` vs `types.rs:284-290` (Confidence: 72%) -- `HotspotRow.changes_30d` and `RiskRow.total_commits` are `i64` (matching SQLite INTEGER), while `FileTemporalStats` uses `u32`. The caller must bridge these two representations with a cast. This is documented in the feature knowledge ("cast carefully"), but a shared representation or a `From` impl would make the boundary safer. The current design is defensible (SQLite native types in row structs, Rust native types in domain structs), so this is a suggestion, not a finding.

- **`compute_file_temporal_stats` does not deduplicate files per commit the same way as `compute_file_risk_scores`** - `scoring.rs:225-252` vs `scoring.rs:137-153` (Confidence: 65%) -- `compute_file_temporal_stats` uses a `HashSet<String>` deduplication buffer per commit (allocating owned strings), while `compute_file_risk_scores` does no per-commit dedup (relying on additive float accumulation being idempotent enough). The approaches are different but each is correct for its use case: `compute_file_temporal_stats` counts discrete commits so duplicates would inflate counts, while `compute_file_risk_scores` accumulates weighted floats where a duplicate just double-counts weight (acceptable for a heuristic). This is a design choice, not a bug; noting it for awareness.

- **`db_err` visibility is `pub(super)` rather than module-private** - `storage.rs:66` (Confidence: 60%) -- `db_err` is marked `pub(super)` so `storage_ops.rs` (included via `#[path]` as a submodule) can use it. This is correct given the `#[path]` module structure. An alternative would be `pub(in crate::temporal)` for more precise scoping, but the current visibility is functionally equivalent since `storage` is the only direct child of `temporal` that uses it.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Rationale

This PR introduces a well-structured SQLite persistence layer for temporal metadata. The architecture has several notable strengths:

**Separation of Concerns (SRP)**: The storage module is cleanly split into three files -- `storage.rs` (connection, migrations, struct definition), `storage_types.rs` (row types), and `storage_ops.rs` (CRUD operations). Each has a single responsibility. This mirrors the existing three-module pattern in the `cochange/` module (format, builder, reader) and is consistent with the codebase.

**Dependency Direction**: Dependencies point inward correctly. The storage layer depends on `crate::types::SearchError` (domain types), never the reverse. Row types (`HotspotRow`, `RiskRow`, `CochangeRow`) are plain structs with no rusqlite dependencies, keeping the public API clean.

**Error Boundary**: All rusqlite errors are converted to `SearchError::Database(String)` at the storage boundary via the `db_err` helper. No rusqlite types leak into the public API. This follows the same pattern as `SearchError::Git(String)` for gix errors, maintaining API consistency.

**Atomic Operations**: The `sync()` method wraps all four table writes in a single transaction, ensuring readers never see partial state. Individual `store_*` methods also use transactions. The DELETE + INSERT pattern for idempotent replacement is appropriate for a cache-like persistence layer.

**Capacity Guards**: All store and sync methods enforce `MAX_ROWS_PER_TABLE = 500_000`, preventing unbounded INSERT loops. This matches the reliability principle of bounded operations.

**Schema Versioning**: Forward-compatible migration guard (`version > CURRENT_VERSION` rejects newer schemas) prevents silent data corruption. Migrations are idempotent via `CREATE TABLE IF NOT EXISTS`.

**Dual Persistence Model**: The documented architecture of `.skcc` binary (mmap point queries) + SQLite (bulk ranking access) is well-motivated. Each format serves a distinct access pattern, and the knowledge base documents the relationship clearly.

The single blocking finding (inaccurate safety comments) is a documentation accuracy issue, not a correctness bug -- the `unchecked_transaction` usage is safe because `TemporalDb` is not `Sync`, which is the property that actually matters.
