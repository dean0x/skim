# Performance Review Report

**Branch**: feat-populate-search-result-line-range -> main
**Date**: 2026-05-23

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Redundant O(n) scan: `byte_offset_to_line` called twice for the same first match position** - `crates/rskim/src/cmd/search/snippet.rs:160-162`
**Confidence**: 85%
- Problem: `byte_offset_to_line` is called at line 160 for `match_positions[0].start`, then `compute_line_range` is called at line 162 which internally calls `byte_offset_to_line` again for _every_ position including `match_positions[0].start`. This means the first match position's byte slice is scanned for newlines twice. Each call to `byte_offset_to_line` is O(offset) -- it counts newlines in `content[..offset]`.
- Impact: For a file with a match at byte offset 100,000 this is two 100KB scans instead of one. With the default limit of 20 results and files up to 5MB, this is measurable but not critical since file I/O dominates. The overhead is proportional to the byte offset of the first match position in each file.
- Fix: Derive `match_line` from the result of `compute_line_range` instead of calling `byte_offset_to_line` separately:
```rust
let line_range = rskim_search::compute_line_range(&content, match_positions);

// The first match position's line is already computed inside compute_line_range.
// For the primary match line we need the line of match_positions[0], which is
// always >= line_range.start. Re-derive it cheaply:
let match_line = byte_offset_to_line(&content, match_positions[0].start);
```
Alternatively, refactor `compute_line_range` to return the per-position lines alongside the range, eliminating the redundant scan entirely. Or compute `match_line` first and pass it into a variant that avoids re-scanning position 0:
```rust
let match_line = byte_offset_to_line(&content, match_positions[0].start);
// If match_positions.len() == 1, line_range is trivially match_line..(match_line+1)
let line_range = if match_positions.len() == 1 {
    (match_line as usize)..((match_line as usize) + 1)
} else {
    rskim_search::compute_line_range(&content, match_positions)
};
```

### LOW

**`compute_line_range` clones iterator to compute min and max separately** - `crates/rskim-search/src/types.rs:373-378`
**Confidence**: 82%
- Problem: The `lines` iterator is cloned to compute `min()` and `max()` separately. This means `byte_offset_to_line` is called `2 * N` times (where N is the number of match positions) instead of `N` times with a single-pass fold. Each call scans `content[..offset]` for newlines, so total work is `2 * sum(offsets)` bytes scanned instead of `sum(offsets)`.
- Impact: Match position counts are typically small (single digits per result), so the constant-factor doubling is unlikely to be measurable in practice. Noted for correctness of algorithmic analysis rather than real-world impact.
- Fix: Use a single-pass fold:
```rust
let (min_line, max_line) = match_positions
    .iter()
    .map(|pos| byte_offset_to_line(content, pos.start))
    .fold((usize::MAX, 0usize), |(mn, mx), line| (mn.min(line), mx.max(line)));

min_line..(max_line + 1)
```

## Issues in Code You Touched (Should Fix)

_No issues found._

## Pre-existing Issues (Not Blocking)

_No issues found._

## Suggestions (Lower Confidence)

- **Consider a precomputed line-offset index for repeated byte-to-line conversions** - `crates/rskim-search/src/types.rs:351` (Confidence: 65%) -- If `compute_line_range` is ever called on files with many match positions, building a line-offset table once via `memchr::memchr_iter(b'\n', content)` and using binary search per position would reduce from O(N * avg_offset) to O(content_len + N * log(lines)). Currently match counts are small enough that the linear scan is fine.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 1 |
| Should Fix | - | - | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Performance Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The changes add minimal overhead to the existing snippet-extraction path. The `byte_offset_to_line` function is O(offset) per call but operates on already-loaded file content with no additional I/O. The redundant scan of the first match position (MEDIUM finding) is a constant-factor inefficiency that is dwarfed by the file read that precedes it. The iterator clone (LOW finding) doubles the newline-counting work but with typically very few match positions per result, this is negligible in practice.

The 5MB file-size guard in `extract_snippet` bounds the worst case. With the default limit of 20 results, peak additional CPU work from these changes is bounded and proportional to content already in memory.

Condition: Consider the single-pass fold fix (LOW) as a low-effort improvement if this code path becomes hot in future profiling.
