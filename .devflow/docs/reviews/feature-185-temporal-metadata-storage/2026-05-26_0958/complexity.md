# Complexity Review Report

**Branch**: feature-185-temporal-metadata-storage -> main
**Date**: 2026-05-26T09:58

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`sync()` duplicates all three store method bodies inline (69 lines)** - `storage_ops.rs:250-325`
**Confidence**: 90%
- Problem: The `sync()` method at 76 lines (250-325) duplicates the exact same DELETE + prepare_cached + INSERT loop pattern already implemented individually in `store_hotspots()`, `store_risks()`, and `store_cochanges()`. This is three near-identical blocks of ~15 lines each copy-pasted into a single function, making it long and repetitive. If the INSERT statement for any table changes (e.g., a column is added), the developer must update it in two places. The function also exceeds the 50-line warning threshold for function length.
- Fix: Refactor `sync()` to call the individual store methods within the transaction, or extract a private helper that takes a table name, SQL, and a closure for parameter binding. The simplest approach that preserves the single-transaction atomicity:
  ```rust
  pub fn sync(
      &self,
      hotspots: &[HotspotRow],
      risks: &[RiskRow],
      cochanges: &[CochangeRow],
      git_head: &str,
  ) -> Result<()> {
      let tx = self.conn.unchecked_transaction().map_err(db_err)?;

      // Delegate to private helpers that accept a &Transaction
      Self::insert_hotspots_tx(&tx, hotspots)?;
      Self::insert_risks_tx(&tx, risks)?;
      Self::insert_cochanges_tx(&tx, cochanges)?;

      // ---- meta ----
      let now_secs = SystemTime::now()
          .duration_since(UNIX_EPOCH)
          .unwrap_or(Duration::ZERO)
          .as_secs()
          .to_string();
      let mut meta_stmt = tx
          .prepare_cached("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)")
          .map_err(db_err)?;
      meta_stmt.execute(params![META_GIT_HEAD, git_head]).map_err(db_err)?;
      meta_stmt.execute(params![META_LAST_UPDATED, now_secs]).map_err(db_err)?;
      drop(meta_stmt);

      tx.commit().map_err(db_err)
  }
  ```
  Then `store_hotspots()` would also call `Self::insert_hotspots_tx()` inside its own transaction. This eliminates the duplication and keeps `sync()` well under 30 lines.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`compute_file_temporal_stats()` allocates `String` for every file in every commit** - `scoring.rs:245-246`
**Confidence**: 82%
- Problem: Unlike the sibling `compute_file_risk_scores()` which uses a `Cow<str>` borrow-first pattern to avoid allocating for already-seen paths, `compute_file_temporal_stats()` calls `file.path_str().into_owned()` unconditionally for every file in every commit, then inserts into a `HashSet<String>`. This creates O(total_file_touches) allocations instead of the O(unique_files) pattern used 100 lines above in the same file. The inconsistency between two sibling functions in the same module creates a maintenance hazard and a performance gap.
- Fix: Either use a `HashSet<Cow<'_, str>>` to avoid the unconditional clone, or since the dedup set is cleared per-commit and borrowing from `commit.changed_files` is viable:
  ```rust
  seen_in_commit.clear();
  for file in &commit.changed_files {
      seen_in_commit.insert(file.path_str());
  }
  for path_cow in &seen_in_commit {
      let entry = accum.entry(path_cow.as_ref().to_string()).or_default();
      // ... same logic
  }
  ```
  This would avoid an allocation for duplicate files within the same commit. For a function explicitly designed for "bulk persistence" workloads, matching the allocation discipline of the sibling function is appropriate.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`store_hotspots`, `store_risks`, `store_cochanges` are structurally identical** - `storage_ops.rs:29-98` (Confidence: 72%) -- These three methods follow the exact same DELETE + prepare + loop + commit pattern, differing only in table name, SQL, and field bindings. A generic `replace_table()` helper accepting a SQL string and a row-to-params closure would reduce 70 lines to ~25. However, the explicit form is more readable for a module with only three tables, so this is a style judgment.

- **`meta` timestamp stored as Unix epoch string, doc says ISO-8601** - `storage.rs:52` vs `storage_ops.rs:308-312` (Confidence: 65%) -- The `META_LAST_UPDATED` doc comment says "ISO-8601 UTC timestamp" but `sync()` stores `SystemTime::now().as_secs().to_string()`, which is a plain Unix epoch integer string. This is a documentation/implementation mismatch, not a complexity issue per se, but it could confuse maintainers.

- **`compute_file_temporal_stats` inner loop nesting depth reaches 3** - `scoring.rs:227-261` (Confidence: 60%) -- The for-commit/for-file/if-fix structure is at nesting depth 3, which is within acceptable limits but approaches the complexity warning zone. The sibling `compute_file_risk_scores` avoids this by pre-computing fix flags into a Vec. Applying the same pattern here would flatten the loop body.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Complexity Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The PR is well-structured overall. The three-file split (storage.rs, storage_ops.rs, storage_types.rs) is a good decision that keeps every file well under 400 lines. Row types are simple, flat data structures. The `open()` method has low cyclomatic complexity. The migration system is clean and idempotent.

The single blocking issue is the duplicated insert logic in `sync()`, which makes the function longer than necessary (76 lines) and creates a maintenance risk with two copies of every INSERT statement. Extracting shared helpers from `sync()` and the individual `store_*()` methods would bring the function under 30 lines and eliminate the duplication.
