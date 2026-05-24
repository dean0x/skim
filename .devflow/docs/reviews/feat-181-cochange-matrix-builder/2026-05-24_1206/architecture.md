# Architecture Review Report

**Branch**: feat/181-cochange-matrix-builder -> main
**Date**: 2026-05-24T12:06

## Issues in Your Changes (BLOCKING)

### HIGH

**Builder bypasses format module's `compute_checksum` in favor of inline Hasher** - `builder.rs:217-220`
**Confidence**: 85%
- Problem: The `serialize` function in `builder.rs` constructs a `crc32fast::Hasher` manually with two `update` calls rather than using the `compute_checksum` function defined in `format.rs`. While the result is mathematically identical (CRC32 is compositional), this creates two independent implementations of the checksum computation. The `format.rs` module is explicitly designed as the single source of truth for codec operations ("No `std::fs` or `std::io::Write`" -- all encoding/decoding lives there), and the `compute_checksum` function exists precisely for this purpose. The reader already uses it. If the checksum algorithm ever changes (e.g., switching to xxHash), the builder's inline implementation would silently diverge.
- Fix: Replace the inline hasher in `builder.rs:serialize` with a call to `compute_checksum` from `format.rs`. Since `compute_checksum` takes a single `&[u8]`, concatenate `fc_buf` and `pair_buf` before calling it:
```rust
// In serialize(), replace lines 217-220:
let mut payload = Vec::with_capacity(fc_bytes + pair_bytes);
payload.extend_from_slice(&fc_buf);
payload.extend_from_slice(&pair_buf);
let checksum = compute_checksum(&payload);
```
Alternatively, add a `compute_checksum_multi(&[&[u8]]) -> u32` function to `format.rs` to avoid the extra allocation while keeping the single source of truth.

**`pairs_for_file` uses O(n) linear scan instead of leveraging sorted order** - `reader.rs:163-182`
**Confidence**: 82%
- Problem: `pairs_for_file` performs a linear scan over all pair entries (O(pair_count)) to find all partners for a given file. The pair entries are sorted by `(file_a, file_b)`, which means entries where `file_a == target_id` are contiguous and could be found via binary search in O(log n), then scanned forward. Entries where `file_b == target_id` are not contiguous (they are scattered), so a full scan is needed for those -- but the `file_a` matches alone can be optimized. For large matrices approaching the 2M pair cap, this linear scan could become a bottleneck when called repeatedly (e.g., during search result ranking).
- Fix: Consider splitting the lookup into two phases: (1) binary search to find the range of entries where `file_a == target_id` (contiguous in sorted order), then (2) maintain a secondary sorted structure or accept the linear scan for `file_b` matches. If this is a known trade-off accepted for simplicity in v1, document it with a comment explaining the expected scale and when optimization should be revisited.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`CochangeMatrixBuilder` does not implement `LayerBuilder` trait but lacks documentation of the architectural rationale** - `builder.rs:38-41`
**Confidence**: 83%
- Problem: The comment at line 38-39 says "Does NOT implement [`crate::LayerBuilder`] -- it takes a [`HistoryResult`] rather than raw file content." This is correct -- the `LayerBuilder` trait is designed for content-based indexing (`add_file(id, content, lang)`) while this builder operates on pre-parsed git history. However, this creates two divergent builder patterns in `rskim-search`: `NgramIndexBuilder` follows `LayerBuilder`, while `CochangeMatrixBuilder` has a completely different API shape (`build(&self, history, path_map)`). The existing `SearchLayer` trait is also not implemented because the cochange module is a lookup layer, not a query-result layer. While these deviations are justified, they should be formalized as a documented architectural pattern rather than ad-hoc departures.
- Fix: Add a doc comment at the module level (`mod.rs`) explaining the two-tier architecture: "rskim-search has two kinds of persistence layers: (1) content-based layers implementing `LayerBuilder`/`SearchLayer` (e.g., ngram index), and (2) metadata-based layers with their own builder/reader patterns (e.g., cochange matrix). The latter operate on pre-computed metadata rather than raw source content, so they cannot conform to the content-oriented `LayerBuilder` trait."

