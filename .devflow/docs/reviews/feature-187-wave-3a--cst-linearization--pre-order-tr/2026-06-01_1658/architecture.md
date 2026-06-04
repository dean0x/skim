# Architecture Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01T16:58

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Duplicated bounds constants across three modules** - `linearize.rs:40-45`, `ast_extract.rs:21-24`, `ast_walk.rs:69-71`
**Confidence**: 85%
- Problem: `MAX_AST_DEPTH` (500) and `MAX_AST_NODES` (100,000) are defined identically in three locations: `rskim-search/src/ast_index/linearize.rs`, `rskim-research/src/ast_extract.rs`, and as `AstWalkConfig::default()` in `rskim-core/src/ast_walk.rs`. The extraction of `AstWalkIter` into `rskim-core` was the right moment to centralize these constants alongside the config type. If any caller changes their local constant without updating others, the bounds behavior silently diverges. (applies ADR-001 -- fix noticed issues now rather than deferring)
- Fix: Export the canonical defaults from `AstWalkConfig` and have callers reference them:
  ```rust
  // rskim-core/src/ast_walk.rs
  impl AstWalkConfig {
      pub const DEFAULT_MAX_DEPTH: u32 = 500;
      pub const DEFAULT_MAX_NODES: u32 = 100_000;
  }
  ```
  Then in `linearize.rs` and `ast_extract.rs`:
  ```rust
  const MAX_AST_DEPTH: u32 = AstWalkConfig::DEFAULT_MAX_DEPTH;
  const MAX_AST_NODES: u32 = AstWalkConfig::DEFAULT_MAX_NODES;
  ```
  This keeps the local aliases for readability but sources truth from `rskim-core`.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`AstWalkConfig` fields are `pub` without builder pattern** - `ast_walk.rs:58-65` (Confidence: 65%) -- The struct exposes raw `pub` fields, which means callers can construct invalid configs (e.g., `max_depth: u32::MAX` paired with a `u16` depth in `LinearNode`). A builder or `new()` constructor with validation would strengthen the contract, but for an internal API with two callers this is not urgent.

- **`level_stack` in `AstWalkIter` starts empty with no capacity hint** - `ast_walk.rs:118` (Confidence: 62%) -- `Vec::new()` allocates nothing initially. For typical trees (depth 5-20), this triggers several re-allocations in the first traversal. A small `Vec::with_capacity(32)` would avoid early reallocs. However, tree-sitter traversals are already sub-millisecond so the impact is negligible.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Architectural Assessment

**What this PR does well:**

1. **Single Responsibility (SRP) extraction** -- The `AstWalkIter` cleanly separates generic DFS traversal (cursor management, bounds guarding, depth tracking) from caller-specific logic (vocabulary lookup in `linearize.rs`, bigram/trigram emission in `ast_extract.rs`). Each caller is now ~30 lines of domain logic delegating traversal mechanics to a shared, well-tested iterator. This is textbook information hiding per Parnas (1972).

2. **Correct layering direction** -- The shared iterator lives in `rskim-core` (the lowest layer), and both `rskim-search` and `rskim-research` depend downward into it. No circular dependencies, no upward imports. The dependency direction follows Clean Architecture.

3. **Deep module design** -- `AstWalkIter` has a simple 2-parameter constructor and yields `AstWalkNode` items via `Iterator`. The implementation encapsulates non-trivial cursor state management, depth stack, bounds enforcement, and error/missing node detection. This matches Ousterhout's "deep module" pattern: rich functionality behind a narrow interface.

4. **Consistent error handling** -- `linearize_tree` now returns `LinearizeResult` directly (infallible) instead of `Result<LinearizeResult>`, correctly reflecting that traversal after successful parsing cannot fail. The `SearchError::AstError` -> `Ast` rename follows Rust naming conventions for enum variants.

5. **Feature knowledge compliance** -- The design aligns with documented patterns: AstWalkIter in rskim-core provides shared DFS; linearize.rs delegates DFS to AstWalkIter, owns vocabulary lookup + LinearNode construction; no non-tree-sitter languages in LANG_MAPS. No documented anti-patterns are violated.

**The one condition** is centralizing the duplicated bounds constants (the single MEDIUM finding above), which prevents silent divergence as the codebase evolves.
