# Regression Review Report

**Branch**: main (commit 353ef87)
**Date**: 2026-05-24
**Focus**: Regression analysis of co-change matrix module addition

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none)

## Passed Checks

### 1. No Removed Exports
- **Verified**: All previously exported symbols from `lib.rs` remain intact.
- Previous exports: `NgramIndexBuilder`, `NgramIndexReader`, `BM25FConfig`, `FIELD_COUNT`, `MAX_QUERY_BYTES`, `QueryEngine`, `bm25f_score`, `classify_source`, `dominant_field`, `Ngram`, `extract_ngrams`, `extract_ngrams_with_weights`, `extract_query_ngrams`, `extract_query_ngrams_with_weights`, `GixSource`, `is_fix_commit`, `CommitInfo`, `FieldClassifier`, `FileChangeInfo`, `FileId`, `HistoryResult`, `IndexStats`, `LayerBuilder`, `NodeInfo`, `Result`, `SearchError`, `SearchField`, `SearchLayer`, `SearchQuery`, `SearchResult`, `TemporalFlags`, `TemporalMetadata`, `TemporalSource`, `byte_offset_to_line`, `compute_line_range`, `BIGRAM_WEIGHTS`, `DEFAULT_WEIGHT`, `bigram_weight`.
- All remain in the new `lib.rs` with identical visibility.

### 2. No Name Collisions
- **Verified**: `CochangeMatrixBuilder`, `CochangeMatrixReader`, and `CochangeStats` are new names not used anywhere else in the crate or downstream consumers.
- Grep across all `.rs` files in `crates/` confirms no prior usage of these identifiers.

### 3. No Changed Return Types or Signatures
- **Verified**: The `types.rs` change is purely additive -- only the new `CochangeStats` struct was appended after the `SearchField` section. No existing struct, enum, trait, or function signature was modified.

### 4. No Changed Module Visibility
- **Verified**: The existing `types` module remains `mod types;` (private, re-exported via `pub use`). The new `cochange` module is `pub mod cochange;` which is consistent with other public modules (`index`, `lexical`, `ngram`, `temporal`, `weights`).

### 5. Existing Tests Pass
- **Verified**: `cargo test -p rskim-search` reports 354 tests passing, 0 failures, 3 skipped (2 doc-test ignores + 1 integration ignore). All pre-existing tests remain green.

### 6. Downstream Crate Compiles
- **Verified**: `cargo check -p rskim` succeeds, confirming no compilation breakage in the main CLI binary that depends on `rskim-search`.

### 7. Re-export Order in `pub use types::{...}`
- **Verified**: The `CochangeStats` addition to the re-export block maintains alphabetical ordering (it comes before `CommitInfo`), consistent with the existing convention.

### 8. No New Dependencies Added
- **Verified**: `git diff 353ef87^..353ef87 -- '*.toml'` shows zero changes to any Cargo.toml file. The `memmap2`, `crc32fast`, and `tempfile` dependencies used by the new module were already declared in the workspace before this commit.

### 9. Commit Message Matches Implementation
- **Verified**: Commit message ("co-change matrix builder with Jaccard similarity and binary persistence") accurately describes the implementation: a builder that accumulates co-change pairs from git history and persists them in the `.skcc` binary format, with a reader that supports Jaccard similarity queries.

### 10. New Module Follows Crate Conventions
- **Verified**: The `cochange` module follows established patterns from the `index` module:
  - Builder/reader separation (`CochangeMatrixBuilder` / `CochangeMatrixReader`), mirroring `NgramIndexBuilder` / `NgramIndexReader`.
  - Binary format module (`format.rs`) with encode/decode functions, mirroring `index/format.rs`.
  - `Result` types throughout, no panics outside `#[cfg(test)]`.
  - `#[must_use]` annotations on key public methods.
  - Atomic file writes via `tempfile` + `persist`.
  - Memory-mapped reader via `memmap2`.
  - CRC32 integrity validation via `crc32fast`.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 10/10
**Recommendation**: APPROVED

This commit is purely additive. It introduces a new `cochange` module with three new public exports (`CochangeMatrixBuilder`, `CochangeMatrixReader`, `CochangeStats`) and makes no modifications to any existing types, traits, function signatures, or module visibility. All 354 existing tests pass. The downstream `rskim` crate compiles without issues. No dependencies were added or changed. No regression risk detected.
