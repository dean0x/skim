# Regression Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10T22:48
**Commits reviewed**: fb539f5, 551d6f1, 81cdb2e (3 commits since a51bd9c)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`NodeInfo` not re-exported from `lib.rs` -- public type inaccessible to downstream consumers** - `crates/rskim-search/src/lib.rs:14-17`
**Confidence**: 92%
- Problem: The `FieldClassifier::classify` trait method now accepts `&NodeInfo` (changed from `&tree_sitter::Node<'_>`). `NodeInfo` is declared `pub` in `types.rs` and is required by any downstream implementor of `FieldClassifier`, but it is NOT included in the `pub use types::{...}` re-export in `lib.rs`. This means external crates cannot construct a `NodeInfo` to call `classify()`, nor can they reference `NodeInfo` in their `impl FieldClassifier` without reaching into `rskim_search::types::NodeInfo` (which is a private module path). The trait is exported but its required parameter type is not.
- Fix: Add `NodeInfo` to the re-export list in `crates/rskim-search/src/lib.rs`:
  ```rust
  pub use types::{
      FieldClassifier, FileId, IndexStats, LayerBuilder, NodeInfo, Result, SearchError,
      SearchField, SearchLayer, SearchQuery, SearchResult, TemporalFlags,
  };
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`FieldClassifier::classify` signature is an intentional breaking change -- no migration path documented** - `crates/rskim-search/src/types.rs:277`
**Confidence**: 82%
- Problem: The `FieldClassifier` trait signature changed from `fn classify(&self, node: &tree_sitter::Node<'_>, source: &str) -> SearchField` to `fn classify(&self, node: &NodeInfo, source: &str) -> SearchField`. This is an intentional decoupling of tree-sitter from the public API (good architectural move), but since the trait was already public and exported in the prior commit on this branch, any in-flight implementors on parallel branches would break silently. The crate is `publish = false` and pre-1.0, so this is not a semver violation, but documenting the break in the PR description or CHANGELOG reduces confusion.
- Fix: No code change required. Acknowledge the breaking trait signature change in the PR description or a CHANGELOG entry. The PR description currently mentions "FieldClassifier traits" but does not call out the signature change from `tree_sitter::Node` to `NodeInfo`.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`NodeInfo::from_ts_node` couples rskim-search to tree-sitter at the type level** - `crates/rskim-search/src/types.rs:258` (Confidence: 65%) -- The stated goal of `NodeInfo` is to decouple `rskim-search` from tree-sitter, yet `NodeInfo::from_ts_node` accepts `&tree_sitter::Node` and `tree-sitter` remains a direct dependency in `Cargo.toml`. The decoupling is partial: the trait is clean but the convenience constructor re-introduces the coupling. A future wave could move `from_ts_node` to an adapter in `rskim-core` or behind a feature flag.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The single blocking issue is that `NodeInfo` must be re-exported from `lib.rs` for `FieldClassifier` to be usable by downstream crates. Without this export, the trait's `classify` method cannot be implemented or called from outside the crate, which is a functional regression relative to the base commit where `classify` accepted a `tree_sitter::Node` (an externally-accessible type). The fix is a one-line addition to the re-export list.
