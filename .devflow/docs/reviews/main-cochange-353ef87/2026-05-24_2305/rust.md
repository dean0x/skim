# Rust Review Report

**Branch**: main (commit 353ef87)
**Date**: 2026-05-24
**Scope**: `crates/rskim-search/src/cochange/` -- 10 files, ~1,881 lines added

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Redundant `min`/`max` after `sort_unstable` + `dedup`** - `builder.rs:179-180`
**Confidence**: 90%
- Problem: The `ids` vector is sorted and deduplicated on lines 167-168, so for any `i < j`, `ids[i] < ids[j]` is guaranteed. The `.min()`/`.max()` calls on lines 179-180 are redundant -- they always return `ids[i]` and `ids[j]` respectively.
- Impact: Unnecessary branching in the hot pair-generation loop. Not a correctness bug, but clutters the code and adds false complexity. The `debug_assert!` on line 183 confirms the invariant that `a < b` is already guaranteed.
- Fix:
  ```rust
  // Replace lines 179-180:
  let a = ids[i];
  let b = ids[j];
  ```

**`#[must_use]` on `open()` has misleading reason string** - `reader.rs:69`
**Confidence**: 82%
- Problem: The `#[must_use]` attribute on `open()` says `"dropping the reader immediately means no queries can be made"`. While technically true, `#[must_use]` is automatically applied to `Result` in Rust, so this attribute is redundant for the `Result` wrapper. The same applies to `builder.rs:51` and `builder.rs:76`. The existing codebase (`index/reader.rs`) does not add `#[must_use]` to `Result`-returning functions.
- Impact: Inconsistency with the rest of the codebase's `Result`-returning function patterns.
- Fix: Remove `#[must_use]` from functions that return `Result<T>` (lines 51, 76 in builder.rs and line 69 in reader.rs). Keep `#[must_use]` only on `pair_count()` (line 135) and `jaccard()` (line 154) if desired for semantic emphasis, though `Result` already carries this attribute.

**`SearchError::IndexCorrupted` used for non-corruption error (safety cap exceeded)** - `builder.rs:187-189`
**Confidence**: 85%
- Problem: When the pair count exceeds `MAX_PAIRS`, the error returned is `SearchError::IndexCorrupted("co-change pair count exceeds safety limit")`. But the index is not corrupted -- the input data is simply too large. This is a resource limit violation, not data corruption. The existing `SearchError` enum has no `ResourceExhausted` or `LimitExceeded` variant, but using `IndexCorrupted` for a non-corruption condition conflates two semantically different error classes.
- Impact: Consumers matching on `SearchError::IndexCorrupted` may misinterpret a legitimate "too much data" condition as file corruption, potentially triggering unnecessary re-indexing.
- Fix: Consider adding a new variant to `SearchError` (e.g., `LimitExceeded { limit: usize, actual: usize, context: String }`) or at minimum, use a clear error message that distinguishes it from actual corruption. If adding a variant is out of scope for this PR, document this choice as a known limitation.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`pairs_for_file()` performs O(n) linear scan over all pairs** - `reader.rs:189-207`
**Confidence**: 80%
- Problem: The doc comment on line 177-182 acknowledges this is O(pair_count). At the MAX_PAIRS cap of 2M pairs, this reads ~24 MB per call. Since pairs are sorted by `(file_a, file_b)`, binary search could find the contiguous range where `file_a == id` in O(log n), and a second pass could scan for `file_b == id` entries. The doc mentions this as a future optimization.
- Impact: At scale this becomes a performance bottleneck for callers that query multiple files. The linear scan is fine for initial release but should be tracked.
- Fix: No blocking fix needed -- the documentation honestly states the complexity. Consider adding a `// TODO:` comment referencing a specific issue number for tracking.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Consider `HashMap::entry` pattern for `pair_counts` check** - `builder.rs:186-192` (Confidence: 65%) -- The current pattern calls `contains_key` then `entry().or_insert()`, performing two hash lookups for new keys. Using `entry()` with `VacantEntry`/`OccupiedEntry` matching could reduce this to one lookup, though the limit check makes the logic slightly awkward with the Entry API.

- **`CochangeStats` fields are all `u32` but `MAX_PAIRS` is `usize`** - `builder.rs:106-107` (Confidence: 70%) -- The `unwrap_or(u32::MAX)` saturating cast on lines 106-107 silently clamps pair/file counts. On 64-bit systems with large repos, `pair_counts.len()` could theoretically exceed `u32::MAX` before the `MAX_PAIRS` (2M) cap triggers. In practice, `MAX_PAIRS=2M` ensures this cannot happen, but the silent saturation hides a hypothetical bug if someone raises `MAX_PAIRS` above 4B.

- **`atomic_write` does not call `fsync`/`flush` before persist** - `builder.rs:294-306` (Confidence: 62%) -- `tmp.persist()` (rename) is atomic on POSIX, but without `fsync` the data may not be durable on crash. Whether durability matters depends on whether a missing `.skcc` file triggers a rebuild on next startup (which it should).

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 3 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Passed Checks

- **Ownership and borrowing**: Clean. No unnecessary clones. Borrows `&HistoryResult`, `&HashMap<PathBuf, FileId>` appropriately. `CochangeMatrixReader` stores owned `Mmap` which is correct for a long-lived reader.
- **Error handling**: Consistent use of `Result<T>` with `?` operator throughout. `thiserror`-based `SearchError` enum. No `.unwrap()` outside test code. `#[allow(clippy::unwrap_used)]` only in test modules.
- **Unsafe code**: Single `unsafe` block for `Mmap::map()` in `reader.rs:75` -- justified, documented with `// SAFETY` comment, and consistent with existing `index/reader.rs` pattern.
- **Type design**: `FileId` newtype used correctly from existing types. `CochangeStats` is a clean data carrier with Serialize/Deserialize.
- **Binary serialization**: Explicit little-endian encoding via `.to_le_bytes()` and `from_le_bytes()`. No reliance on platform endianness. Fixed-size structs with documented layouts. CRC32 checksum validation.
- **Checked arithmetic**: Overflow-safe with `checked_mul`, `checked_add`, `saturating_add` throughout. No unchecked arithmetic on user-controlled values.
- **Mmap safety**: Read-only mapping. Module-level documentation acknowledges the inherent TOCTOU limitation of mmap. Consistent with existing `index/reader.rs` approach.
- **Atomic writes**: Uses `tempfile::NamedTempFile::persist()` for atomic rename. Explicit `0o644` permissions set on Unix.
- **Idiomatic Rust**: Good use of iterators (`sort_unstable_by_key`, `collect`, `map`). Pattern matching with `match`. `debug_assert!` for invariants in hot paths.
- **Test quality**: 14 builder tests, 10 format tests, 13 reader tests -- behavior-focused, testing edge cases (empty, self-pairs, dedup, corruption, CRC mismatch, Send+Sync compile check). Shared test helpers avoid duplication.
- **Safety caps**: `COUPLING_MAX_FILES=50` and `MAX_PAIRS=2M` prevent unbounded growth. Both are tested at boundary conditions.
- **Clippy compliance**: `is_multiple_of` usage is consistent with edition 2024 and existing codebase patterns.

### Assessment

Well-structured Rust code that follows existing codebase patterns closely. The binary format is well-documented with clear layout diagrams. Error handling is thorough. The main issues are minor: redundant min/max calls, inconsistent `#[must_use]` usage, and a semantic mismatch in error variant choice. No blocking issues found.
