# Regression Review Report

**Branch**: feat/populate-search-result-line-range -> main
**Date**: 2026-05-23T11:28
**PR**: #249

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Duplicate `byte_offset_to_line` functions with divergent return types** - `crates/rskim-search/src/types.rs:351`, `crates/rskim/src/cmd/search/snippet.rs:47`
**Confidence**: 82%
- Problem: The PR introduces a new public `byte_offset_to_line` in `rskim_search::types` returning `usize`, while the pre-existing private `byte_offset_to_line` in `snippet.rs` returns `u32`. Both functions have identical logic (count newlines before the offset, add 1). The `snippet.rs` version continues to be used for `match_line` (the primary line number), while the library version is used indirectly via `compute_line_range`. If the two implementations ever diverge (e.g., one gets a bug fix the other doesn't), `line_number` and `line_range` could report inconsistent results for the same match positions. This is a latent regression vector -- not a regression today, but a fragile setup.
- Fix: Call the library function from snippet.rs and cast the result to `u32`:
  ```rust
  // snippet.rs - replace the local byte_offset_to_line with:
  let match_line = rskim_search::byte_offset_to_line(&content, match_positions[0].start) as u32;
  ```
  Then remove the local `byte_offset_to_line` function from `snippet.rs`.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

**`SearchResult::line_range` doc comment says "0-indexed" but `compute_line_range` returns 1-indexed** - `crates/rskim-search/src/types.rs:331`
**Confidence**: 90%
- Problem: The doc comment on `SearchResult::line_range` reads "Source lines spanned by this match (0-indexed, exclusive end)" but the new `compute_line_range` function explicitly returns 1-indexed line numbers (`newlines + 1`). The `NgramIndexReader` currently sets `line_range: 0..0` as a placeholder, and the new `ResolvedResult::line_range` field is populated via `compute_line_range` which is 1-indexed. The doc comment on `SearchResult::line_range` is stale -- it describes a convention that was never actually used (the field was always `0..0` before). This is not a functional regression from this PR since the `SearchResult::line_range` field itself was not changed, but the doc comment will mislead anyone who reads it in conjunction with the new `compute_line_range` docs.
- Fix: Update the doc comment to reflect the actual convention:
  ```rust
  /// Source lines spanned by this match (1-indexed, exclusive end)
  pub line_range: Range<usize>,
  ```

## Suggestions (Lower Confidence)

- **Tuple variant `SnippetOutcome::Ok` growing unwieldy** - `crates/rskim/src/cmd/search/snippet.rs:32` (Confidence: 65%) -- The `Ok` variant now carries 3 positional fields `(u32, Range<usize>, SnippetContext)`. Positional tuple fields are harder to read at match sites than a named struct variant. If another field is added, this will be difficult to maintain.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Regression Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The PR achieves its stated goal cleanly: it populates `ResolvedResult.line_range` with real 1-indexed line numbers computed from match positions. All existing tests pass, all consumers of `SnippetOutcome` and `ResolvedResult` are updated, and no exports or public APIs are removed. The migration is complete across the codebase.

The one blocking condition is the duplicate `byte_offset_to_line` implementations (MEDIUM). While both produce correct results today, maintaining two copies of the same logic with different return types is a latent regression vector. Consolidating to the library version with a cast eliminates this risk.