**Duplicated test helpers across `builder_tests.rs` and `reader_tests.rs`** - `builder_tests.rs:17-51`, `reader_tests.rs:19-53`
**Confidence**: 88%
- Problem: The `make_history` and `make_path_map` helper functions are copy-pasted verbatim between `builder_tests.rs` and `reader_tests.rs` (34 lines each). This violates DRY within the same module. If the `CommitInfo` or `HistoryResult` structure changes, both copies must be updated independently.
- Fix: Extract a shared `test_helpers.rs` file (or a `#[cfg(test)]` submodule) within the `cochange/` directory that exports these helpers. Both test files can then `use super::test_helpers::*`.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`lib.rs` module doc comment does not mention the `cochange` module** - `lib.rs:1-11`
**Confidence**: 90%
- Problem: The module-level doc comment in `lib.rs` lists the architecture overview ("Core types, index, ngram, temporal") but does not mention the new `cochange` module. While the module is added to the `pub mod` list and re-exports, the architectural documentation at the top of the file is now stale.
- Fix: Add a line to the architecture comment: `//! - The `cochange` module persists file co-change matrices for coupling-aware ranking.`

## Suggestions (Lower Confidence)

- **Consider adding `#[must_use]` to `CochangeMatrixBuilder::build`** - `builder.rs:74` (Confidence: 70%) -- The `build` method returns `Result<CochangeStats>` containing observability data that callers should inspect. Adding `#[must_use]` would catch callers who accidentally discard the stats. The existing codebase uses `#[must_use]` on functions with important return values per `CLAUDE.md` Rust guidelines.

- **`CochangeStats` could implement `Default`** - `types.rs:176-190` (Confidence: 65%) -- The struct has natural zero-valued defaults for all fields. Deriving `Default` would simplify the initial construction in `accumulate_pairs` (lines 166-173) where fields are set to 0 and then filled in later.

- **`read_array` in `format.rs` is duplicated from `index/format.rs`** - `format.rs:116-128` (Confidence: 75%) -- The generic `read_array<const N: usize>` function is an exact copy of the one in `index/format.rs:163-179`. Consider extracting to a shared utility within `rskim-search` to avoid the duplication.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 0 | - |
| Should Fix | - | 0 | 2 | - |
| Pre-existing | - | - | 1 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Assessment

The cochange module demonstrates strong architectural discipline that closely mirrors the established `index/` module pattern:

**Strengths:**
- Clean separation of concerns: `format.rs` (pure codec, no I/O), `builder.rs` (write path), `reader.rs` (read path) -- exactly matching the existing `index/` module structure.
- Proper error handling throughout -- all operations return `Result`, no panics outside tests.
- Safety caps (`COUPLING_MAX_FILES`, `MAX_PAIRS`) with checked arithmetic prevent unbounded resource consumption.
- Atomic writes via tempfile+persist prevent partial-write observation.
- Memory-mapped reader is correctly `Send + Sync` with documented SAFETY rationale.
- Binary format has integrity protection (magic bytes, version, CRC32 checksum, size validation).
- The decision to use Jaccard (symmetric) vs. the heatmap's confidence (asymmetric) is architecturally sound -- they serve different purposes.
- `CochangeStats` in `types.rs` (pure, no I/O) follows the module's established pattern.
- Dependencies (`memmap2`, `crc32fast`, `tempfile`) are already in the workspace -- no new dependency additions.

**Conditions for approval:**
1. Use `compute_checksum` from `format.rs` in the builder instead of inline `crc32fast::Hasher` to maintain single source of truth for the checksum algorithm.
2. Document the `pairs_for_file` O(n) scan as a known trade-off with a note on when to optimize, or implement the `file_a` binary search optimization.
