# Reliability Review Report

**Branch**: main (353ef87)
**Date**: 2026-05-24
**Scope**: `crates/rskim-search/src/cochange/` -- co-change matrix builder, binary format, mmap reader (~1,881 lines, 10 files)

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Missing `flush()` before `persist()` in atomic_write** - `builder.rs:294-306`
**Confidence**: 85%
- Problem: `atomic_write` calls `tmp.write_all(data)` then `tmp.persist(path)` without an explicit `flush()` call on the `BufWriter`/file handle. While `NamedTempFile` wraps a raw `File` (not a `BufWriter`), `persist()` does not call `sync_data()` or `sync_all()`. On power loss between `write_all` and the OS flushing dirty pages, the renamed file could be empty or truncated. The CRC32 check on read would catch this, but the user would see "corrupt index" rather than a clean rebuild prompt.
- Fix: Add `tmp.as_file().sync_all()?;` before `persist()` for crash-safe writes:
```rust
fn atomic_write(dir: &Path, path: &Path, data: &[u8]) -> Result<()> {
    let mut tmp = NamedTempFile::new_in(dir)?;
    use std::io::Write as _;
    tmp.write_all(data)?;
    tmp.as_file().sync_all()?; // ensure data reaches disk before rename

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o644))?;
    }

    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
```

**`unwrap_or(u32::MAX)` silently truncates stats instead of returning an error** - `builder.rs:106-107`
**Confidence**: 82%
- Problem: `u32::try_from(pairs.len()).unwrap_or(u32::MAX)` silently saturates the stats `pair_count` and `file_count` to `u32::MAX` if the HashMap has more entries than `u32::MAX` can hold. While `MAX_PAIRS` (2M) makes this practically unreachable for pair_count, `file_counts.len()` has no explicit cap. More importantly, the same values are later re-derived with `u32::try_from(...).map_err(...)` in `serialize()` (lines 266-277) which returns a proper error. The stats values will therefore be wrong (saturated to `u32::MAX`) while the serialization will fail -- a confusing inconsistency.
- Fix: Use the same `map_err` pattern consistently, or derive stats from the serialize result:
```rust
stats.pair_count = u32::try_from(pairs.len()).map_err(|_| {
    SearchError::IndexCorrupted(format!("pair_count {} exceeds u32::MAX", pairs.len()))
})?;
stats.file_count = u32::try_from(file_counts.len()).map_err(|_| {
    SearchError::IndexCorrupted(format!("file_count {} exceeds u32::MAX", file_counts.len()))
})?;
```

### LOW

**`pairs_for_file` linear scan with no result-size bound** - `reader.rs:189-208`
**Confidence**: 80%
- Problem: `pairs_for_file` performs an O(n) scan over all pair entries and collects into an unbounded `Vec`. With `MAX_PAIRS = 2_000_000`, a single file could theoretically appear in all 2M pairs, producing a 2M-element Vec. The doc comment acknowledges the O(n) scan and notes a future optimization, but there is no `top_k` / capacity limit on the results vector. This is a bounded-but-large allocation.
- Fix: Consider accepting an optional `limit: usize` parameter or using `Vec::with_capacity(min(n, reasonable_cap))`. This is low severity because the 2M cap on pairs already bounds the worst case, but adding a `top_k` parameter would make the API more defensible:
```rust
pub fn pairs_for_file(&self, file_id: FileId, limit: Option<usize>) -> Result<Vec<(FileId, u32)>> {
    // ... scan logic ...
    results.truncate(limit.unwrap_or(results.len()));
    Ok(results)
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Redundant min/max after sort+dedup** - `builder.rs:179-180` (Confidence: 70%) -- After `ids.sort_unstable(); ids.dedup();`, the vector is sorted ascending, so `ids[i] < ids[j]` is guaranteed when `i < j`. The `ids[i].min(ids[j])` / `ids[i].max(ids[j])` calls are redundant (they always return `ids[i]` and `ids[j]` respectively). Simplifying to `let a = ids[i]; let b = ids[j];` would be marginally clearer, though the `debug_assert!` below already documents the invariant.

- **No `file_count` safety cap analogous to `MAX_PAIRS`** - `builder.rs` (Confidence: 65%) -- `MAX_PAIRS` caps pair accumulation, but `file_commit_counts` grows unbounded (limited only by the number of distinct files in the path_map). In practice this is bounded by `path_map.len()` which the caller controls, but an explicit cap (e.g., `MAX_FILES`) would be more defensive.

- **`is_multiple_of` nightly API surface risk** - `format.rs:256` (Confidence: 60%) -- `usize::is_multiple_of` was stabilized in Rust 1.73.0 and the project uses 1.94.1, so this is safe today. However, if MSRV is ever specified below 1.73, this would break. A `% PAIR_ENTRY_SIZE != 0` check is equivalent and universally portable.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 1 |
| Should Fix | - | - | 0 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Passed Checks

The following reliability properties were verified and found to be correctly implemented:

1. **Bounded loops** -- All iteration is bounded:
   - `accumulate_pairs`: iterates over `history.commits` (finite input), inner loop bounded by `COUPLING_MAX_FILES` (50) after skip check
   - `pairs_for_file`: iterates `0..n` where n is derived from validated mmap length
   - `lookup_pair` and `file_commits`: standard binary search with `lo < hi` convergence
   - `MAX_PAIRS = 2_000_000` hard cap prevents unbounded HashMap growth

2. **Atomic file writes** -- Builder uses `tempfile::NamedTempFile::new_in()` + `persist()` (rename), so readers never observe a partial write. The temp file is created in the same directory as the target, ensuring same-filesystem rename semantics.

3. **Corrupt file detection** -- Reader validates magic bytes, format version, exact file size match, and CRC32 checksum before returning. All four checks produce descriptive `IndexCorrupted` errors. A truncated, garbage, or bit-flipped file will be rejected.

4. **Error propagation** -- All Results are properly propagated with `?`. No `unwrap()` or `expect()` in production code paths. The `unwrap_or(u32::MAX)` on stats lines is the only non-`?` pattern (flagged above).

5. **Checked arithmetic** -- All size computations use `checked_mul` / `checked_add` with explicit overflow errors, protecting 32-bit targets.

6. **Memory bounds from malicious files** -- Reader validates `mmap.len() == pairs_end` (computed from header counts) before any slice operations. A crafted header with `pair_count = u32::MAX` would fail the `checked_mul` or size-mismatch check, preventing excessive allocation.

7. **Precondition assertions** -- `debug_assert!(a < b)` enforces canonical pair invariant in hot path. `#[must_use]` on `new()`, `build()`, `pair_count()`, `jaccard()`, `open()`.

8. **Resource cleanup** -- `NamedTempFile` is dropped (and temp file cleaned up) if any step fails before `persist()`. Mmap handle is owned by `CochangeMatrixReader` and dropped when the reader is dropped. No leaked file descriptors.

9. **Saturating arithmetic for counters** -- `commits_processed`, `commits_skipped_too_large`, `unknown_paths_skipped`, and per-file commit counts all use `saturating_add`, preventing panic on overflow.

10. **Deduplication invariant** -- `ids.sort_unstable(); ids.dedup();` prevents self-pairs from duplicate paths in a single commit, with test coverage (`test_duplicate_paths_in_commit_deduplicated`).

11. **Test coverage of safety caps** -- `test_max_pairs_safety_cap_returns_index_corrupted`, `test_coupling_max_files_skip_exceeds`, `test_coupling_max_files_exactly_at_limit_processed`, `test_crc32_mismatch_detected`, `test_open_corrupt_file_fails` all exercise the defensive boundaries.
