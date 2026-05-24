# Rust Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10
**Commits reviewed**: a51bd9c...HEAD (3 commits: fb539f5, 551d6f1, 81cdb2e)

## Issues in Your Changes (BLOCKING)

### HIGH

**`NodeInfo` is public but not re-exported from `lib.rs`** - `crates/rskim-search/src/lib.rs:14-17`
**Confidence**: 95%
- Problem: `NodeInfo` is a `pub struct` (types.rs:242) used in the signature of the `pub trait FieldClassifier::classify(&self, node: &NodeInfo, source: &str)` (types.rs:277). However, `NodeInfo` is absent from the `pub use` re-export in `lib.rs:14-17`. Any downstream crate that imports `rskim_search::FieldClassifier` cannot name the `NodeInfo` type required to implement or call `classify`, making the trait unusable outside the crate. The `rskim-search` Cargo.toml `#[deny(unreachable_pub)]` lint is not enabled, so this compiles silently.
- Fix: Add `NodeInfo` to the re-export list:
```rust
pub use types::{
    FieldClassifier, FileId, IndexStats, LayerBuilder, NodeInfo, Result, SearchError,
    SearchField, SearchLayer, SearchQuery, SearchResult, TemporalFlags,
};
```

**`NodeInfo::from_ts_node` leaks tree-sitter into public API despite stated goal of decoupling** - `crates/rskim-search/src/types.rs:258`
**Confidence**: 85%
- Problem: The doc comment on `NodeInfo` (types.rs:231-240) explicitly states the purpose is to keep `rskim-search` from exposing tree-sitter as part of its public API, so non-tree-sitter languages can implement `FieldClassifier`. However, `from_ts_node` is `pub` and accepts `&tree_sitter::Node<'_>`, which means `rskim-search` must declare `tree-sitter` as a dependency (it does, in Cargo.toml:15) and any consumer using `from_ts_node` must also depend on `tree-sitter`. This contradicts the decoupling goal. The conversion should live in the call-site crate (e.g. `rskim-core` or the indexer crate), not in `rskim-search`.
- Fix: Either (a) move `from_ts_node` to an extension trait in `rskim-core` / the indexer crate so `rskim-search` drops its `tree-sitter` dependency entirely, or (b) gate it behind a cargo feature (`tree-sitter` feature, off by default):
```rust
// Option (a): Move to the consuming crate, remove tree-sitter dep from rskim-search
// In rskim-core or indexer crate:
impl NodeInfo {
    pub fn from_ts_node(node: &tree_sitter::Node<'_>) -> Self {
        Self {
            kind: node.kind(),
            byte_range: node.byte_range(),
            named_child_count: node.named_child_count(),
        }
    }
}

// Option (b): Feature-gate in rskim-search/Cargo.toml:
// [features]
// tree-sitter = ["dep:tree-sitter"]
// [dependencies]
// tree-sitter = { workspace = true, optional = true }
```

### MEDIUM

**No test for `NodeInfo::from_ts_node` constructor** - `crates/rskim-search/src/types.rs:258-264`
**Confidence**: 82%
- Problem: `test_node_info_construction` (types.rs:356) only tests direct struct construction. The `from_ts_node` method -- which extracts `kind`, `byte_range`, and `named_child_count` from a `tree_sitter::Node` -- has no test coverage. If the tree-sitter API changes field semantics (e.g., `byte_range()` returns different bounds), the mapping would silently break. This is the only non-trivial constructor in the module.
- Fix: Add a test that parses a small snippet with tree-sitter, then calls `from_ts_node` and asserts the resulting `NodeInfo` fields match expectations:
```rust
#[test]
fn test_node_info_from_ts_node() {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
    let tree = parser.parse("fn main() {}", None).unwrap();
    let root = tree.root_node();
    let info = NodeInfo::from_ts_node(&root);
    assert_eq!(info.kind, "source_file");
    assert_eq!(info.byte_range, 0..13);
    assert!(info.named_child_count > 0);
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`SearchResult` lacks `PartialEq` -- consider a manual impl** - `crates/rskim-search/src/types.rs:158` (Confidence: 65%) -- The doc comment explains `f64` prevents deriving `PartialEq`, but a manual impl using epsilon comparison or `f64::total_cmp` (stable since Rust 1.62) would enable simpler test assertions and downstream equality checks without the field-by-field pattern repeated across roundtrip tests.

- **`SearchQuery` does not derive `Serialize`/`Deserialize`** - `crates/rskim-search/src/types.rs:119-120` (Confidence: 60%) -- Other search types (`SearchResult`, `IndexStats`, `SearchField`) derive serde traits. `SearchQuery` is the primary input to `SearchLayer::search` and will likely need serialization for query logging, caching, or CLI `--json` input. Adding serde derives now prevents a breaking change later if the struct gains non-public fields.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The type design is well-structured: newtype `FileId`, exhaustive `SearchField` enum with serde sync test, `thiserror`-based errors, and proper `#[must_use]` annotations all follow Rust best practices. The `NodeInfo` abstraction to decouple `FieldClassifier` from tree-sitter is architecturally sound. However, two issues undermine the decoupling goal: (1) `NodeInfo` is not re-exported, making `FieldClassifier` unusable by downstream crates, and (2) `from_ts_node` re-introduces the tree-sitter dependency that `NodeInfo` was designed to eliminate. Both should be resolved before merge.
