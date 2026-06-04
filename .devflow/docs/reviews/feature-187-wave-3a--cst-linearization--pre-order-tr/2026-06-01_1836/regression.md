# Regression Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01
**Prior Cycle**: Cycle 2 resolved 11 issues (9 fixed, 0 FP, 0 deferred). This cycle verifies no regressions from those fixes and evaluates the final state.

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

- **MISSING node behavioral change in ast_extract.rs** - `crates/rskim-research/src/ast_extract.rs:152` (Confidence: 70%) -- The old `walk_tree` used `node.is_error() || kind == "ERROR"` to detect error nodes. The new code delegates to `AstWalkIter` which uses `node.is_error() || node.is_missing()`. This means MISSING nodes (tree-sitter's placeholder for expected-but-absent syntax) are now excluded from bigram/trigram pairs, whereas before they were treated as normal nodes and registered in the vocabulary. This is arguably a bug fix rather than a regression, since MISSING nodes represent parse artifacts, not real grammar structure. However, it changes the numeric output of `extract_ast_ngrams_from_file` for any source that triggers MISSING nodes. If downstream consumers depend on exact bigram counts matching previous runs (e.g., pre-computed IDF tables or golden test snapshots), this could surface as a discrepancy. The existing tests all pass, which reduces concern.

## Regression Checklist

| Check | Status | Notes |
|-------|--------|-------|
| No exports removed without deprecation | PASS | `MAX_AST_DEPTH` and `MAX_AST_NODES` were private (`const`) in `rskim-research/ast_extract.rs`; no external consumers. New re-exports added: `AstWalkConfig`, `AstWalkIter`, `AstWalkNode` from `rskim-core`, and `LinearNode`, `LinearizeResult`, `linearize_source` from `rskim-search`. |
| Return types backward compatible | PASS | `walk_tree` signature changed but it is a private function. Public API `extract_ast_ngrams_from_file` is unchanged. |
| Default values unchanged (or documented) | PASS | `AstWalkConfig::DEFAULT_MAX_DEPTH` (500) and `DEFAULT_MAX_NODES` (100,000) match the previous hardcoded constants exactly. |
| Side effects preserved | PASS | Error counting, bigram/trigram emission, and trigram cap all preserved. Counter population moved from inline to post-iteration (`iter.error_count()` / `iter.node_count()`), but final values are equivalent. |
| All consumers of changed code updated | PASS | `rskim-research/ast_extract.rs` fully migrated to `AstWalkIter`. `rskim-search/linearize.rs` is new code using the same iterator. |
| Migration complete across codebase | PASS | No remaining references to the removed `WalkContext` struct or old `walk_tree` signature. All `MAX_AST_DEPTH`/`MAX_AST_NODES` references in `rskim-core/transform/` are unrelated (different module, different constants with same name). |
| Commit message matches implementation | PASS | All 5 commits match their implementation: refactor (extract shared iterator), fix (restore exports, centralize bounds), perf (lazy-grow ancestor vec). |
| SearchError::Ast variant is additive | PASS | `SearchError` is `#[non_exhaustive]`, so the new `Ast` variant is backward compatible. All existing `match` expressions have wildcard arms. applies ADR-001 |
| Re-exports preserved | PASS | Cycle 2 resolved the missing re-export issue. `rskim-core/lib.rs` re-exports `AstWalkConfig`, `AstWalkIter`, `AstWalkNode`. `rskim-search/lib.rs` re-exports `LinearNode`, `LinearizeResult`, `linearize_source`. |
| Tests pass | PASS | All 14 `ast_walk` tests pass, all 15 `ast_extract` tests pass (including new `error_node_breaks_ancestor_chain_for_descendants`), all 29 `linearize` tests pass. |

## Cross-Cycle Verification

| Cycle 2 Fix | Verified |
|-------------|----------|
| Centralize bounds constants | PASS -- `AstWalkConfig::DEFAULT_MAX_DEPTH` / `DEFAULT_MAX_NODES` are the canonical source; `linearize.rs` uses `#[cfg(test)]` aliases, `ast_extract.rs` references docs. |
| Prealloc level_stack | PASS -- `Vec::with_capacity((config.max_depth as usize).min(64))` in `AstWalkIter::new`. |
| Fused iterator | PASS -- `FusedIterator` impl present, `done` flag never cleared. |
| Strengthen error test | PASS -- `error_nodes_flagged` test asserts `is_error` for broken syntax. |
| Rename test | PASS -- `missing_nodes_flagged` (was `missing_nodes_are_flagged`). |
| Re-add `#[must_use]` | PASS -- `#[must_use]` on `AstWalkIter::new`, `node_count()`, `error_count()`. |
| Restore re-exports | PASS -- `rskim-core/lib.rs` line 45, `rskim-search/lib.rs` line 35. |
| Lazy-grow ancestor vec | PASS -- `vec![None; 64]` with `resize` on demand at line 148-149 in `ast_extract.rs`. |
| Chain-break test | PASS -- `error_node_breaks_ancestor_chain_for_descendants` test at line 493 in `ast_extract.rs`. |

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED

The refactoring from duplicated hand-rolled `TreeCursor` DFS to a shared `AstWalkIter` is clean, well-tested, and introduces no regressions. All public APIs are preserved. All Cycle 2 fixes are verified. The new `SearchError::Ast` variant is properly additive behind `#[non_exhaustive]`. The one behavioral change (MISSING node handling) is an improvement, not a regression, and is covered by tests. avoids PF-002
