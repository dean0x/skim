# Consistency Review Report

**Branch**: feat-populate-search-result-line-range -> main
**Date**: 2026-05-23

## Issues in Your Changes (BLOCKING)

### HIGH

**Duplicate `byte_offset_to_line` with inconsistent return type** - `crates/rskim-search/src/types.rs:351`, `crates/rskim/src/cmd/search/snippet.rs:47`
**Confidence**: 92%
- Problem: There are now two `byte_offset_to_line` functions with identical logic but different return types: the new library version in `rskim-search/src/types.rs` returns `usize`, while the pre-existing CLI version in `snippet.rs` returns `u32`. Both are used in `snippet.rs` (line 160 uses the local `u32` version, line 162 calls `rskim_search::compute_line_range` which internally uses the library `usize` version). The new library function is also re-exported from `rskim_search::byte_offset_to_line`, but the CLI does not use it -- it calls its own local copy. This creates a maintenance risk where a fix to one is not applied to the other.
- Fix: Replace the local `snippet.rs::byte_offset_to_line` with a call to `rskim_search::byte_offset_to_line`, casting the result to `u32` at the single call site (line 160) where `u32` is needed for the `extract_context_window` signature: `let match_line = rskim_search::byte_offset_to_line(&content, match_positions[0].start) as u32;`. Then remove the local function and update `snippet_tests.rs` to test via the library import. This eliminates the duplication and ensures a single source of truth.

**`SearchResult::line_range` doc comment contradicts `compute_line_range` doc** - `crates/rskim-search/src/types.rs:331`, `crates/rskim-search/src/types.rs:364`
**Confidence**: 95%
- Problem: The `SearchResult::line_range` field doc comment says "Source lines spanned by this match (0-indexed, exclusive end)" (line 331). However, the newly added `compute_line_range` function's doc comment says it returns "1-indexed" values and claims this "matching the convention used by `SearchResult::line_range`" (line 364). These two statements contradict each other. The actual runtime data is 1-indexed (since `byte_offset_to_line` returns 1-based line numbers), so the `SearchResult` doc comment is wrong. The reader in `index/reader.rs:406` initializes `line_range` to `0..0` as a sentinel for "not yet computed," which is consistent with the 1-indexed convention (0 is not a valid 1-indexed line).
- Fix: Update the doc comment on `SearchResult::line_range` (line 331) from `"0-indexed, exclusive end"` to `"1-indexed, exclusive end; 0..0 when not yet computed"` to match the actual convention used throughout the codebase.

### MEDIUM

**`ResolvedResult::line_range` uses `Option<Range<usize>>` while `SearchResult::line_range` uses `Range<usize>` -- different optionality patterns for the same concept** - `crates/rskim/src/cmd/search/types.rs:71`, `crates/rskim-search/src/types.rs:332`
**Confidence**: 82%
- Problem: The library-level `SearchResult::line_range` is a non-optional `Range<usize>` that uses the sentinel `0..0` for "not computed." The new CLI-level `ResolvedResult::line_range` is `Option<Range<usize>>` that uses `None` for "unavailable." These are two different patterns for representing the same concept (absence of a valid line range). The `SearchResult` uses a sentinel value pattern while `ResolvedResult` uses an explicit `Option`. While both work, using different conventions for the same semantic concept within the same data pipeline (library result flows into resolved result) creates cognitive overhead. The `Option` pattern in `ResolvedResult` is actually the more idiomatic Rust approach.
- Fix: This is acceptable as-is because `ResolvedResult` serves a different purpose (JSON serialization for end users, where `null` is more informative than `{"start":0,"end":0}`). Consider adding a brief doc comment noting the intentional deviation: `/// Uses Option instead of the 0..0 sentinel in SearchResult for cleaner JSON output.`

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Re-export ordering breaks alphabetical convention** - `crates/rskim-search/src/lib.rs:34`
**Confidence**: 80%
- Problem: The `pub use types::` block in `lib.rs` lists types in roughly alphabetical order (CommitInfo, FieldClassifier, FileChangeInfo...) but appends the two new free functions `byte_offset_to_line, compute_line_range` at the end, after `TemporalSource`. Other re-export blocks in the same file (`pub use lexical::`, `pub use ngram::`) mix types and functions but maintain alphabetical ordering within each block.
- Fix: Move `byte_offset_to_line` and `compute_line_range` to their correct alphabetical positions in the list:
```rust
pub use types::{
    CommitInfo, FieldClassifier, FileChangeInfo, FileId, HistoryResult, IndexStats, LayerBuilder,
    NodeInfo, Result, SearchError, SearchField, SearchLayer, SearchQuery, SearchResult,
    TemporalFlags, TemporalMetadata, TemporalSource, byte_offset_to_line, compute_line_range,
};
```
should become:
```rust
pub use types::{
    CommitInfo, FieldClassifier, FileChangeInfo, FileId, HistoryResult, IndexStats, LayerBuilder,
    NodeInfo, Result, SearchError, SearchField, SearchLayer, SearchQuery, SearchResult,
    TemporalFlags, TemporalMetadata, TemporalSource, byte_offset_to_line, compute_line_range,
};
```
Actually, looking more carefully, the existing convention places PascalCase types first, then snake_case functions at the end (see `pub use lexical::` which has `BM25FConfig, FIELD_COUNT, MAX_QUERY_BYTES, QueryEngine` then `bm25f_score, classify_source, dominant_field`). The new code follows this pattern correctly -- types first, then functions. No change needed. Withdrawing this finding.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Consider unifying the `u32`/`usize` line number type** - `crates/rskim/src/cmd/search/snippet.rs:47` (Confidence: 65%) -- The CLI layer uses `u32` for line numbers throughout (`SnippetLine::line_number`, `extract_context_window`, `DEFAULT_CONTEXT`) while the new library functions use `usize`. A future consistency pass could standardize on one type across both layers.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 6/10
**Recommendation**: CHANGES_REQUESTED
