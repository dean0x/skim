# Dependencies Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16T23:08

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

(none)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Dependencies Score**: 9/10
**Recommendation**: APPROVED

## Analysis Notes

### Dependency Change Summary

The only dependency change in this PR is the addition of `tree-sitter = { workspace = true }` to `crates/rskim-search/Cargo.toml` (line 21).

### Evaluation

**No new transitive dependencies introduced.** `tree-sitter v0.25.10` was already a transitive dependency of `rskim-search` via `rskim-core`. The Cargo.lock diff is a single line adding `tree-sitter` to the `rskim-search` package entry -- no new crates are pulled into the dependency graph.

**Version is workspace-managed.** The dependency uses `{ workspace = true }`, resolving to `tree-sitter = "0.25"` defined in the root `Cargo.toml`. This ensures version consistency with `rskim-core` and all grammar crates. No version duplication detected via `cargo tree -d`.

**Dependency is justified.** The new `classify_source()` function in `crates/rskim-search/src/lexical/classifier.rs` walks the tree-sitter AST directly (calling `.root_node()`, `.walk()`, `.byte_range()`, `.kind()`, `.goto_first_child()`, `.goto_next_sibling()`, `.goto_parent()` on tree-sitter types). While these types flow through `rskim_core::Parser::parse()` which returns `tree_sitter::Tree`, making the dependency explicit is correct practice -- it documents the coupling and prevents silent breakage if rskim-core ever changes its re-export strategy.

**No grammar crate leakage.** `rskim-search` depends only on the core `tree-sitter` crate, not on any `tree-sitter-{language}` grammar crates. Grammar resolution is correctly delegated to `rskim-core::Parser`, maintaining the existing architectural boundary.

**Good documentation.** The dependency line includes a comment explaining its purpose: `# tree-sitter is used by the BM25F classifier to walk AST nodes for field classification.`

**No feature flags concerns.** Default features are used, matching the workspace configuration.

### Architecture Observation (informational, not blocking)

The codebase defines a `NodeInfo` abstraction (in `types.rs`) and `FieldClassifier` trait specifically designed so that "non-tree-sitter languages can implement this trait without depending on the tree-sitter crate." However, the current `classify_source()` implementation walks the tree-sitter AST directly rather than going through the `NodeInfo`/`FieldClassifier` abstraction. This is a design choice -- the direct approach is simpler and avoids an unnecessary indirection layer for the current use case. If the abstraction becomes the canonical path in the future, the direct `tree-sitter` dependency on `rskim-search` could potentially be removed.
