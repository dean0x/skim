# Complexity Review Report

**Branch**: feat-populate-search-result-line-range -> main
**Date**: 2026-05-23T11:28

## Issues in Your Changes (BLOCKING)

### HIGH

**Positional tuple variant with 3 fields harms readability** - `crates/rskim/src/cmd/search/snippet.rs:32`
**Confidence**: 85%
- Problem: `SnippetOutcome::Ok(u32, std::ops::Range<usize>, SnippetContext)` is a 3-field positional tuple. Each call site must destructure all three fields in the right order (e.g. `SnippetOutcome::Ok(ln, lr, ctx)`) and there is no compiler help if `u32` and `Range<usize>` are accidentally swapped in future changes since both start values are often small integers. The previous 2-field variant was borderline; adding a third field crosses into readability-risk territory.
- Fix: Convert to a named-field struct variant:
```rust
pub(super) enum SnippetOutcome {
    Ok {
        match_line: u32,
        line_range: std::ops::Range<usize>,
        context: SnippetContext,
    },
    Stale,
    Unavailable,
}
```
  This makes destructuring self-documenting at every call site:
```rust
SnippetOutcome::Ok { match_line, line_range, context } => { ... }
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Duplicate `byte_offset_to_line` implementations across crates** - `crates/rskim/src/cmd/search/snippet.rs:47`, `crates/rskim-search/src/types.rs:351`
**Confidence**: 92%
- Problem: There are now two near-identical implementations of `byte_offset_to_line`:
  - `snippet.rs:47` returns `u32` (the pre-existing CLI version)
  - `types.rs:351` returns `usize` (the new library version, re-exported from `rskim_search`)

  Both have identical logic (clamp offset, count newlines, add 1). The CLI version is still called directly on line 160 of `snippet.rs` for the `match_line` value, while the library version is called indirectly via `rskim_search::compute_line_range` on line 162. The duplication means two functions to maintain, two sets of tests covering the same behavior (7 tests in `snippet_tests.rs` plus 7 in `types.rs` tests), and a subtle type mismatch risk (`u32` truncation vs `usize`).

- Fix: Remove the CLI-local `byte_offset_to_line` in `snippet.rs` and use the library version everywhere, casting the return value where `u32` is needed:
```rust
let match_line = rskim_search::byte_offset_to_line(&content, match_positions[0].start) as u32;
```
  Then delete the 7 duplicate tests in `snippet_tests.rs` that test the same behavior already covered by the library tests.

## Pre-existing Issues (Not Blocking)

_No pre-existing complexity issues of CRITICAL severity found in the reviewed files._

## Suggestions (Lower Confidence)

- **`types.rs` file length at 1,101 lines** - `crates/rskim-search/src/types.rs` (Confidence: 65%) -- The file exceeds the 500-line warning threshold (1,101 lines including tests). The new utility functions and their ~70 lines of tests add to this. Consider extracting the line-range utilities into a separate `line_utils.rs` module to keep `types.rs` focused on type definitions.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 8/10
**Recommendation**: CHANGES_REQUESTED
