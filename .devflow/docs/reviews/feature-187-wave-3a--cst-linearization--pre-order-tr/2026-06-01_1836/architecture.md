# Architecture Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**`tree-sitter` direct dependency in `rskim-search` duplicates what `rskim-core` already provides** - `crates/rskim-search/Cargo.toml:20`
**Confidence**: 82%
- Problem: `rskim-search` adds `tree-sitter = { workspace = true }` as a direct dependency. The `rskim-core` crate already depends on `tree-sitter` and re-exports its `Parser` type. The `linearize.rs` module uses `tree-sitter::Tree`, `tree_sitter::TreeCursor`, and `tree_sitter::Language` directly -- these are internal types passed between `rskim-core::AstWalkIter` and the tree created by `rskim-core::Parser`. While this is technically valid (workspace dedup ensures a single copy at link time), it establishes an architectural precedent where `rskim-search` depends on the same tree-sitter version as `rskim-core`, creating a hidden coupling: a tree-sitter major bump must be coordinated across both crates simultaneously. The existing pattern (`rskim-research` depends on `rskim-core` for tree-sitter access but does NOT list `tree-sitter` as a direct dependency in its Cargo.toml) shows the project's convention is to access tree-sitter through `rskim-core`.
- Fix: Remove the direct `tree-sitter` dependency from `rskim-search/Cargo.toml`. Re-export the tree-sitter types needed by `linearize.rs` from `rskim-core` (specifically `tree_sitter::Tree` and `tree_sitter::Language`). The `linearize_tree` function already receives a `&tree_sitter::Tree` -- the type flows from `rskim_core::Parser::parse()` which returns `tree_sitter::Tree`. Re-exporting it makes the dependency chain explicit and single-source.

  Note: After checking `rskim-research/Cargo.toml`, `rskim-research` also does not list `tree-sitter` directly -- it accesses tree-sitter purely through `rskim-core`. This PR breaks that convention. However, this may be intentional to avoid expanding `rskim-core`'s public API surface. Confidence is 82% because there may be a deliberate reason to keep the direct dep (e.g., accessing `tree_sitter::Language::node_kind_for_id` which `rskim-core` does not wrap).

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **LANG_MAPS initialization silently drops languages on grammar load failure** - `crates/rskim-search/src/ast_index/linearize.rs:131-134` (Confidence: 68%) -- The `LANG_MAPS` LazyLock `continue`s past grammar load errors with no logging or diagnostic. In production this path is unreachable (all 14 grammars are compiled in), but if a grammar ABI mismatch occurs after a tree-sitter upgrade, the language will silently produce empty results rather than surfacing the misconfiguration. Consider logging to stderr or returning an error variant that callers can detect. This is a defense-in-depth concern, not a current bug.

- **`LinearNode.depth` uses `u16` while `AstWalkIter` uses `u32`** - `crates/rskim-search/src/ast_index/linearize.rs:72,257` (Confidence: 65%) -- The depth field in `LinearNode` is `u16` (max 65535) while `AstWalkConfig::DEFAULT_MAX_DEPTH` is 500 (well within `u16`), and the code correctly saturates via `.min(u32::from(u16::MAX))`. The narrowing is intentional for memory density (4 bytes per `LinearNode` instead of 6). No bug, but the type mismatch across the abstraction boundary (`AstWalkIter` yields `u32`, consumer stores `u16`) is worth documenting explicitly in `LinearNode`'s doc comment to prevent future confusion.

- **`NODE_KIND_VOCABULARY` binary search in `LANG_MAPS` init assumes sorted vocabulary** - `crates/rskim-search/src/ast_index/linearize.rs:162` (Confidence: 72%) -- The correctness of `binary_search` depends on `NODE_KIND_VOCABULARY` being sorted. There is a test (`vocabulary_is_sorted`) that verifies this, but the invariant is only enforced at test time, not at the init site. A `debug_assert!` at the top of the LazyLock closure would catch regressions earlier during development builds.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Detailed Assessment

