# Regression Review Report

**Branch**: feat/178-two-file-mmap-index -> main
**Date**: 2026-05-14
**Diff**: `git diff e392dbc3efca8a144b28fb9432be964314955aa9...HEAD`

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`lib.rs` module-level doc comment contradicts new I/O behavior** - `crates/rskim-search/src/lib.rs:5`
**Confidence**: 85%
- Problem: The crate doc comment declares `**IMPORTANT: This is a LIBRARY with NO I/O.**` and `Pure types and traits, no side effects`. The newly added `index` module (builder.rs, reader.rs) performs significant filesystem I/O: creating temp files, persisting via rename, opening files, and memory-mapping them. The doc comment is now factually wrong and misleads consumers about the crate's nature.
- Fix: Update the crate-level doc comment to reflect the new reality. For example:
  ```rust
  //! Skim Search - Code search foundation library
  //!
  //! # Architecture
  //!
  //! - `types` module: pure types and traits with no I/O
  //! - `ngram` / `weights` modules: pure computation, no I/O
  //! - `index` module: on-disk index builder and mmap'd reader (performs file I/O)
  //!
  //! CLI/binary code in `crates/rskim/src/cmd/search.rs` handles user-facing I/O.
  ```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`tempfile` as runtime dependency** - `crates/rskim-search/Cargo.toml:23` (Confidence: 65%) -- `tempfile` is listed under `[dependencies]` rather than `[dev-dependencies]`, meaning it ships as a runtime dependency. This is technically correct since builder.rs uses `NamedTempFile` in production code for atomic writes, but it adds a non-trivial dependency to the library crate for a feature that could alternatively use `std::fs::rename` with manual temp file creation. Not a regression, but worth noting for dependency size awareness.

- **`is_multiple_of` nightly/edition dependency** - `crates/rskim-search/src/index/format.rs:298` (Confidence: 60%) -- The `is_multiple_of` method was stabilized in Rust 1.73.0 but the crate uses edition 2024 (requiring Rust 1.85+), so this is safe. However, if this crate were ever extracted for use with older toolchains, this would be a compatibility concern. Not a regression.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | - | - | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Detailed Analysis

### Public API Backward Compatibility: PASS

The changes to `SearchField` in `types.rs` are additive and backward compatible:

1. **`#[repr(u8)]` added** -- This does not change the serde serialization behavior. The existing `#[serde(rename_all = "snake_case")]` attribute continues to produce and parse the same JSON strings. Verified by the existing `test_search_field_serde_agrees_with_name` and `test_search_field_deserialization` tests, which both pass.

2. **Explicit discriminants `= 0` through `= 7` added** -- These match the default Rust discriminant assignment (0, 1, 2, ...), so no variant's discriminant value changed. The new `test_search_field_discriminant_roundtrip` test explicitly verifies this.

3. **New methods `discriminant()` and `from_discriminant()` added** -- Purely additive. No existing method was modified or removed.

4. **New `pub mod index` and `pub use` in `lib.rs`** -- Purely additive exports. No existing exports were removed or renamed.

### Existing Exports Preserved: PASS

All items from the pre-PR `pub use types::{...}` line are still exported: `FieldClassifier`, `FileId`, `IndexStats`, `LayerBuilder`, `NodeInfo`, `Result`, `SearchError`, `SearchField`, `SearchLayer`, `SearchQuery`, `SearchResult`, `TemporalFlags`. The ngram and weights re-exports are unchanged.

### Dependency Changes: PASS (no conflict risk)

Three new dependencies added to `rskim-search`:
- `memmap2 = "0.9"` (workspace) -- no overlap with existing deps
- `crc32fast = "1.4"` (workspace) -- no overlap with existing deps
- `tempfile = "3.0"` (workspace, already in workspace `[dev-dependencies]`, now also a runtime dep for the builder's atomic writes)

None of these conflict with existing workspace dependencies. The workspace already declared `tempfile` for dev use; promoting it to a runtime dependency is safe.

### Test Suite: PASS

- `rskim-search`: 114 pass, 0 fail, 1 skip (the skip is a release-only benchmark)
- `rskim` (binary crate): 3,069 pass, 0 fail
- All pre-existing tests continue to pass without modification
- New tests cover builder, reader, format, and lang_map modules comprehensively

### Commit Message vs. Implementation: ALIGNED

The PR description states "two-file format (index.skidx + index.skpost) with memory-mapped I/O, CRC32 integrity checking, and BM25 scoring over bigram posting lists." The implementation delivers all four components as described.

### Migration Completeness: N/A

This PR adds new code; it does not deprecate or migrate any existing API. No incomplete migration risk.
