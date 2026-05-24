# Code Review Summary

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10_2248
**Reviewers**: 8 agents (architecture, complexity, consistency, performance, regression, rust, security, testing)

## Merge Recommendation: CHANGES_REQUESTED

The PR foundation is well-structured with exemplary complexity and performance characteristics. However, **2 HIGH issues in the public API must be resolved before merge**. Both relate to `NodeInfo` and were flagged by 4-5 independent reviewers with 90%+ confidence, indicating high certainty.

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 0 | 0 |
| Should Fix | 0 | 0 | 4 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Total Blocking Issues**: 2 (both HIGH)
**Total Should-Fix Issues**: 4 (all MEDIUM)

---

## Blocking Issues

### 1. NodeInfo Not Re-exported from lib.rs

**Location**: `crates/rskim-search/src/lib.rs:14-17`
**Severity**: HIGH
**Confidence**: 95% (flagged by architecture, consistency, regression, rust, testing reviewers)

**Problem**: 
The `FieldClassifier` trait is publicly exported and its `classify` method signature requires `&NodeInfo` as a parameter. However, `NodeInfo` is NOT included in the `pub use types::{...}` re-export list in `lib.rs`. This means:
- Downstream crates importing `FieldClassifier` cannot name the `NodeInfo` type
- They cannot implement the trait because they cannot construct a `NodeInfo` to pass to `classify()`
- The trait is exported but unusable to external consumers
- rustdoc build confirms this with warning: "public documentation for `FieldClassifier` links to private item `NodeInfo`"

**Impact**: The stated goal (per PR description) is for `FieldClassifier` to be implemented by external code. This is currently impossible. The trait is dead code.

**Fix**:
```rust
// crates/rskim-search/src/lib.rs (lines 14-17)
pub use types::{
    FieldClassifier, FileId, IndexStats, LayerBuilder, NodeInfo, Result, SearchError,
    SearchField, SearchLayer, SearchQuery, SearchResult, TemporalFlags,
};
```

---

### 2. tree-sitter Leaks into Public API via NodeInfo::from_ts_node

**Location**: `crates/rskim-search/src/types.rs:258`
**Severity**: HIGH
**Confidence**: 90% (flagged by architecture, rust reviewers)

**Problem**:
The `NodeInfo` doc comments (lines 231-240) explicitly state: "rskim-search does not expose tree-sitter as part of its public API." Yet `NodeInfo::from_ts_node` is public and accepts `&tree_sitter::Node<'_>`. The `tree-sitter` crate is a direct dependency in `Cargo.toml`, meaning:
- All consumers of `rskim-search` transitively depend on `tree-sitter` even if they never use `from_ts_node`
- The decoupling that `NodeInfo` was designed to provide is undermined
- Non-tree-sitter classifiers (JSON/YAML/TOML) pull in an unnecessary tree-sitter dependency

**Impact**: Violates the Dependency Inversion Principle. The architectural goal of decoupling `FieldClassifier` from tree-sitter is contradicted by the public API.

**Fix** (Option A - Preferred):
Move `from_ts_node` to the calling code (`rskim-core` or a future indexer crate). `rskim-search` would then drop `tree-sitter` as a dependency entirely:

```rust
// In rskim-core or the indexer crate that consumes both rskim-search and tree-sitter:
impl NodeInfo {
    pub fn from_ts_node(node: &tree_sitter::Node<'_>) -> Self {
        Self {
            kind: node.kind(),
            byte_range: node.byte_range(),
            named_child_count: node.named_child_count(),
        }
    }
}
```

**Fix** (Option B - Feature Gate):
If `from_ts_node` must stay in `rskim-search`, gate it behind an optional feature:

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

---

## Should-Fix Issues (MEDIUM)

### 1. Missing Test for NodeInfo::from_ts_node

**Location**: `crates/rskim-search/src/types.rs:258-264`
**Severity**: MEDIUM
**Confidence**: 85% (flagged by rust, testing reviewers)

