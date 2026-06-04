# Performance Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Unconditional 501-element Vec allocation in ast_extract::walk_tree** - `crates/rskim-research/src/ast_extract.rs:137`
**Confidence**: 85%
- Problem: `vec![None; ancestor_cap]` allocates a 501-element `Vec<Option<NodeKindId>>` on every call to `walk_tree`, regardless of actual tree depth. `NodeKindId` is likely a `u32` or similar, so each `Option<NodeKindId>` is 8 bytes. That is ~4 KiB per invocation. In the corpus extraction path (`extract_ast_ngrams_from_corpus`), this function is called once per file. For a corpus of thousands of files processed sequentially, this creates thousands of short-lived 4 KiB allocations that hit the allocator repeatedly.
- Fix: Pre-allocate and reuse a single `ancestors` buffer across files by threading it through `walk_tree` (or resetting via `fill(None)`), similar to how `vocab` is already threaded. Alternatively, size the initial allocation to a smaller default (e.g., 64) and grow on demand, since typical Rust/TS files rarely exceed depth 20-30. The 501-element upfront allocation is only needed to handle the pathological max_depth=500 case.

```rust
// Option A: Start small, grow on demand
let mut ancestors: Vec<Option<NodeKindId>> = vec![None; 64];
// ...in the loop:
if depth >= ancestors.len() {
    ancestors.resize(depth + 1, None);
}

// Option B: Reuse across files (preferred for corpus path)
fn walk_tree(
    tree: &tree_sitter::Tree,
    vocab: &mut NodeKindVocabulary,
    collect_trigrams: bool,
    result: &mut AstFileResult,
    ancestors: &mut Vec<Option<NodeKindId>>,  // reused buffer
) {
    ancestors.fill(None);  // O(n) but no allocation
    // ...
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**AstWalkIter::level_stack starts with zero capacity** - `crates/rskim-core/src/ast_walk.rs:118`
**Confidence**: 82%
- Problem: `level_stack: Vec::new()` starts with zero capacity. For every file processed, the `Vec<u32>` grows incrementally as the cursor descends. A typical tree-sitter parse tree for a 1000-line file has 15-30 levels of nesting, causing ~4-5 reallocations per traversal (0 -> 1 -> 2 -> 4 -> 8 -> 16 -> 32). With the `AstWalkIter` now shared across two hot paths (linearize and ast_extract), every call pays this reallocation cost.
- Fix: Pre-allocate `level_stack` to a reasonable depth estimate (e.g., 32 or 64) since max_depth is known at construction time. This eliminates reallocation for all but pathologically deep trees.

```rust
pub fn new(cursor: tree_sitter::TreeCursor<'a>, config: AstWalkConfig) -> Self {
    // Pre-allocate for typical tree depths. config.max_depth is the hard upper
    // bound, but real trees rarely exceed 30. Use min(64, max_depth) as a
    // balance between avoiding realloc and not over-allocating.
    let initial_cap = (config.max_depth as usize).min(64);
    Self {
        cursor,
        level_stack: Vec::with_capacity(initial_cap),
        // ...
    }
}
```

## Pre-existing Issues (Not Blocking)

### MEDIUM

**LANG_MAPS LazyLock initializes all 14 grammars eagerly** - `crates/rskim-search/src/ast_index/linearize.rs:109-174`
**Confidence**: 80%
- Problem: The first call to `linearize_source` for any language triggers `LazyLock` initialization of `LANG_MAPS`, which loads all 14 tree-sitter grammars, parses an empty string for each, and builds a vocabulary lookup table per grammar. This is a one-time cost but may be surprising in latency-sensitive contexts (e.g., a single-file indexing operation). The feature knowledge notes this: "LANG_MAPS is LazyLock -- first call per language triggers grammar loading" but actually the first call triggers loading for ALL languages.
- Fix: Not blocking. If single-file latency matters, consider per-language `OnceLock` entries instead of one monolithic `LazyLock<HashMap<...>>`. This would amortize initialization cost across the first use of each language rather than front-loading all 14 at once. The current approach is correct for batch indexing where all languages are used.

## Suggestions (Lower Confidence)

- **Redundant Parser::new in linearize_source after LANG_MAPS lookup** - `crates/rskim-search/src/ast_index/linearize.rs:212` (Confidence: 65%) -- `LANG_MAPS` initialization already calls `Parser::new` for each language. The subsequent `Parser::new(language)` in `linearize_source` creates a second parser instance. Prior resolution (Cycle 1) noted this was flagged as FP because the parsers serve different purposes (init vs. per-file parsing). Still, caching or reusing the parser could save the grammar-load overhead on each call.

- **Performance test threshold relaxed from 5ms to 10ms** - `crates/rskim-search/src/ast_index/linearize_tests.rs:445` (Confidence: 70%) -- The benchmark threshold was doubled from `< 5ms` to `< 10ms` while simultaneously increasing the input from 100 functions to 1000 functions. This is a 10x input increase with only 2x threshold increase, which is actually a tighter per-unit budget. However, relaxing the absolute threshold may mask future regressions. Consider adding Criterion benchmarks alongside the unit-test gate for continuous tracking.

- **`ancestors` bounds check on every iteration** - `crates/rskim-research/src/ast_extract.rs:156,174` (Confidence: 62%) -- The `if depth < ancestor_cap` check runs on every node. Since `AstWalkIter` guarantees `item.depth < max_depth` (which equals `ancestor_cap - 1`), the check is always true for non-error paths and could theoretically be elided. In practice the branch predictor handles this well, so the impact is negligible.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Performance Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The refactoring from duplicated hand-rolled `TreeCursor` DFS into a shared `AstWalkIter` is architecturally sound and introduces no performance regressions. The iterator pattern preserves the same O(n) traversal complexity and the same bounds guards (max_depth=500, max_nodes=100K). The vocabulary lookup remains O(1) via array indexing at traversal time (binary search only at init), consistent with documented feature knowledge patterns.

The two MEDIUM findings are allocation micro-optimizations: (1) the 501-element ancestor Vec in `ast_extract::walk_tree` is allocated per-file in the corpus path, and (2) the `level_stack` in `AstWalkIter` starts at zero capacity causing incremental reallocation. Neither is a correctness issue or a regression -- they are opportunities to reduce allocator pressure in the hot path. applies ADR-001 -- surfacing these for immediate consideration rather than deferring. avoids PF-002 -- all findings are presented for decision, none silently deferred.
