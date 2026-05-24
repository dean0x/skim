# Complexity Review Report

**Branch**: feat/177-sparse-ngram-algorithm -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Covering-set loop has O(n^2) early-exit check** - `ngram.rs:277`
**Confidence**: 85%
- Problem: Inside the greedy covering-set loop (lines 271-280), every iteration calls `covered.iter().all(|&c| c)` to check if all positions are covered. This is O(n) per candidate, making the overall covering-set phase O(n^2) where n is the query byte length. For typical short queries this is negligible, but the function accepts arbitrary `&str` with no length bound.
- Fix: Track an `uncovered_count` counter, decrementing it when a position transitions from false to true, and break when it reaches zero:
```rust
let mut uncovered_count = bytes.len();
// ...
for (ngram, w, pos) in candidates {
    if !covered[pos] || !covered[pos + 1] {
        if !covered[pos] { covered[pos] = true; uncovered_count -= 1; }
        if !covered[pos + 1] { covered[pos + 1] = true; uncovered_count -= 1; }
        selected.push((ngram, w));
    }
    if uncovered_count == 0 {
        break;
    }
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`is_border_bigram` linear scan over border ranges** - `ngram.rs:154-158` (Confidence: 65%) -- The function does a linear scan over all border ranges for every candidate bigram position inside `extract_query_ngrams_with_weights`. For typical queries this is fine (small range count), but if queries grow long with many tokens the scan compounds. A bitset marking border positions would be O(1) per lookup. Low priority given expected query sizes.

- **`extract_ngrams_with_weights` max-weight dedup is a no-op for identical lookups** - `ngram.rs:191-196` (Confidence: 70%) -- When the same bigram key appears at multiple positions, `lookup_weight` returns the same value each time (the weight table is static per key), so `entry.max(w)` always produces the same result. The max-dedup logic is semantically correct and self-documenting (it protects against future weight-per-position changes), but the repeated binary searches for already-seen keys could be avoided by checking map containment first. Minor -- the current approach is clear and the cost is only O(log k) per duplicate.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

This is a well-structured module with low complexity throughout:

- **File length**: `ngram.rs` is 300 lines (well under the 500-line critical threshold), with tests extracted to a separate 391-line file. Clean separation.
- **Function length**: The longest function (`extract_query_ngrams_with_weights`) is ~52 lines including doc comments. The actual logic body is ~35 lines. All other functions are under 20 lines.
- **Cyclomatic complexity**: All functions have low branching. `token_border_ranges` has the highest at ~5 (two nested conditions), well within the "good" range. No function exceeds 10.
- **Nesting depth**: Maximum nesting is 3 levels (the `while`/`if`/`if` in `token_border_ranges`). Within the "good" threshold.
- **Parameter counts**: All public functions take 1-2 parameters. Private helpers take 2. Well within limits.
- **Magic values**: No magic numbers -- `BORDER_MULTIPLIER` is a named constant with documentation. The `256` capacity hint in `extract_ngrams_with_weights` is the only unnamed value, but it is a reasonable heuristic and self-explanatory in context.
- **Readability**: Excellent module-level and function-level documentation. Clear section separators. Code reads linearly with no surprising control flow.
- **Loop bounds**: All loops are bounded by input length (byte windows, candidate iteration). No unbounded iteration.

The single MEDIUM finding (O(n^2) early-exit check) is a minor algorithmic inefficiency that does not affect correctness and is unlikely to matter for realistic query sizes. Recommend fixing it opportunistically for cleanliness.
