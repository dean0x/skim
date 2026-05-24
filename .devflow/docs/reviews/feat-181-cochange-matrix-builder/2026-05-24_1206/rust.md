# Rust Review Report

**Branch**: feat/181-cochange-matrix-builder -> main
**Date**: 2026-05-24T12:06

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Missing `#[must_use]` on public Result-returning methods** (5 occurrences) -- Confidence: 85%
- `builder.rs:50` (`CochangeMatrixBuilder::new`)
- `builder.rs:74` (`CochangeMatrixBuilder::build`)
- `reader.rs:62` (`CochangeMatrixReader::open`)
- `reader.rs:119` (`CochangeMatrixReader::pair_count`)
- `reader.rs:137` (`CochangeMatrixReader::jaccard`)
- Problem: All five public functions return `Result<T>` but lack `#[must_use]`. Callers can silently discard the Result without a compiler warning, violating the Rust API guidelines (C-MUST-USE). The `build()` method is particularly important since it performs I/O and accumulation -- silently ignoring its result means the caller never learns whether the matrix was written.
- Fix: Add `#[must_use]` to each public method signature. For example:
```rust
#[must_use = "this returns a Result that should be checked"]
pub fn new(output_dir: PathBuf) -> Result<Self> { ... }
```

### MEDIUM

**Redundant `min`/`max` in pair generation when IDs are already sorted** -- `builder.rs:148-149` -- Confidence: 85%
- Problem: After `ids.sort_unstable()` and `ids.dedup()` on line 136-137, the vector is sorted ascending. The inner loop with `j > i` guarantees `ids[i] <= ids[j]` (strict `<` due to dedup). The explicit `ids[i].min(ids[j])` / `ids[i].max(ids[j])` calls are redundant -- `ids[i]` is always `a` and `ids[j]` is always `b`. This adds unnecessary branch instructions in a hot loop that runs O(n^2) per commit.
- Fix:
```rust
let a = ids[i];
let b = ids[j];
debug_assert!(a < b, "canonical pair invariant: a({a}) < b({b})");
```

**`pairs_for_file` linear scan could use early termination for sorted pairs** -- `reader.rs:163-182` -- Confidence: 80%
- Problem: Since pair entries are sorted by `(file_a, file_b)`, once `entry.file_a > id`, no further entry can have `file_a == id`. The scan still needs to check `file_b == id` in the remaining entries, but entries where `file_a == id` are contiguous and could be found via binary search rather than linear scan. For 2M pairs, this is a measurable difference. The current O(pair_count) scan works correctly but leaves performance on the table for large matrices.
- Fix: Consider a two-phase approach: binary search to find the range where `file_a == id`, then continue scanning for `file_b == id`. Alternatively, document this as a known O(n) cost and defer optimization.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`atomic_write` does not `flush()` before `persist()`** -- `builder.rs:254-260` (Confidence: 70%) -- `NamedTempFile::persist()` renames the file but does not call `fsync`. On crash between `write_all` and the OS flushing buffers, the renamed file could contain partial data. For an index file that can be rebuilt, this is low risk, but adding `tmp.as_file().sync_all()?` before `persist()` would provide crash-consistency.

- **`Jaccard` returns 0.0 for self-pairs rather than 1.0** -- `reader.rs:138-140` (Confidence: 65%) -- Mathematically, `Jaccard(A, A) = 1.0` (a set has perfect overlap with itself). The code returns 0.0. The PR description explicitly calls this out as intentional ("returns 0.0 for self-pairs"), so this is a design choice rather than a bug. However, it deviates from the standard Jaccard definition. If downstream consumers expect standard Jaccard semantics, this could cause confusion.

- **Duplicate `make_history` / `make_path_map` helpers across test files** -- `builder_tests.rs:17-51`, `reader_tests.rs:19-53` (Confidence: 75%) -- The test helper functions `make_history` and `make_path_map` are duplicated verbatim across builder_tests.rs and reader_tests.rs. Extracting them into a shared `test_utils.rs` or a `#[cfg(test)] mod test_helpers` in `mod.rs` would reduce maintenance burden.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The implementation is well-structured with excellent discipline around error handling (consistent `Result` propagation, no `unwrap()` outside tests), overflow protection (`checked_mul`/`checked_add`, `saturating_add`), and binary format correctness (encode/decode symmetry verified by roundtrip tests, CRC32 integrity checks). The safety caps (`COUPLING_MAX_FILES`, `MAX_PAIRS`) are checked at the right boundaries (before insertion, not after). The `unsafe` block for mmap is properly documented with a `// SAFETY:` comment. All 43 tests pass, clippy is clean with zero warnings.

The one HIGH-severity finding (missing `#[must_use]`) is a straightforward addition that follows Rust API guidelines and the project's own quality standards. The MEDIUM findings are performance observations, not correctness issues. Overall this is a clean, well-tested module.
