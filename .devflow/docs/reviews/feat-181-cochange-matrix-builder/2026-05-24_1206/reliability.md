# Reliability Review Report

**Branch**: feat/181-cochange-matrix-builder -> main
**Date**: 2026-05-24T12:06

## Issues in Your Changes (BLOCKING)

### HIGH

**Unchecked multiplication in `serialize()` can panic on overflow** - `builder.rs:204-205`
**Confidence**: 90%
- Problem: The `serialize` function computes `file_entries.len() * FILE_COMMIT_ENTRY_SIZE` and `pair_entries.len() * PAIR_ENTRY_SIZE` using plain `*` which panics on overflow in debug mode and wraps in release mode. The reader (`reader.rs:72-81`) correctly uses `checked_mul` for the same computation, but the writer does not. While `MAX_PAIRS` caps pairs at 2M (which fits comfortably: 2M * 12 = 24MB), `file_commit_counts` has no explicit cap -- it is bounded only by `path_map.len()` which the caller controls. A sufficiently large `path_map` with crafted input could trigger this.
- Fix: Use `checked_mul` with error propagation, matching the reader's pattern:
```rust
let fc_bytes = file_entries.len()
    .checked_mul(FILE_COMMIT_ENTRY_SIZE)
    .ok_or_else(|| SearchError::IndexCorrupted("fc_bytes overflow".into()))?;
let pair_bytes = pair_entries.len()
    .checked_mul(PAIR_ENTRY_SIZE)
    .ok_or_else(|| SearchError::IndexCorrupted("pair_bytes overflow".into()))?;
```

**Unchecked addition in `serialize()` total size computation** - `builder.rs:244`
**Confidence**: 85%
- Problem: `let total = HEADER_SIZE + fc_bytes + pair_bytes;` uses unchecked addition. If `fc_bytes` or `pair_bytes` were large (from a very large `path_map` or near-limit pairs), this could overflow. Again, the reader (`reader.rs:82-85`) uses `checked_add` for the equivalent computation.
- Fix: Use `checked_add` matching the reader pattern:
```rust
let total = HEADER_SIZE
    .checked_add(fc_bytes)
    .and_then(|s| s.checked_add(pair_bytes))
    .ok_or_else(|| SearchError::IndexCorrupted("total size overflow".into()))?;
```

### MEDIUM

**Unchecked arithmetic in `file_commit_slice` and `pairs_slice` helper methods** - `reader.rs:219, 225-226`
**Confidence**: 82%
- Problem: The private helper methods `file_commit_slice()` and `pairs_slice()` recompute `(header.file_count as usize) * FILE_COMMIT_ENTRY_SIZE` and `(header.pair_count as usize) * PAIR_ENTRY_SIZE` using unchecked multiplication and addition. While `open()` validates these same computations with `checked_mul`/`checked_add` before constructing the struct, the helpers recompute them independently. This is safe by construction (validated at open-time), but violates the reliability principle that every arithmetic operation on untrusted-derived data should use checked arithmetic. The values come from the header which was validated, but the duplication means a future refactor that skips validation in `open()` would silently introduce panics here.
- Fix: Cache the validated byte offsets in the struct at construction time instead of recomputing:
```rust
pub struct CochangeMatrixReader {
    header: SkccHeader,
    mmap: Mmap,
    fc_start: usize,   // always HEADER_SIZE
    fc_end: usize,     // validated at open-time
    pairs_end: usize,  // validated at open-time
}
```
Then `file_commit_slice` and `pairs_slice` become simple `&self.mmap[self.fc_start..self.fc_end]`.

**No explicit cap on `file_commit_counts` HashMap growth** - `builder.rs:108`
**Confidence**: 80%
- Problem: The `pair_counts` HashMap has an explicit `MAX_PAIRS` safety cap (line 155), but `file_commit_counts` has no analogous cap. It grows proportionally to `path_map.len()` which is caller-controlled. In practice, the number of unique files in git history is bounded by filesystem limits, but the reliability principle says every resource should have an explicit bound. A monorepo with millions of tracked files passed via `path_map` could cause excessive memory use.
- Fix: Add a `MAX_FILES` constant (e.g., 500,000) and check before inserting into `file_commit_counts`, or document that the bound is inherited from `path_map.len()` which is caller-managed. A comment asserting this invariant would be the minimum fix:
```rust
// file_commit_counts is bounded by path_map.len() which the caller
// controls; no separate cap needed because path_map is finite.
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`pairs_for_file` linear scan with no result cap** - `reader.rs:163-182` (Confidence: 65%) -- The linear scan over all pair entries collects all matches into a Vec with no upper bound on results. With MAX_PAIRS=2M, a file that co-changes with many others could produce a large result Vec. A `top_k` parameter would provide defense in depth, though the 2M cap from the builder makes this unlikely to be problematic in practice.

- **`compute_checksum` in reader uses single-pass over concatenated payload** - `reader.rs:95-96` (Confidence: 60%) -- The reader validates CRC32 over `mmap[HEADER_SIZE..expected_size]` as a single contiguous slice, while the builder computes it with two separate `hasher.update()` calls (fc_buf then pair_buf). These produce the same result because CRC32 is streaming (update is associative), but the asymmetry could confuse future maintainers. The equivalence is correct but could benefit from a doc comment explaining why.

- **`is_multiple_of` is relatively new stdlib API** - `format.rs:256` (Confidence: 60%) -- `usize::is_multiple_of` was stabilized in Rust 1.83. The crate uses edition 2024 and builds on 1.94, so this is fine today, but if MSRV is ever backported, this would need to become `pairs_data.len() % PAIR_ENTRY_SIZE != 0`.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | - | 0 | 0 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The code demonstrates strong reliability practices overall: bounded iteration (COUPLING_MAX_FILES, MAX_PAIRS), saturating arithmetic for counters, checked arithmetic in the reader's open() path, atomic writes, CRC32 integrity verification, and comprehensive error handling with Result types throughout. The two HIGH findings are about inconsistent use of checked arithmetic between the writer and reader paths -- the reader correctly uses checked_mul/checked_add, but the writer uses unchecked operators for the same computations. Fixing these for consistency would bring the module to a high reliability standard.
