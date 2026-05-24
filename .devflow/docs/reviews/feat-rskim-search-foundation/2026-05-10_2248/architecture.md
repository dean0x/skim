# Architecture Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10
**Diff**: `git diff a51bd9c...HEAD` (3 commits, 4 files, +217/-4)

## Issues in Your Changes (BLOCKING)

### HIGH

**NodeInfo not exported from public API but used in public trait signature** - `crates/rskim-search/src/lib.rs:14-17`, `crates/rskim-search/src/types.rs:277`
**Confidence**: 95%
- Problem: The `FieldClassifier` trait is publicly exported and its `classify` method accepts `&NodeInfo` as a parameter. However, `NodeInfo` is NOT re-exported from `lib.rs`. Downstream consumers (including `rskim-core` and any future indexing crate) cannot implement `FieldClassifier` because they cannot name the `NodeInfo` type. The rustdoc build confirms this with warning: "public documentation for `FieldClassifier` links to private item `NodeInfo`".
- Impact: Any crate depending on `rskim-search` that tries to implement `FieldClassifier` will fail to compile. The trait is currently dead code, but the stated goal (per PR description and doc comments) is for it to be implemented by external code. This is a broken public API contract.
- Fix: Add `NodeInfo` to the re-export list in `lib.rs`:
  ```rust
  pub use types::{
      FieldClassifier, FileId, IndexStats, LayerBuilder, NodeInfo, Result, SearchError,
      SearchField, SearchLayer, SearchQuery, SearchResult, TemporalFlags,
  };
  ```

**tree-sitter leaks into rskim-search public API via NodeInfo::from_ts_node** - `crates/rskim-search/src/types.rs:258`
**Confidence**: 90%
- Problem: The doc comments on `NodeInfo` explicitly state its purpose: "rskim-search does not expose tree-sitter as part of its public API." Yet `NodeInfo::from_ts_node` is a `pub` method that accepts `&tree_sitter::Node<'_>`, and `tree-sitter` is a direct dependency in `rskim-search/Cargo.toml`. This contradicts the stated design goal. Any consumer of `NodeInfo` sees `tree-sitter` types in the API surface. The `Cargo.toml` dependency on `tree-sitter` is also not behind a feature gate, meaning all consumers transitively depend on tree-sitter even if they never call `from_ts_node`.
- Impact: Violates the Dependency Inversion Principle (DIP). The decoupling that `NodeInfo` was designed to provide is undermined. Non-tree-sitter consumers (JSON/YAML/TOML classifiers) pull in tree-sitter as a transitive dependency for no reason.
- Fix: Move `from_ts_node` behind an optional feature gate, or move it to a separate module/crate that depends on both `tree-sitter` and `rskim-search`. Example with feature gate:
  ```toml
  # Cargo.toml
  [features]
  tree-sitter = ["dep:tree-sitter"]

  [dependencies]
  tree-sitter = { workspace = true, optional = true }
  ```
  ```rust
  // types.rs
  #[cfg(feature = "tree-sitter")]
  impl NodeInfo {
      #[must_use]
      pub fn from_ts_node(node: &tree_sitter::Node<'_>) -> Self {
          Self {
              kind: node.kind(),
              byte_range: node.byte_range(),
              named_child_count: node.named_child_count(),
          }
      }
  }
  ```
  Alternatively, since this is Wave 0 and `from_ts_node` is a convenience constructor, it could live in the calling code (the indexer in `rskim-core` or a future `rskim-indexer` crate) rather than in the library crate itself.

### MEDIUM

**SearchQuery fields are all public with no builder pattern** - `crates/rskim-search/src/types.rs:119-133`
**Confidence**: 82%
- Problem: `SearchQuery` has 6 public fields that can be directly mutated after construction. The `new()` constructor sets defaults, but callers can bypass it entirely with struct literal construction or mutate fields arbitrarily. As the query API grows (more filters, field weights, etc.), this unconstrained public surface becomes harder to evolve without breaking changes.
- Impact: Future additions to `SearchQuery` (e.g., field boosts, result scoring mode, regex support) will be breaking changes since `SearchQuery` does not have `#[non_exhaustive]`. Adding a new field to a struct with all-public fields forces every struct-literal construction site to update.
- Fix: Add `#[non_exhaustive]` to `SearchQuery` and consider a builder pattern for the optional fields:
  ```rust
  #[derive(Debug, Clone)]
  #[non_exhaustive]
  pub struct SearchQuery {
      pub text: String,
      pub lang: Option<rskim_core::Language>,
      pub ast_pattern: Option<String>,
      pub temporal_flags: Option<TemporalFlags>,
      pub limit: Option<usize>,
      pub offset: Option<usize>,
  }
  ```
  The `#[non_exhaustive]` attribute alone would be sufficient for Wave 0, as it allows adding fields without a semver-major bump.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **SearchResult lacks #[non_exhaustive]** - `crates/rskim-search/src/types.rs:158` (Confidence: 75%) -- Same forward-compatibility concern as SearchQuery. Adding fields (e.g., `highlight_ranges`, `context_lines`) would be a breaking change for any code constructing SearchResult via struct literals.

- **IndexStats lacks #[non_exhaustive]** - `crates/rskim-search/src/types.rs:179` (Confidence: 72%) -- Similar concern. Index statistics will almost certainly grow (avg doc length, vocabulary size, compression ratio).

- **TemporalFlags is thin and may not warrant its own struct** - `crates/rskim-search/src/types.rs:105-109` (Confidence: 65%) -- Currently wraps a single `Option<u32>`. Could be inlined into SearchQuery until more temporal fields are needed. The separate struct adds a layer of indirection with no current benefit.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The crate foundation is well-structured overall: clean separation between the library crate (`rskim-search`) and the CLI stub (`rskim/cmd/search.rs`), proper use of Result types and thiserror, trait-based abstraction for SearchLayer/LayerBuilder/FieldClassifier, and the compile-time canary dev-dependency is a thoughtful quality gate. The NodeInfo abstraction layer is the right architectural instinct for decoupling from tree-sitter. However, the two HIGH issues (NodeInfo not exported, tree-sitter leaking into the public API) directly undermine the stated design goals and need resolution before merge.
