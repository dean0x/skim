# Performance Review Report

**Branch**: main (PR #253)
**Date**: 2026-05-26
**Scope**: TemporalDb SQLite persistence layer, `compute_file_temporal_stats`, SearchError::Database variant

## Issues in Your Changes (BLOCKING)

### HIGH

**Unnecessary String allocations in `compute_file_temporal_stats` hot loop** - `scoring.rs:244-250`
**Confidence**: 90%
- Problem: The deduplication loop allocates a new `String` on every call to `file.path_str().into_owned()` (line 245) even when the path was already seen in a previous commit. Then it clones the path *again* on line 250 via `accum.entry(path.clone())`. This is O(total_file_touches) allocations for dedup plus O(unique_files_per_commit) clones for the accumulator. By contrast, the sibling function `compute_file_risk_scores` (lines 141-147) avoids this by probing with a borrowed `&str` first and only calling `into_owned()` for genuinely new entries.
- Fix: Apply the same borrow-then-own pattern from `compute_file_risk_scores`. Replace the `HashSet<String>` dedup approach with a borrowed probe:

```rust
// Replace the seen_in_commit HashSet approach with borrowed probing.
// For dedup: use a HashSet<&str> borrowing from commit.changed_files paths,
// and for accumulator insertion, probe with &str first:
let mut seen_in_commit: HashSet<&str> = HashSet::new();

for commit in commits {
    let is_fix = super::is_fix_commit(&commit.message);
    // ... elapsed_days computation unchanged ...

    seen_in_commit.clear();
    for file in &commit.changed_files {
        let path_cow = file.path_str();
        let path_ref: &str = &path_cow;
        // Skip if already seen in this commit (borrow, no alloc).
        if !seen_in_commit.insert(path_ref) {
            continue;
        }
        // Probe accumulator with borrowed &str first.
        if let Some(entry) = accum.get_mut(path_ref) {
            entry.total_commits += 1;
            // ... increment other fields ...
        } else {
            let mut stats = FileTemporalStats::default();
            stats.total_commits = 1;
            // ... set other fields ...
            accum.insert(path_cow.into_owned(), stats);
        }
    }
}
```

Note: The `HashSet<&str>` approach requires the `path_cow` to outlive the `seen_in_commit` set within each commit iteration. The exact borrow-checker-satisfying pattern may require collecting `Cow`s into a local `Vec` first, or using a `HashSet<String>` only for the dedup set but still probing the accumulator with `&str`. The key optimization is avoiding the `path.clone()` on the `accum.entry()` call -- that clone happens for every file in every commit, not just new entries.

### MEDIUM

**`sync()` duplicates all INSERT logic from individual `store_*` methods** - `storage_ops.rs:257-325`
**Confidence**: 82%
- Problem: The `sync()` method manually replicates the DELETE + batch INSERT loop for all three tables rather than calling `store_hotspots`, `store_risks`, and `store_cochanges` within a shared transaction. This is not a runtime performance issue today, but it prevents a future optimization: if the individual store methods are enhanced (e.g., with batch size chunking for very large datasets), `sync()` will not benefit. More critically, duplicate SQL strings mean twice the compiled statements in the SQLite statement cache.
- Fix: Refactor `store_*` methods to accept an optional transaction reference (or extract the INSERT loop into private helpers that take `&Transaction`), then have `sync()` compose them. This is a moderate refactor and could be deferred if the current 10k-row performance targets are met.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Missing `PRAGMA synchronous = NORMAL` for WAL mode** - `storage.rs:187-188`
**Confidence**: 85%
- Problem: SQLite defaults to `synchronous = FULL`, which issues an fsync on every commit. When WAL mode is enabled, `synchronous = NORMAL` is the recommended setting (per SQLite documentation) because WAL already provides crash safety guarantees. The current code enables WAL but does not set `synchronous = NORMAL`, meaning every `sync()` and `store_*()` call pays an unnecessary fsync. On spinning disks or network-attached storage this can dominate write latency. For 10k rows in a single transaction the impact is modest (one fsync per COMMIT, not per INSERT), but it is still an unnecessary overhead.
- Fix: Add `PRAGMA synchronous = NORMAL;` after enabling WAL mode:

```rust
conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
    .map_err(db_err)?;
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Load methods allocate per-row without pre-sizing the Vec** - `storage_ops.rs:132-143` (Confidence: 65%) -- The `load_*` methods use `.collect::<Vec<_>>()` without a size hint. For large tables, a `SELECT COUNT(*)` pre-query or `Vec::with_capacity` could reduce re-allocation. This is unlikely to matter at 10k rows but could become relevant at 100k+.

- **`compute_file_temporal_stats` calls `is_fix_commit` inline per commit** - `scoring.rs:228` (Confidence: 70%) -- Unlike `compute_file_risk_scores` which pre-classifies all commits into a `Vec<bool>` before the accumulation loop, `compute_file_temporal_stats` calls `is_fix_commit` inline. The per-call cost is low (LazyLock regex, no recompilation), but pre-classification is a consistent pattern in this module and the two functions handle the same `commits` slice. Aligning the pattern would avoid branching into the regex on every iteration.

- **Performance tests use wall-clock `Instant::now()` assertions** - `storage_perf_tests.rs:145-154` (Confidence: 75%) -- Wall-clock assertions (`elapsed.as_millis() < 100`) are non-deterministic and can fail on slow CI runners, VMs, or under load. Consider using criterion benchmarks for absolute performance targets and keeping unit tests focused on correctness. Alternatively, multiply the thresholds by 3-5x for CI resilience, or gate the assertions behind a `#[cfg(not(feature = "ci"))]` flag.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The SQLite persistence layer is well-structured: WAL mode, prepared statement caching, atomic transactions, and proper busy timeout are all in place. Performance tests confirm 10k-row targets are met. The main concern is the allocation pattern in `compute_file_temporal_stats` which introduces unnecessary String allocations in the hot loop -- a pattern the adjacent `compute_file_risk_scores` already solved with borrow-then-own. Adding `synchronous = NORMAL` is a straightforward win for write latency with no safety trade-off under WAL mode.
