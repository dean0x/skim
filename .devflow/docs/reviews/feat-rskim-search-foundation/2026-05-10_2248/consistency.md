# Consistency Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10
**Commits reviewed**: a51bd9c...HEAD (3 commits: fb539f5, 551d6f1, 81cdb2e)

## Issues in Your Changes (BLOCKING)

### HIGH

**NodeInfo is not re-exported from lib.rs** - `crates/rskim-search/src/lib.rs:14-17`
**Confidence**: 95%
- Problem: `NodeInfo` is defined as `pub struct` in `types.rs` and is part of the `FieldClassifier` trait's public API (the `classify` method accepts `&NodeInfo`), but it is not included in the `pub use types::{...}` re-export list in `lib.rs`. Downstream consumers can use `FieldClassifier` but cannot construct the `NodeInfo` it requires without reaching into private module paths. This is inconsistent with how every other public type in this crate is handled -- `FileId`, `SearchField`, `SearchQuery`, `SearchResult`, `IndexStats`, `TemporalFlags`, and all traits are re-exported.
- Fix: Add `NodeInfo` to the re-export list in `lib.rs`:
```rust
pub use types::{
    FieldClassifier, FileId, IndexStats, LayerBuilder, NodeInfo, Result, SearchError, SearchField,
    SearchLayer, SearchQuery, SearchResult, TemporalFlags,
};
```

### MEDIUM

**NodeInfo missing test for from_ts_node constructor** - `crates/rskim-search/src/types.rs:258-264`
**Confidence**: 82%
- Problem: The `NodeInfo::from_ts_node` method is the primary constructor for `NodeInfo` and the bridge between tree-sitter and the search crate's abstraction. While a `test_node_info_construction` test exercises direct struct construction, there is no test for `from_ts_node` itself. The existing test patterns in this file exercise all public constructors (e.g., `test_search_query_new` tests `SearchQuery::new`). The `from_ts_node` method accesses three tree-sitter `Node` methods (`kind()`, `byte_range()`, `named_child_count()`) and a test would verify these map correctly.
- Fix: Add an integration test that parses a small snippet with tree-sitter and verifies `NodeInfo::from_ts_node` extracts the expected values. The tree-sitter dependency is already available in the crate.

**Test comment style inconsistency in search.rs** - `crates/rskim/src/cmd/search.rs:87-107`
**Confidence**: 80%
- Problem: The new help-flag tests use inline comments to describe test behavior (`// Empty args -> print help -> ExitCode::SUCCESS`, `// --help flag -> print help -> ExitCode::SUCCESS`, etc.), whereas the existing test pattern in this crate uses doc comments (`///`) for test descriptions. Looking at `stats.rs` tests, they use `///` doc comments on test functions (e.g., `/// In-memory mock store for testing...`, `/// AD-AN-2: "Per Session" section is hidden...`). The new deserialization, roundtrip, and IndexStats tests in `types.rs` correctly use `///` doc comments, but the search.rs tests use `//` inline comments inconsistently.
- Fix: Convert the inline comments to `///` doc comments to match the crate convention:
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
```

## Issues in Code You Touched (Should Fix)

_No issues found._

## Pre-existing Issues (Not Blocking)

_No issues found._

## Suggestions (Lower Confidence)

- **NodeInfo could derive PartialEq for test ergonomics** - `crates/rskim-search/src/types.rs:241` (Confidence: 65%) -- Other types in this file (`FileId`, `SearchField`) derive `PartialEq`. `NodeInfo` derives only `Debug, Clone`. Adding `PartialEq` would be consistent with the other types and enable direct `assert_eq!` comparisons in tests. However, `Range<usize>` already implements `PartialEq`, so this is a minor convenience rather than a functional gap.

- **Cargo.toml `lints` section uses `deny` for `expect_used` unlike rskim-core** - `crates/rskim-search/Cargo.toml:20-24` (Confidence: 62%) -- The `rskim-search` lint configuration denies both `unwrap_used` and `expect_used`, while `rskim-core`'s lint configuration (in the workspace) may differ. This is a new crate, so stricter lints are defensible, but worth confirming this is an intentional choice rather than an accidental deviation.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The new `rskim-search` crate demonstrates strong consistency with the existing codebase in most areas: error handling follows the `thiserror`/`Result<T>` pattern from `rskim-core`, documentation style uses the `// ====` section separators, trait design follows the `Send + Sync` convention, `#[must_use]` annotations match existing patterns, the `Cargo.toml` structure mirrors `rskim-core`, and the CLI stub in `search.rs` follows the `run(&[String], &AnalyticsConfig) -> anyhow::Result<ExitCode>` signature used by all other cmd modules. The one blocking issue is the missing `NodeInfo` re-export from `lib.rs`, which makes the `FieldClassifier` trait unusable by downstream consumers since they cannot construct the type its method requires.
