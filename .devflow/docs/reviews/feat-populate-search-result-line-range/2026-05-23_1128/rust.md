# Rust Review Report

**Branch**: feat-populate-search-result-line-range -> main
**Date**: 2026-05-23

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Duplicate `byte_offset_to_line` implementations with divergent return types** - `crates/rskim-search/src/types.rs:351`, `crates/rskim/src/cmd/search/snippet.rs:47`
**Confidence**: 90%
- Problem: Two `byte_offset_to_line` functions now coexist with identical logic but different return types: `pub fn byte_offset_to_line(...) -> usize` in `rskim-search/types.rs` and `pub(super) fn byte_offset_to_line(...) -> u32` in `snippet.rs`. The `snippet.rs` version pre-dates this PR, but the new public version was added as part of this change. Having two copies of the same algorithm with different return types (one using `saturating_add`, one using `+ 1`) creates a maintenance risk: a bug fix in one will not propagate to the other.
- Fix: Replace the private `snippet.rs` version with a call to `rskim_search::byte_offset_to_line` (which is now public and re-exported), casting the result: `let match_line = rskim_search::byte_offset_to_line(&content, match_positions[0].start) as u32;`. This eliminates the duplication. Alternatively, if `u32` is truly needed for `extract_context_window`, add a thin wrapper that delegates to the library function.

**Documentation inconsistency: `compute_line_range` claims to match `SearchResult::line_range` convention** - `crates/rskim-search/src/types.rs:364`
**Confidence**: 92%
- Problem: The doc comment on `compute_line_range` at line 364 states it returns values "matching the convention used by `SearchResult::line_range`". However, `SearchResult::line_range` at line 331 is documented as "0-indexed, exclusive end", while `compute_line_range` returns 1-indexed values. This is misleading. The `SearchResult::line_range` doc is arguably stale (the field is currently populated with placeholder `0..0` from the index reader, so neither convention is in actual use there), but the cross-reference in the new code is actively wrong.
- Fix: Either (a) update the `SearchResult::line_range` doc comment to say "1-indexed, exclusive end" to match the new reality, or (b) remove the cross-reference from `compute_line_range`'s doc comment. Option (a) is preferred since this PR is establishing the convention for the first time. Update line 331 from `/// Source lines spanned by this match (0-indexed, exclusive end)` to `/// Source lines spanned by this match (1-indexed, exclusive end)`.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`SnippetOutcome::Ok` tuple has 3 positional fields -- consider a named struct** - `crates/rskim/src/cmd/search/snippet.rs:32`
**Confidence**: 80%
- Problem: `SnippetOutcome::Ok(u32, std::ops::Range<usize>, SnippetContext)` now carries three positional fields. The destructuring sites (in `query.rs:104` and `snippet_tests.rs:181,205`) use opaque variable names like `ln`, `lr`, `ctx` that require context to understand. Adding a fourth field in the future would make this worse. This was already marginal with two fields; three pushes it past the readability threshold.
- Fix: Introduce a named struct variant:
  ```rust
  pub(super) struct SnippetMatch {
      pub match_line: u32,
      pub line_range: Range<usize>,
      pub context: SnippetContext,
  }
  enum SnippetOutcome {
      Ok(SnippetMatch),
      Stale,
      Unavailable,
  }
  ```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`byte_offset_to_line` is O(n) per call; `compute_line_range` calls it per match position** - `crates/rskim-search/src/types.rs:373` (Confidence: 65%) -- For a large file with many match positions, the repeated linear scan of `content[..offset]` could be optimized with a single pass building a line-start offset table. However, match_positions vectors are typically small (under 20 entries) and files are capped at 5 MB, so this is unlikely to matter in practice.

- **`compute_line_range` returns `0..0` for empty input -- potential sentinel confusion** - `crates/rskim-search/src/types.rs:370` (Confidence: 62%) -- Returning `0..0` for empty match_positions uses `0` as a sentinel in a 1-indexed system. The caller in `snippet.rs` returns `Unavailable` before reaching `compute_line_range` when positions are empty, so this path is dead in practice. But an `Option<Range<usize>>` return would make the "no positions" case explicit if the function is used elsewhere in the future.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The core logic is correct and well-tested. The `byte_offset_to_line` and `compute_line_range` functions are straightforward, use `#[must_use]`, accept borrowed slices (not owned types), and clamp inputs to avoid panics. The new tests cover edge cases (empty content, out-of-bounds offsets, single line, multi-line spans). The two blocking MEDIUM issues are: (1) the duplicate `byte_offset_to_line` with divergent return types creating a maintenance hazard, and (2) the doc comment on `compute_line_range` falsely claiming it matches the `SearchResult::line_range` convention (which says 0-indexed). Both are quick fixes that should be addressed before merge.
