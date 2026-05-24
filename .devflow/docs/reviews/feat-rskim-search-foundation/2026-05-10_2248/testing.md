# Testing Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10
**Diff**: `git diff a51bd9c...HEAD`
**PR**: #213

## Issues in Your Changes (BLOCKING)

### HIGH

**NodeInfo missing from crate re-exports makes `from_ts_node` untestable by downstream consumers** - `crates/rskim-search/src/lib.rs:14-17`
**Confidence**: 90%
- Problem: `NodeInfo` is declared `pub` in `types.rs` (line 242) and is a required parameter of the `FieldClassifier::classify` trait method (line 277), but it is NOT re-exported from `lib.rs`. This means downstream crates (including the `rskim` binary's compile-time canary added in this PR) cannot construct a `NodeInfo` to call `FieldClassifier::classify`, and cannot test their `FieldClassifier` implementations. The compile-time canary in `crates/rskim/Cargo.toml` exists specifically to catch API surface issues like this, yet it currently has no test exercising the `FieldClassifier` + `NodeInfo` path, so the gap is invisible.
- Fix: Add `NodeInfo` to the `pub use types::{...}` list in `lib.rs`, and add a downstream test in the `rskim` crate's dev-dependencies that constructs a `NodeInfo` and passes it to a mock `FieldClassifier`. This validates the public API surface that the canary is supposed to guard.

```rust
// lib.rs fix:
pub use types::{
    FieldClassifier, FileId, IndexStats, LayerBuilder, NodeInfo, Result, SearchError,
    SearchField, SearchLayer, SearchQuery, SearchResult, TemporalFlags,
};
```

### MEDIUM

**No test for `NodeInfo::from_ts_node` constructor** - `crates/rskim-search/src/types.rs:258-264`
**Confidence**: 85%
- Problem: The `from_ts_node` constructor is new code introduced in this PR. While `test_node_info_construction` tests direct struct construction (verifying field access), there is no test for the `from_ts_node` method that converts a `tree_sitter::Node` into a `NodeInfo`. This is the primary entry point for real-world usage and the correctness of `kind`, `byte_range`, and `named_child_count` extraction from a tree-sitter node is untested.
- Fix: Add an integration test that parses a small source snippet with tree-sitter, walks to a known node, and calls `NodeInfo::from_ts_node` to verify the fields match expected values.

```rust
#[test]
fn test_node_info_from_ts_node() {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
    let source = "fn hello() {}";
    let tree = parser.parse(source, None).unwrap();
    let root = tree.root_node();
    let fn_node = root.child(0).unwrap(); // function_item
    let info = NodeInfo::from_ts_node(&fn_node);
    assert_eq!(info.kind, "function_item");
    assert_eq!(info.byte_range, 0..source.len());
    assert!(info.named_child_count > 0);
}
```

**No test for `FieldClassifier` trait contract with `NodeInfo`** - `crates/rskim-search/src/types.rs:275-278`
**Confidence**: 82%
- Problem: The `FieldClassifier` trait accepts `&NodeInfo` instead of `&tree_sitter::Node` (the key decoupling change in this PR), but there is no test demonstrating that a concrete `FieldClassifier` implementation can be written and used with `NodeInfo`. Without a test double implementing the trait, there is no compile-time or runtime verification that the trait is actually implementable and usable as designed.
- Fix: Add a simple mock/fake `FieldClassifier` in the test module and exercise it with a `NodeInfo`.

```rust
struct TestClassifier;
impl FieldClassifier for TestClassifier {
    fn classify(&self, node: &NodeInfo, _source: &str) -> SearchField {
        match node.kind {
            "function_item" | "function_definition" => SearchField::FunctionSignature,
            _ => SearchField::Other,
        }
    }
}

#[test]
fn test_field_classifier_with_node_info() {
    let classifier = TestClassifier;
    let info = NodeInfo {
        kind: "function_item",
        byte_range: 0..10,
        named_child_count: 2,
    };
    assert_eq!(classifier.classify(&info, ""), SearchField::FunctionSignature);

    let other = NodeInfo {
        kind: "comment",
        byte_range: 0..5,
        named_child_count: 0,
    };
    assert_eq!(classifier.classify(&other, ""), SearchField::Other);
}
```

**`IndexStats` roundtrip deserialization not tested** - `crates/rskim-search/src/types.rs:567-597`
**Confidence**: 80%
- Problem: `IndexStats` has `Serialize` and `Deserialize` derives, and the tests verify serialization output shape, but no roundtrip test verifies that deserializing the serialized JSON produces the correct `IndexStats` back. This is inconsistent with `SearchResult` which does have roundtrip tests (`test_search_result_roundtrip`). Since `IndexStats` will be persisted/loaded from index files, its deserialization correctness matters.
- Fix: Add an `IndexStats` roundtrip test following the same pattern as `test_search_result_roundtrip`.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`SearchLayer` and `LayerBuilder` traits have zero test coverage** - `crates/rskim-search/src/types.rs:199-229`
**Confidence**: 82%
- Problem: These are the two primary traits of the crate's public API, yet neither has any test verifying that a concrete implementation can be built, called, or type-checked. This is a foundation crate -- the traits define the contract that all future search layer implementations must follow. Testing with a minimal fake implementation would catch trait signature issues (e.g., the `where Self: Sized` bound on `build`) before downstream consumers try to implement them.
- Fix: Add a minimal `InMemorySearchLayer` fake that implements both traits, verifying the compile-time contract and basic behavior.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`test_search_result_roundtrip_null_snippet` weaker assertions than its sibling** - `crates/rskim-search/src/types.rs:545-563` (Confidence: 70%) -- The committed version of this test only checks `file_id`, `snippet`, and `field`, while its sibling `test_search_result_roundtrip` checks all 6 fields including `score`, `line_range`, and `match_positions`. Uncommitted working-copy changes appear to fix this, but the committed version is asymmetric. Aligning both roundtrip tests to assert all fields would be more thorough.

- **Compile-time canary in `rskim` has no actual API exercise test** - `crates/rskim/Cargo.toml:47-50` (Confidence: 65%) -- The `rskim-search` dev-dependency is described as a "compile-time API canary" but no test in the `rskim` crate actually imports or uses any `rskim_search` types. A `use rskim_search::*;` in a test function would make the canary effective at catching export regressions.

- **`TemporalFlags` has no dedicated test** - `crates/rskim-search/src/types.rs:105-109` (Confidence: 62%) -- `TemporalFlags` is only tested indirectly via `test_search_query_with_filters`. A dedicated test for its `Default` impl and serialization/deserialization would improve coverage for when more temporal fields are added.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 3 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

### Rationale

The existing tests are well-structured: they follow clear AAA patterns, test behavior rather than implementation, have good doc comments, and cover serialization thoroughly for the types they do cover. The `test_search_field_serde_agrees_with_name` test is a particularly good pattern for verifying two sources of truth stay synchronized.

However, the PR introduces a key architectural change (decoupling `FieldClassifier` from tree-sitter via `NodeInfo`) and the testing does not validate this change end-to-end. The `NodeInfo` type is not re-exported (so downstream consumers cannot use it), the `from_ts_node` constructor has no test, and neither primary trait (`SearchLayer`, `LayerBuilder`) has any test coverage. For a foundation crate that defines contracts for future implementations, these gaps leave the core API surface unvalidated.
