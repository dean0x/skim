# Rust Review Report

**Branch**: feature-185-temporal-metadata-storage -> main
**Date**: 2026-05-26T09:58

## Issues in Your Changes (BLOCKING)

### HIGH

**Documentation says ISO-8601 but code stores Unix epoch seconds** - `storage.rs:52`
**Confidence**: 95%
- Problem: The doc comment for `META_LAST_UPDATED` says "Key storing the ISO-8601 UTC timestamp" but `storage_ops.rs:308-312` computes `SystemTime::now().duration_since(UNIX_EPOCH).as_secs().to_string()`, which is a Unix epoch integer, not ISO-8601. Consumers reading the doc will parse the value as ISO-8601 (e.g., `2026-05-25T12:00:00Z`) and get a parse failure or incorrect data.
- Fix: Either change the doc comment to match reality, or change the code to format as ISO-8601. Matching the doc to the code is simpler:

```rust
/// Key storing the Unix epoch timestamp (seconds) of the last successful [`TemporalDb::sync`].
pub const META_LAST_UPDATED: &str = "last_updated";
```

### MEDIUM

**Code duplication between `sync` and individual `store_*` methods** - `storage_ops.rs:250-325` vs `storage_ops.rs:29-97`
**Confidence**: 82%
- Problem: The `sync` method duplicates the DELETE + INSERT logic from `store_hotspots`, `store_risks`, and `store_cochanges` line-for-line. This means any schema change (e.g., adding a column to `hotspot`) must be updated in two places. The sync method is 76 lines of nearly identical SQL that could delegate to internal helpers.
- Fix: Extract the per-table INSERT logic into private helpers that accept a `&Transaction` instead of creating their own, then call those helpers from both the individual `store_*` methods and `sync`. Example:

```rust
fn insert_hotspots_in_tx(tx: &rusqlite::Transaction<'_>, rows: &[HotspotRow]) -> Result<()> {
    tx.execute("DELETE FROM hotspot", []).map_err(db_err)?;
    let mut stmt = tx.prepare_cached(
        "INSERT INTO hotspot (file_path, score, changes_30d, changes_90d)
         VALUES (?1, ?2, ?3, ?4)",
    ).map_err(db_err)?;
    for row in rows {
        stmt.execute(params![row.file_path, row.score, row.changes_30d, row.changes_90d])
            .map_err(db_err)?;
    }
    drop(stmt);
    Ok(())
}
```

**Unnecessary String allocation in deduplication loop** - `scoring.rs:244-246`
**Confidence**: 80%
- Problem: `file.path_str().into_owned()` allocates a new `String` for every file in every commit. When the same file appears across thousands of commits, this produces redundant allocations. The `seen_in_commit` `HashSet` is cleared each iteration so allocations are not reused across commits.
- Fix: This is an accepted pattern per the feature knowledge (the `HashSet<String>` reuse is documented). However, if `path_str()` returns a `Cow::Borrowed`, the allocation can be avoided by using `Cow<str>` as the set type or by inserting only when not already present:

```rust
for file in &commit.changed_files {
    let path = file.path_str();
    if !seen_in_commit.contains(path.as_ref()) {
        seen_in_commit.insert(path.into_owned());
    }
}
```

This avoids the `into_owned()` allocation for duplicates within a single commit.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`set_permissions` error silently discarded** - `storage.rs:180` (Confidence: 65%) -- `let _ = std::fs::set_permissions(db_path, perms);` discards the error. On read-only filesystems or Docker volumes the database file may be world-readable. Consider logging a debug warning. (Also noted in feature knowledge gotchas.)

- **`FileTemporalStats` uses `u32` while `HotspotRow`/`RiskRow` use `i64`** - `types.rs:284` vs `storage_types.rs:18` (Confidence: 70%) -- The type mismatch means callers must cast `u32 -> i64` when writing and guard `i64 -> u32` when reading. A conversion helper or consistent type choice would prevent truncation bugs at the boundary. (Feature knowledge gotcha #10 documents this but provides no safeguard.)

- **Performance tests use wall-clock assertions** - `storage_perf_tests.rs:150-154` (Confidence: 65%) -- Assertions like `elapsed.as_millis() < 100` are non-deterministic and may flake on slow CI runners or under load. Consider using a higher threshold in CI or gating with `#[ignore]` and running separately.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The implementation is solid overall: proper error handling with `?` and `map_err(db_err)` throughout (no `unwrap()` in non-test code), `#[must_use]` on Result-returning load methods, clean separation between storage_types / storage_ops / storage.rs, correct use of `unchecked_transaction()` for `&self` methods, forward-compat schema guard, WAL mode, and `pub(super)` visibility to prevent rusqlite leakage. The main blocking issue is the doc/code mismatch on `META_LAST_UPDATED` format (ISO-8601 vs Unix epoch), which will mislead consumers. The duplication between `sync` and individual `store_*` methods is a maintenance risk worth addressing.