**Problem**:
The `from_ts_node` constructor is the primary real-world entry point for `NodeInfo`. It extracts `kind`, `byte_range`, and `named_child_count` from a tree-sitter node with zero test coverage. The existing `test_node_info_construction` test only verifies direct struct construction.

**Fix**: Add an integration test:
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

---

### 2. No Concrete Implementation Test for FieldClassifier Trait

**Location**: `crates/rskim-search/src/types.rs:275-278`
**Severity**: MEDIUM
**Confidence**: 82% (testing reviewer)

**Problem**:
The `FieldClassifier` trait accepts `&NodeInfo` (key architectural change), but no test demonstrates that a concrete implementation can be written and used. For a foundation crate defining API contracts, this is a significant gap.

**Fix**: Add a minimal test implementation:
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

---

### 3. Test Comment Style Inconsistency in search.rs

**Location**: `crates/rskim/src/cmd/search.rs:87-107`
**Severity**: MEDIUM
**Confidence**: 80% (consistency reviewer)

**Problem**:
New help-flag tests use inline comments (`//`) while the codebase convention uses doc comments (`///`). The `stats.rs` tests and new tests in `types.rs` both use `///` doc comments.

**Fix**: Convert inline comments to doc comments:
```rust
/// Empty args prints help and returns ExitCode::SUCCESS.
#[test]
fn test_search_help_returns_success() {
    let result = run(&[], &TEST_ANALYTICS).unwrap();
    assert_eq!(result, ExitCode::SUCCESS);
}

/// --help flag prints help and returns ExitCode::SUCCESS.
#[test]
fn test_search_help_flag_returns_success() {
    // ...
}
```

---

### 4. IndexStats Roundtrip Deserialization Not Tested

**Location**: `crates/rskim-search/src/types.rs:567-597`
**Severity**: MEDIUM
**Confidence**: 80% (testing reviewer)

**Problem**:
`IndexStats` derives `Serialize` and `Deserialize`, but unlike `SearchResult` (which has `test_search_result_roundtrip`), there is no test verifying that deserializing serialized JSON produces the correct `IndexStats` back. Since `IndexStats` will be persisted/loaded from index files, this correctness matters.

**Fix**: Add an `IndexStats` roundtrip test following the same pattern as `test_search_result_roundtrip`.

---

## Strengths

**Exemplary Design**:
- Complexity score: 9/10 - All functions have cyclomatic complexity <5, no deep nesting
- Flat structure in `types.rs` (314 lines production, 284 lines tests) is the correct shape for a foundation module
- Sound trait-based abstraction with `SearchLayer`, `LayerBuilder`, `FieldClassifier`

**Strong Performance Foundation**:
- Performance score: 9/10 - Pure computation, no I/O surface
- Copy-friendly types (`FileId`, `SearchField`), zero-allocation field naming
- `NodeInfo` abstraction costs nothing at runtime (40 bytes of stack data, all Copy types)

**Security Posture**:
- Security score: 9/10 - No unsafe code, no I/O surface, strict clippy lints (`unwrap_used`, `expect_used` denied)
- Type-safe design (`FileId` newtype, exhaustive enum matches)
- Sound error handling with `thiserror`

**API Consistency**:
- Follows project conventions for error handling, trait design, `#[must_use]` annotations
- CLI stub matches the `run(&[String], &AnalyticsConfig) -> anyhow::Result<ExitCode>` signature used throughout
- Compile-time canary dev-dependency is a thoughtful quality gate

---

## Action Plan

1. **Add `NodeInfo` to lib.rs re-export** (blocking) - 5 minutes
2. **Resolve tree-sitter dependency leak** (blocking) - Choose Option A or B and implement - 15 minutes
3. **Add test for `NodeInfo::from_ts_node`** (should-fix) - 10 minutes
4. **Add concrete `FieldClassifier` implementation test** (should-fix) - 10 minutes
5. **Fix test comment style in search.rs** (should-fix) - 5 minutes
6. **Add `IndexStats` roundtrip test** (should-fix) - 10 minutes

**Estimated total effort**: ~55 minutes

Once these are resolved, the PR is ready to merge. The architectural foundation is sound; these are tactical fixes to align the public API surface with stated design goals.
