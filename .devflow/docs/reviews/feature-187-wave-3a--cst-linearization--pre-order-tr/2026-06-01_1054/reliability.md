# Reliability Review Report

**Branch**: feature-187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Counter increments use raw `+=` instead of `saturating_add`** - `linearize.rs:271`, `linearize.rs:274`
**Confidence**: 82%
- Problem: `result.node_count += 1` and `result.error_count += 1` use raw addition on `u32` counters. The PR description states "saturating_add for counters" but only `depth` (line 297) actually uses `saturating_add`. While overflow is impossible in practice because `MAX_AST_NODES = 100_000` caps traversal well below `u32::MAX`, the inconsistency between documentation/intent and implementation creates a reliability gap. In debug builds, `u32` overflow panics; in release builds it wraps silently. The `depth: u16` counter correctly uses `saturating_add(1)` on the same principle — counters should be consistent. (applies ADR-001)
- Fix: Replace both raw `+=` with `saturating_add`:
  ```rust
  // line 271
  result.node_count = result.node_count.saturating_add(1);
  // line 274
  result.error_count = result.error_count.saturating_add(1);
  ```

### LOW

**`level_stack` is not pre-allocated with capacity** - `linearize.rs:248`
**Confidence**: 80%
- Problem: `let mut level_stack: Vec<u16> = Vec::new()` starts with zero capacity. The maximum depth is bounded by `MAX_AST_DEPTH = 500`, so the stack will grow through several reallocations for deeper trees. While the total memory impact is negligible (500 * 2 bytes = 1 KiB), the `nodes` Vec at line 240 is carefully pre-allocated with `with_capacity(capacity)`, making the omission for `level_stack` an inconsistency in allocation discipline.
- Fix:
  ```rust
  let mut level_stack: Vec<u16> = Vec::with_capacity(MAX_AST_DEPTH as usize);
  ```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Missing `debug_assert!` for node_count invariant in hot path** - `linearize.rs:312` (Confidence: 65%) — The documented invariant `node_count == nodes.len() + error_count` is only asserted in tests via `assert_node_count_invariant()`. A `debug_assert!` at the end of `linearize_tree` (before `Ok(result)` on lines 261 and 306) would catch invariant violations during development without release-build cost.

- **Outer `loop` has no explicit upper bound** - `linearize.rs:251` (Confidence: 62%) — The outer `loop` in `linearize_tree` terminates because tree-sitter's `TreeCursor` exhausts all nodes in a finite tree, and the `level_stack` eventually empties. However, the loop relies on implicit bounds (cursor exhaustion) rather than an explicit iteration counter. A `debug_assert!` checking iterations against a generous upper bound (e.g., `2 * MAX_AST_NODES`) would provide defense-in-depth against unforeseen cursor bugs, consistent with the project's reliability philosophy.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 1 |
| Should Fix | - | 0 | 0 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

This is a well-designed CST linearization module with strong reliability fundamentals:

- **Bounded iteration**: `MAX_AST_DEPTH` (500) and `MAX_AST_NODES` (100K) cap traversal effectively. `MAX_FILE_SIZE` (100 KiB) provides an input-level guard.
- **Allocation discipline**: `nodes` Vec is pre-allocated with `min(descendant_count, MAX_AST_NODES)`, preventing unbounded growth. The `LazyLock` init is bounded by 14 languages.
- **Defensive depth tracking**: `depth.saturating_add(1)` prevents `u16` overflow on the depth counter.
- **Error tolerance**: ERROR/MISSING nodes are counted but skipped, maintaining the invariant without terminating traversal.
- **Termination**: All inner loops exit via `level_stack.is_empty()` (return) or sibling advancement. The outer loop terminates when the cursor exhausts the finite tree.
- **Good test coverage**: 29 tests covering types, vocabulary lookup, core linearization, error handling, bounds guards, multi-language, edge cases, and performance.

The two blocking findings are low-severity consistency issues (raw `+=` vs documented `saturating_add`, missing pre-allocation on `level_stack`). Neither poses a runtime risk given the existing bounds guards, but fixing them aligns implementation with stated intent.
