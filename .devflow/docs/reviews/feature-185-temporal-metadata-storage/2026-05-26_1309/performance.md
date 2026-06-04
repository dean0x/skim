# Performance Review Report

**Branch**: feature/185-temporal-metadata-storage -> main
**Date**: 2026-05-26T13:09

## Issues in Your Changes (BLOCKING)

### MEDIUM

**String allocations in `compute_file_temporal_stats` dedup loop: `seen_in_commit` allocates a new `String` per unique file per commit** - `crates/rskim-search/src/temporal/scoring.rs:245-252`
**Confidence**: 82%
- Problem: The `seen_in_commit: HashSet<String>` is cleared and repopulated every commit iteration. Each unique file path in a commit triggers `path_cow.into_owned()` to insert into `seen_in_commit`, allocating a fresh `String`. For repositories with many commits touching the same files, this produces O(commits * files_per_commit) allocations even though the path strings are already owned in the `accum` HashMap. The sibling function `compute_file_risk_scores` avoids this by not needing per-commit deduplication (it does not deduplicate within a commit) and probing `accum` directly with borrowed `&str`.
- Fix: Consider using a `HashSet<&str>` that borrows from `commit.changed_files[*].path` (which is a `PathBuf` that outlives the loop body), avoiding the `into_owned()` call entirely for dedup purposes. The `path_str()` returns a `Cow` that borrows from the `PathBuf`, so extracting the `&str` from each `FileChangeInfo.path` directly as `&str` would remove the allocation:

```rust
seen_in_commit.clear();
for file in &commit.changed_files {
    let path_cow = file.path_str();
    let path_ref: &str = &path_cow;
    // Cow borrows from file.path which lives for the commit iteration
    // but the Cow itself is dropped here — we cannot store &path_cow
    // because it does not outlive the for body.
}
```

Note: The `Cow` lifetime is tied to `file.path` (which lives for the outer `for commit in commits` loop), but the current code calls `into_owned()` because `HashSet<String>` requires ownership. A `HashSet<usize>` keyed on the index into `commit.changed_files` would be zero-allocation but less readable. Given that typical commits touch 1-10 files, the allocation cost per commit is small. This is a MEDIUM severity because the overall function is still single-pass O(commits * avg_files) and the PR's stated purpose is to cache the results so this function runs infrequently. The borrow-first pattern for `accum` (line 256) already prevents the heavier repeated allocation.

**`load_*` methods collect all rows into a `Vec` without size hint from SQLite** - `crates/rskim-search/src/temporal/storage_ops.rs:183-201`
**Confidence**: 80%
- Problem: All three `load_*` methods (`load_hotspots`, `load_risks`, `load_cochanges`) use `.collect::<Result<Vec<_>, _>>()` on the `query_map` iterator. `Vec::collect()` starts with a small allocation and grows geometrically. For the documented 10k-row workload this means several reallocations. The row count is known to SQLite (via `SELECT COUNT(*)`) but not exposed to the Rust allocator.
- Fix: For the current 10k-row target this is minor (a few extra memcpy calls during reallocation). If you want to optimize, you could run a `SELECT COUNT(*) FROM hotspot` first and use `Vec::with_capacity()`, but this adds a second query and is likely not worth the complexity for 10k rows. The current approach is acceptable given the performance tests pass well under threshold. No code change recommended unless profiling shows reallocation as a bottleneck at higher row counts.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`compute_file_temporal_stats` allocates a `HashSet<String>` per-commit for dedup when most commits have no duplicates** - `crates/rskim-search/src/temporal/scoring.rs:225` (Confidence: 65%) -- The `HashSet` is allocated once and reused (`.clear()` each iteration), which is good, but a `SmallVec`-based linear scan might be faster for the common case of 1-10 files per commit where hash overhead exceeds linear search cost. However, this would add a dependency and the current approach is correct and readable.

- **No `PRAGMA cache_size` tuning in `TemporalDb::open`** - `crates/rskim-search/src/temporal/storage.rs:192` (Confidence: 62%) -- SQLite's default page cache is 2MB. For bulk-load workloads with 10k+ rows across 3 tables, a larger cache (e.g., `PRAGMA cache_size = -8000` for 8MB) could reduce I/O during `sync()`. However, the performance tests already pass well under threshold, and the DB is WAL-mode which is already optimized for this pattern. Only relevant if profiling shows cache pressure at scale.

- **`sync()` performs three sequential DELETE + INSERT loops rather than using `DELETE FROM hotspot; DELETE FROM risk; DELETE FROM cochange;` as a single batch** - `crates/rskim-search/src/temporal/storage_ops.rs:328-330` (Confidence: 60%) -- Each `insert_*_in_tx` helper calls `DELETE FROM <table>` individually. Combining all three DELETEs into one `execute_batch` call would save two round-trips to SQLite's query planner. Marginal at 10k rows but could matter at the 500k ceiling.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 8/10
**Recommendation**: APPROVED

### Rationale

The performance design of this PR is solid. Key positives:

1. **WAL mode + NORMAL synchronous**: Correct pragmas for a write-heavy cache database. WAL avoids reader-writer contention and NORMAL synchronous is the right trade-off for a cache that can be rebuilt.

2. **`prepare_cached` for batch inserts**: All insert helpers use `prepare_cached`, avoiding repeated SQL compilation across rows and across calls. This is the single most impactful SQLite performance pattern for batch operations.

3. **Single-transaction sync**: The `sync()` method wraps all table replacements in one transaction, producing a single WAL commit rather than 3+ separate commits. This is critical for write throughput.

4. **Capacity guards**: The `MAX_ROWS_PER_TABLE = 500_000` bound prevents unbounded INSERT loops, which would be a latency and memory regression risk.

5. **Borrow-first allocation pattern**: Both `compute_file_risk_scores` and `compute_file_temporal_stats` probe `accum` with `&str` before calling `into_owned()`, reducing allocations from O(total_file_touches) to O(unique_files). The capacity heuristic `(commits.len() / 4).clamp(64, 50_000)` avoids both over-allocation and excessive rehashing.

6. **Performance acceptance tests with debug/release split thresholds**: The perf tests use `cfg!(debug_assertions)` to give 5x headroom in debug builds, avoiding flaky CI failures while still enforcing tight release ceilings.

7. **`saturating_add` on u32 counters**: Prevents overflow without panic -- correct for counters that should never wrap.

The two MEDIUM findings are about allocation efficiency in paths that are expected to run infrequently (the whole point of this PR is to cache these results). Neither warrants blocking the merge.
