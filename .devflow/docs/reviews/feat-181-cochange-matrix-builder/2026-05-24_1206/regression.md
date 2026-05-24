# Regression Review Report

**Branch**: feat/181-cochange-matrix-builder -> main
**Date**: 2026-05-24T12:06

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

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED

## Detailed Analysis

### 1. Lost Functionality - NONE DETECTED

All previously exported symbols from `rskim-search` remain intact. The diff to `lib.rs` only adds:
- `pub mod cochange` (new module)
- `pub use cochange::{CochangeMatrixBuilder, CochangeMatrixReader}` (new exports)
- `CochangeStats` added to the existing `types` re-export block

The re-export line for `types::*` was reformatted (line-wrapped differently) but contains every symbol that was previously exported:
`CommitInfo, FieldClassifier, FileChangeInfo, FileId, HistoryResult, IndexStats, LayerBuilder, NodeInfo, Result, SearchError, SearchField, SearchLayer, SearchQuery, SearchResult, TemporalFlags, TemporalMetadata, TemporalSource, byte_offset_to_line, compute_line_range`.

No files were deleted. No CLI options changed. No API endpoints removed.

### 2. Broken Behavior - NONE DETECTED

- No existing function signatures were changed
- No existing return types were widened or narrowed
- No default values were modified
- No existing error handling paths were altered
- The `types.rs` change is purely additive (new `CochangeStats` struct appended after `SearchField`)

### 3. Intent vs Reality - VERIFIED CORRECT

The commit message claims:
- "43 new tests" -- Verified: 14 (builder) + 16 (reader) + 13 (format) = 43
- "COUPLING_MAX_FILES=50 cap" -- Verified: `const COUPLING_MAX_FILES: usize = 50` in `builder.rs:24`
- "MAX_PAIRS=2M safety limit" -- Verified: `const MAX_PAIRS: usize = 2_000_000` in `builder.rs:30`
- "CRC32 integrity" -- Verified: CRC32 computed over `file_commit_bytes ++ pair_bytes` in `builder.rs:217-220`, validated in `reader.rs:94-102`
- "atomic write" -- Verified: `tempfile::NamedTempFile` + `persist()` in `builder.rs:254-259`
- "Jaccard similarity" -- Verified: `count_ab / (count_a + count_b - count_ab)` with `u64` denominator in `reader.rs:147`
- "mmap-based reader" -- Verified: `memmap2::Mmap` in `reader.rs:67`
- "Send+Sync" -- Verified: compile-time assertion test in `reader_tests.rs:281-284`

PR description focus areas all verified:
- `HEADER_SIZE=18` -- Correct: 4+2+4+4+4 = 18
- `FILE_COMMIT_ENTRY_SIZE=8` -- Correct: 4+4 = 8
- `PAIR_ENTRY_SIZE=12` -- Correct: 4+4+4 = 12
- Encode/decode symmetry verified via roundtrip tests
- `COUPLING_MAX_FILES` uses `>` (not `>=`) -- exactly 50 files processed, 51+ skipped
- `MAX_PAIRS` checked before inserting new key (line 155), not after

### 4. Incomplete Migrations - NONE DETECTED

This is a purely additive module. No downstream consumers reference `cochange`, `CochangeMatrixBuilder`, `CochangeMatrixReader`, or `CochangeStats` yet. No migration needed.

### 5. Test Suite Verification

- `cargo test -p rskim-search`: 351 pass, 0 fail, 3 skip
- `cargo clippy -p rskim-search -- -D warnings`: 0 warnings
- `cargo fmt -p rskim-search -- --check`: clean

### 6. Dependency Changes

No `Cargo.toml` or `Cargo.lock` changes. All used dependencies (`crc32fast`, `memmap2`, `tempfile`) were already workspace dependencies for `rskim-search`.

### 7. Score Justification (9/10)

Deducted 1 point because `tempfile` is used as a non-dev dependency (it's used in production code for atomic writes via `NamedTempFile`), which is an appropriate use but means a test-utility crate ships in the production binary. This is a pre-existing pattern in the codebase, not a regression introduced by this PR. The change is purely additive, preserves all existing exports and behavior, and is well-tested.