### What This PR Does Well (Architecture)

1. **Excellent DFS extraction into shared `AstWalkIter`** (applies ADR-001). The prior duplication of the `TreeCursor` DFS loop in both `rskim-research/ast_extract.rs` and `rskim-search/linearize.rs` was a clear SRP violation. Extracting it to `rskim-core::ast_walk` as a reusable, bounds-guarded iterator is the correct architectural fix. The iterator owns cursor management, depth tracking, and bounds guards; callers (`linearize_tree`, `walk_tree`) own only their domain-specific logic (vocabulary lookup, n-gram emission, `LinearNode` construction). This is a textbook Deep Module pattern -- simple `Iterator` interface hiding the DFS complexity.

2. **Centralized bounds constants**. `AstWalkConfig::DEFAULT_MAX_DEPTH` and `DEFAULT_MAX_NODES` are the single source of truth. Both consumers (`linearize.rs` and `ast_extract.rs`) use `AstWalkConfig::default()` rather than local constants, eliminating the drift risk that existed before. The `#[cfg(test)]` re-exports in `linearize.rs` for test assertions are a clean pattern.

3. **Clean layer separation**. The architecture layers are well-defined:
   - `rskim-core::ast_walk` -- generic DFS iterator (no domain knowledge)
   - `rskim-search::ast_index::linearize` -- CST linearization (vocabulary mapping, `LinearNode` construction)
   - `rskim-research::ast_extract` -- n-gram extraction (bigram/trigram emission, ancestor tracking)
   
   Each layer adds exactly one concern. Dependencies point inward (search/research depend on core, not on each other). This follows Clean Architecture's Dependency Rule.

4. **`LANG_MAPS` LazyLock design**. Using `LazyLock<HashMap<Language, Vec<Option<u16>>>>` for O(1) per-node vocabulary lookup is architecturally sound. The one-time cost (binary search per kind string at init) amortizes to zero over traversal. The `Vec<Option<u16>>` indexed by `kind_id` is the correct data structure -- it trades ~1-2 KiB per language for O(1) array access instead of per-node HashMap lookup.

5. **Error type extension**. The new `SearchError::Ast` variant follows the existing pattern (string-wrapped errors at the boundary, no library types leaking). The distinction between grammar load failures (returns `Err`) and parse failures (returns `Ok(default)`) is architecturally correct -- it separates configuration errors from data errors.

6. **Fused iterator contract**. Implementing `FusedIterator` for `AstWalkIter` is the right stdlib integration -- callers can rely on `take_while`, `chain`, etc. without re-checking `None`.

### Cross-Cycle Awareness

Prior cycle (Cycle 2) resolved 9/11 issues including centralizing bounds constants, preallocating `level_stack`, fusing the iterator, and restoring re-exports. All resolved items remain correctly implemented in the current diff. No regressions detected from prior fixes.

### Architectural Risks Assessed

- **No circular dependencies**: `rskim-core` has no dependency on `rskim-search` or `rskim-research`. Direction is strictly inward.
- **No god modules**: `ast_walk.rs` (258 lines) and `linearize.rs` (274 lines) are well-scoped. The test file (450 lines) is separate.
- **No tight coupling**: `AstWalkIter` communicates only through its `Iterator` trait and the `AstWalkNode` struct -- no callbacks, no trait objects, no shared mutable state.
- **No leaky abstractions**: `LinearNode` uses vocabulary IDs (`u16`), not tree-sitter `Node` references. The tree-sitter dependency does not leak into the public API of `ast_index`.

### Feature Knowledge Alignment

The PR aligns with documented anti-patterns: it uses `AstWalkIter` instead of reimplementing DFS cursor logic (avoids documented anti-pattern). `LANG_MAPS` correctly excludes non-tree-sitter languages (JSON, YAML, TOML). `linearize_source` is the single public API as documented.
