# Reliability Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01
**Prior Resolutions**: Cycle 2 resolved 9/11 issues (0 FP, 0 deferred). Centralized bounds constants, prealloc level_stack, fused iterator, strengthened error test, lazy-grow ancestors, chain-break test. This cycle focuses on any remaining or newly introduced reliability concerns.

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`skip_subtree` loop has implicit bound via finite tree size** - `crates/rskim-core/src/ast_walk.rs:162-178` (Confidence: 65%) -- The `loop` in `skip_subtree` terminates because every `goto_parent()` pops one element from `level_stack`, which is finite. Similarly the `advance` inner loop at line 191-206 has the same structure. Both terminate by construction (tree has finite depth, stack drains to empty). However, neither has an explicit iteration bound counter, which deviates from the strict "Power of Ten" rule. In practice, the stack size is capped by `max_depth` (500), making unbounded iteration impossible. The implicit bound is sound but an explicit `for _ in 0..self.level_stack.len() + 1` would be more defensive. Low priority given the structural guarantee.

- **`linearize_tree` pre-allocation uses `descendant_count()` which may overcount** - `crates/rskim-search/src/ast_index/linearize.rs:238-242` (Confidence: 62%) -- `descendant_count()` returns the count for the entire tree, but `AstWalkIter` may yield fewer nodes due to `max_nodes` or `max_depth` guards. The `.min(DEFAULT_MAX_NODES as usize)` cap prevents unbounded allocation, so this is a minor over-allocation concern, not a reliability risk. The `Vec` will simply have unused capacity, bounded at 100,000 entries (~400 KiB for `LinearNode`). Acceptable tradeoff.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 9/10
**Recommendation**: APPROVED

## Detailed Analysis

### Bounded Iteration (Power of Ten Rule 1)

All traversal loops terminate by construction:

1. **`AstWalkIter::next()`** (ast_walk.rs:212-250): The main iterator advances through a finite tree. `max_depth` (500) and `max_nodes` (100,000) provide hard upper bounds on how many nodes can be yielded. The inner `loop` at line 226 runs at most once per skip (each iteration either yields or terminates). The `done` flag ensures post-exhaustion calls immediately return `None` (applies ADR-001 -- no deferred fix needed, FusedIterator is correctly implemented).

2. **`AstWalkIter::skip_subtree()`** (ast_walk.rs:161-178): Ascent loop bounded by `level_stack` depth (max 500 entries from `max_depth`). Each iteration either finds a sibling (returns `true`) or pops the stack. Stack drains to empty and returns `false`.

3. **`AstWalkIter::advance()`** (ast_walk.rs:184-206): Same stack-draining pattern as `skip_subtree`. Bounded by same `level_stack` depth.

4. **`linearize_tree` for-loop** (linearize.rs:246): Iterates over `AstWalkIter` which is bounded by `max_nodes`.

5. **`walk_tree` in ast_extract.rs** (ast_extract.rs:141): Same `AstWalkIter` bounds apply. Additional `MAX_TRIGRAMS_PER_FILE` (50,000) cap on trigram output.

### Assertion Density (Power of Ten Rule 2)

Strong precondition and invariant coverage:

- `LinearizeResult` documents and enforces invariant `node_count == nodes.len() + error_count` (linearize.rs:79, tested via `assert_node_count_invariant` in 11+ tests).
- `AstWalkIter` documents and tests invariant `node_count() == non_error_yields + error_count()` (ast_walk.rs:33, tested in `node_count_invariant_holds`).
- `MAX_FILE_SIZE` guard (100 KiB) prevents pathological inputs at entry (linearize.rs:199, ast_extract.rs:76).
- Zero-limit edge cases explicitly documented and tested (`zero_max_depth_yields_nothing`, `zero_max_nodes_yields_nothing`).

### Allocation Discipline (Power of Ten Rule 3)

- `level_stack` pre-allocated with `min(max_depth, 64)` capacity (ast_walk.rs:132) -- avoids repeated allocation for typical trees while capping initial allocation.
- `ancestors` table in `ast_extract.rs` starts at 64 entries and grows lazily via `resize` (ast_extract.rs:137, 148-149) -- avoids 501-entry upfront allocation for typical 20-30 depth trees.
- `linearize_tree` pre-sizes `nodes` Vec to `min(descendant_count, DEFAULT_MAX_NODES)` (linearize.rs:238-242) -- single allocation for the common case.
- `LANG_MAPS` initialized once via `LazyLock` (linearize.rs:108) -- no per-call allocation for vocabulary lookup.

### Indirection Limits (Power of Ten Rule 4)

No excessive indirection. Types are flat:
- `LinearNode { kind_id: u16, depth: u16 }` -- Copy, no heap allocation.
- `AstWalkNode` borrows the tree-sitter `Node` directly, no wrapping.
- `AstWalkConfig` is `Copy` with two `u32` fields.

### Metaprogramming Restraint (Power of Ten Rule 5)

No macros, no recursive generics, no trait objects in hot paths. The `LazyLock` is the most complex construct, and it is a well-understood stdlib type.

### Saturating Arithmetic

All counter increments use `saturating_add` (ast_walk.rs:187, 239, 241; ast_extract.rs:231, 235, 253, 254), preventing overflow on u32 counters. Depth uses `saturating_add(1)` for the same reason. The `u32 -> u16` depth cast in linearize.rs:257 uses `.min(u32::from(u16::MAX))` before casting, which is the correct saturating pattern (avoids PF-002 -- no pre-existing issue deferred).

### Error Handling

- `linearize_source` returns `Result<LinearizeResult>` -- only grammar load failures produce `Err`; file-level issues produce `Ok(default)`.
- `extract_ast_ngrams_from_file` returns `anyhow::Result` with the same pattern.
- Parse failures are handled gracefully (empty results), not panics.
- `SearchError::Ast` variant added for grammar-level failures, distinct from parse errors.

### Cross-Cycle Awareness

All 9 fixes from Cycle 2 remain intact and are confirmed in the current code:
- Centralized constants at `AstWalkConfig::DEFAULT_MAX_DEPTH` / `DEFAULT_MAX_NODES` (ast_walk.rs:72-78)
- Pre-allocated `level_stack` with `min(max_depth, 64)` (ast_walk.rs:132)
- `FusedIterator` impl (ast_walk.rs:257)
- Strengthened error test patterns throughout
- `#[must_use]` on `node_count()` and `error_count()` (ast_walk.rs:145, 152)
- Lazy-grow ancestor vec (ast_extract.rs:137, 148-149)
- Chain-break test (ast_extract.rs:493-541)
- Re-exports in rskim-core lib.rs (lib.rs:45)
- Test-only constant aliases (linearize.rs:39-44)

No regressions detected from prior cycle fixes.
