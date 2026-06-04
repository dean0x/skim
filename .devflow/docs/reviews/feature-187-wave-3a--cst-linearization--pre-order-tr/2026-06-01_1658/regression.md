# Regression Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**`#[must_use]` attribute removed from `linearize_source`** - `crates/rskim-search/src/ast_index/linearize.rs:195`
**Confidence**: 82%
- Problem: The base branch had `#[must_use = "linearize_source returns a Result that must be checked"]` on the public `linearize_source` function. This PR removes it. While `Result` itself carries `#[must_use]` in Rust's standard library (so the compiler still warns on unused Results), the custom message provided extra context for callers about the importance of checking the error variant.
- Fix: Not strictly required since `Result` is inherently `#[must_use]`, but re-adding the custom annotation preserves the explicit documentation intent:
  ```rust
  #[must_use = "linearize_source returns a Result that must be checked"]
  pub fn linearize_source(
  ```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Performance threshold relaxed from 5ms to 10ms** - `crates/rskim-search/src/ast_index/linearize_tests.rs:445` (Confidence: 65%) -- The old test generated 100 functions but was labeled "1000-line"; the new test correctly generates 1000 functions and adjusts the threshold to 10ms. This is a test correctness fix, not a performance regression, but the 2x relaxation on a 10x larger input means the per-function throughput actually improved. Worth confirming with benchmarks that the `AstWalkIter` extraction did not add per-node overhead.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Detailed Analysis

### Regression Checklist

- [x] **No exports removed without deprecation**: `linearize_source`, `LinearNode`, `LinearizeResult` remain exported from `rskim-search`. `AstWalkConfig`, `AstWalkIter`, `AstWalkNode` are newly exported from `rskim-core`. The `SearchError::AstError` variant was renamed to `SearchError::Ast` -- this is a breaking change to the enum variant name, but search of the codebase confirms zero external match-arm consumers; all references are updated.
- [x] **Return types backward compatible**: `linearize_source` still returns `Result<LinearizeResult>`. Internal `linearize_tree` changed from `Result<LinearizeResult>` to `LinearizeResult`, but it is `fn` (private) -- not a public API change. The wrapping `Ok()` is added at the call site.
- [x] **Default values unchanged**: `MAX_AST_DEPTH` value stays at 500, `MAX_AST_NODES` stays at 100,000. `AstWalkConfig::default()` uses the same values.
- [x] **Side effects preserved**: Error counting, node counting, traversal order all preserved.
- [x] **All consumers of changed code updated**: `rskim-search/linearize.rs` and `rskim-research/ast_extract.rs` both migrated to `AstWalkIter`. Benchmark file (`linearize_bench.rs`) unchanged and compiles.
- [x] **Migration complete across codebase**: No remaining `AstError` references. No remaining hand-rolled TreeCursor DFS loops in the migrated modules.
- [x] **Commit message matches implementation**: PR description says "shared AstWalkIter" -- confirmed: the `AstWalkIter` is extracted into `rskim-core::ast_walk` and consumed by both `linearize.rs` and `ast_extract.rs`.

### Traversal Behavior Equivalence (applies ADR-001)

The core regression risk in this PR is that the extracted `AstWalkIter` must produce identical traversal behavior to the two hand-rolled loops it replaces. Analysis confirms equivalence:

1. **Pre-order DFS order**: Both old loops and `AstWalkIter` use `goto_first_child()` / `goto_next_sibling()` / `goto_parent()` in the same order. Root is yielded first.

2. **Depth tracking**: Old `linearize.rs` used `u16` depth with `saturating_add(1)`. Old `ast_extract.rs` used `usize` depth with `+1`. New `AstWalkIter` uses `u32` depth with `saturating_add(1)`. The `linearize_tree` caller correctly converts `u32` -> `u16` via `item.depth.min(u32::from(u16::MAX)) as u16`.

3. **Bounds guarding**: Old loops checked `depth >= MAX` and `node_count >= MAX` at the top of each iteration, then skipped the subtree. `AstWalkIter` does the same check inside `next()` with `skip_subtree()`. The skip logic (goto_next_sibling or goto_parent until sibling found) is identical.

4. **Error node handling**: Old `linearize.rs` used `node.is_error() || node.is_missing()`. Old `ast_extract.rs` used `node.is_error() || kind == "ERROR"`. New `AstWalkIter` uses `node.is_error() || node.is_missing()`. The `kind == "ERROR"` string check was redundant with `node.is_error()` -- `is_error()` returns true for ERROR nodes. Adding `is_missing()` to the ast_extract path is a correctness improvement, not a regression (avoids PF-002).

5. **Ancestor context (ast_extract.rs)**: Old code used `parent_id` / `grandparent_id` variables threaded through the loop stack. New code uses a `Vec<Option<NodeKindId>>` indexed by depth. Both approaches correctly track the parent and grandparent for bigram/trigram emission. The depth-indexed approach is simpler and eliminates the `WalkContext` struct.

6. **Counter semantics**: Old `linearize.rs` incremented `result.node_count` inside the loop. New code reads `iter.node_count()` after exhaustion. Both count all yielded nodes (error + non-error). The invariant `node_count == nodes.len() + error_count` is preserved and tested.

### SearchError::AstError -> SearchError::Ast Rename

This is a variant rename on a public enum. The codebase has no match arms on `AstError` outside linearize.rs itself. The rename is complete (zero references to `AstError` remain). External consumers would see a compile error if they matched on `AstError`, but this crate is an internal dependency not published to crates.io independently.

### Test Coverage Delta

- **Removed**: `known_kind_roundtrips_through_lang_map` -- this tested that `binary_search("function_item")` resolves correctly. Coverage is maintained by `rust_lang_map_contains_known_kinds` (line 86-93) which verifies the same property through the lang map.
- **Added**: 14 tests in `ast_walk::tests` covering pre-order order, depth, error/missing nodes, bounds guards, zero limits, and invariants.
- **Modified**: Performance test now generates 1000 functions (was 100) with 10ms threshold (was 5ms). This is a correctness fix -- the old test name said "1000-line" but only generated 100 lines.

All 57 relevant tests pass (14 ast_walk + 29 ast_index + 14 ast_extract).
