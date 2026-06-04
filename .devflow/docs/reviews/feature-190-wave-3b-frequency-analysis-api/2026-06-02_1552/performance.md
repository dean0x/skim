# Performance Review Report

**Branch**: feature/190-wave-3b-frequency-analysis-api -> main
**Date**: 2026-06-02T15:52

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

- **`lang.name()` string dispatch in `ast_bigram_weight` / `ast_trigram_weight` hot path** - `crates/rskim-search/src/ast_weights.rs:105465` (Confidence: 65%) -- The `ast_bigram_idf` and `ast_trigram_idf` functions call `lang.name()` to get a `&'static str`, then the downstream `ast_bigram_weight` matches that string against 14 language names. A match-on-enum dispatch (accepting `Language` directly instead of `&str`) would avoid the string comparisons entirely. However, `ast_weights.rs` is auto-generated code outside this PR's scope, and the 14-arm string match with short static strings is negligible compared to the binary search that follows. This only matters if IDF lookup is called millions of times in a tight loop.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 9/10
**Recommendation**: APPROVED

## Analysis Notes

### What was reviewed

- `crates/rskim-search/src/ast_index/ngram.rs` (256 lines, new file) -- core production module
- `crates/rskim-search/src/ast_index/ngram_tests.rs` (450 lines, new file) -- test suite
- `crates/rskim-search/src/ast_index/mod.rs` (re-export changes)
- `crates/rskim-search/src/lib.rs` (re-export changes)
- `crates/rskim-search/benches/linearize_bench.rs` (formatting changes only)
- `crates/rskim-search/src/ast_index/linearize.rs` (formatting changes only)
- `crates/rskim-search/src/ast_index/linearize_tests.rs` (formatting changes only)
- `crates/rskim-core/src/ast_walk.rs` (formatting changes only)
- `crates/rskim-research/src/ast_extract.rs` (formatting changes only)
- Upstream `ast_bigram_weight` / `ast_trigram_weight` in `ast_weights.rs` (called by new code)

### Performance design assessment

This PR introduces a well-designed performance-oriented API with the following strengths:

1. **Zero-allocation newtypes**: `AstBigram` (u32) and `AstTrigram` (u64) are `#[repr(transparent)]`, `Copy`, and use only bit operations (`<<`, `|`, `&`, `>>`) for encode/decode. No heap allocation, no boxing. The `#[inline]` annotations ensure these compile down to a few shift/mask instructions.

2. **O(log n) weight lookup with zero transformation**: `ast_bigram_idf` passes `bigram.key()` (the raw u32) directly to `ast_bigram_weight`, which performs a single `binary_search_by_key` on a sorted `&[(u32, f32)]` table. The encoding deliberately matches the stored weight table format, so there is no re-encoding step between the caller and the lookup. For the Rust table (~1740 entries), this is approximately 11 comparisons on integer keys.

3. **O(log n) vocabulary lookup**: `vocab_lookup` uses `binary_search` on the 1740-entry `NODE_KIND_VOCABULARY` sorted string table. This is a one-time cost at query construction time (not per-node during traversal), which is the correct architectural placement.

4. **O(1) vocabulary resolve**: `vocab_resolve` is a direct array index into `NODE_KIND_VOCABULARY` -- no search required.

5. **`Display` implementation is diagnostic-only**: The `fmt_kind_id` helper calls `vocab_resolve` (O(1) array index) and is only triggered when formatting for debugging/display, not in the search hot path.

6. **No new allocations in hot paths**: The entire encode -> key -> binary_search -> f32 pipeline involves zero heap allocations. All data is stack-resident or static.

7. **`DEFAULT_AST_WEIGHT` fallback is branchless-friendly**: The `unwrap_or(DEFAULT_AST_WEIGHT)` pattern compiles to a simple conditional move, not an allocation or error path.

### Feature knowledge alignment

The implementation matches the documented performance characteristics in the ast-index feature knowledge: LANG_MAPS uses LazyLock (one-time init, not triggered by this code), vocabulary lookup is O(1) array index during traversal (binary search only at init/query time), AstBigram/AstTrigram are Copy newtypes with pure bit operations, and IDF lookup uses binary search on sorted weight tables.

### Cross-cycle note

Cycle 1 identified 6 fixed issues and 3 false positives. None of those findings were performance-related. This cycle finds no new performance issues -- the code is well-optimized by design.
