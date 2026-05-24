# Architecture Review Report

**Branch**: feat-populate-search-result-line-range -> main
**Date**: 2026-05-23

## Issues in Your Changes (BLOCKING)

### HIGH

**Duplicated `byte_offset_to_line` creates two sources of truth with divergent return types** - `crates/rskim-search/src/types.rs:351`, `crates/rskim/src/cmd/search/snippet.rs:47`
**Confidence**: 90%
- Problem: The PR adds a new `pub fn byte_offset_to_line(content: &[u8], offset: usize) -> usize` to `rskim-search::types` while the existing `pub(super) fn byte_offset_to_line(content: &[u8], offset: usize) -> u32` in `snippet.rs` remains. Both implementations are algorithmically identical (count newlines, add 1, clamp offset), but they return different types (`usize` vs `u32`). The snippet.rs version uses `(newlines as u32).saturating_add(1)` while the types.rs version uses `newlines + 1`. This is two sources of truth for the same computation with a subtle type mismatch, violating SRP and DRY. In `extract_snippet` (line 160), the local `u32` version is called for `match_line`, while on the very next line (162) the library `compute_line_range` delegates to the `usize` version. For the same file content and same byte offset, these two functions now return values with different types.
- Fix: Delete the `snippet.rs` version and have `extract_snippet` call `rskim_search::byte_offset_to_line` instead. Cast the result to `u32` at the call site (or change the `match_line` type to `usize` throughout the snippet pipeline). This keeps a single canonical implementation in the library crate. Example:
  ```rust
  // snippet.rs, line 160 — replace local call with library call
  let match_line = rskim_search::byte_offset_to_line(&content, match_positions[0].start) as u32;
  ```
  Then remove `pub(super) fn byte_offset_to_line` from `snippet.rs` and its dedicated tests in `snippet_tests.rs` (the library version has its own test suite in `types.rs`).

### MEDIUM

**Indexing convention mismatch between `SearchResult.line_range` doc and `compute_line_range` output** - `crates/rskim-search/src/types.rs:332`, `crates/rskim-search/src/types.rs:364`
**Confidence**: 82%
- Problem: `SearchResult.line_range` is documented as "0-indexed, exclusive end" (line 332). The new `compute_line_range` docstring claims it returns values "matching the convention used by `SearchResult::line_range`" (line 364), but it actually produces 1-indexed line numbers (`byte_offset_to_line` returns 1-indexed). The reader's default placeholder is `0..0` which is consistent with 0-indexed, but `compute_line_range` would return `1..2` for a single-line match. The `ResolvedResult.line_range` in the CLI crate is `Option<Range<usize>>` and is correctly documented as 1-indexed (line 66-70 of `types.rs` in rskim), so the consumer is fine, but the library-level doc on `SearchResult.line_range` and the `compute_line_range` docstring contradict each other.
- Fix: Either update the `SearchResult.line_range` doc comment to say "1-indexed, exclusive end" (if that is the intended convention going forward), or update `compute_line_range`'s doc to accurately note it returns 1-indexed values that differ from the existing `SearchResult.line_range` convention. Given that the reader currently emits `0..0` as a placeholder and `compute_line_range` only populates `ResolvedResult.line_range` (not `SearchResult.line_range`), the simplest fix is correcting the `compute_line_range` docstring to remove the inaccurate cross-reference.

**`SnippetOutcome::Ok` tuple variant accumulating positional fields reduces readability** - `crates/rskim/src/cmd/search/snippet.rs:32`
**Confidence**: 80%
- Problem: `SnippetOutcome::Ok` now carries 3 positional fields: `(u32, Range<usize>, SnippetContext)`. Every match site must destructure all three and the positions are not self-documenting. The existing code already destructures as `(ln, lr, ctx)` with abbreviated names. Adding a fourth field in a future PR would make this increasingly fragile. This is a shallow module pattern -- the interface complexity is growing proportionally with each addition rather than being hidden behind a meaningful abstraction.
- Fix: Replace the tuple variant with a named struct variant or a dedicated struct:
  ```rust
  pub(super) struct SnippetMatch {
      pub match_line: u32,
      pub line_range: Range<usize>,
      pub context: SnippetContext,
  }

  pub(super) enum SnippetOutcome {
      Ok(SnippetMatch),
      Stale,
      Unavailable,
  }
  ```
  This makes destructuring self-documenting and makes future field additions non-breaking.

## Issues in Code You Touched (Should Fix)

_None identified._

## Pre-existing Issues (Not Blocking)

_None at CRITICAL severity._

## Suggestions (Lower Confidence)

- **`compute_line_range` has O(n*m) complexity for large match sets** - `crates/rskim-search/src/types.rs:373` (Confidence: 65%) -- Each match position triggers a linear scan of `content[..offset]` counting newlines. For files with many match positions, a single-pass line-offset table would be more efficient. Unlikely to matter at current scale (match_positions are typically small), but worth noting for future optimization.

- **Library-level `byte_offset_to_line` could live in a `util` module rather than `types.rs`** - `crates/rskim-search/src/types.rs:351` (Confidence: 62%) -- The `types.rs` module docstring says "This module contains pure types and traits with NO I/O." While `byte_offset_to_line` has no I/O, it is a utility function rather than a type or trait. Placing it in a dedicated `util` module within `rskim-search` would better match the module's stated contract.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 7/10
**Recommendation**: CHANGES_REQUESTED
