# Architecture Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16T23:08

## Issues in Your Changes (BLOCKING)

### HIGH

**Unused `tree-sitter` direct dependency in rskim-search** - `crates/rskim-search/Cargo.toml:21`
**Confidence**: 90%
- Problem: The `tree-sitter = { workspace = true }` dependency was added to `rskim-search/Cargo.toml` with the comment "used by the BM25F classifier to walk AST nodes for field classification." However, no non-test code in `rskim-search` directly imports `tree_sitter::*`. The classifier in `classifier.rs` accesses tree-sitter types transitively through `rskim_core::Parser::parse()`, which returns a `tree_sitter::Tree`. The direct dependency declaration is unnecessary -- `rskim-core` already re-exports the parser, and the tree-sitter types flow through the `rskim_core::Parser` API.
- Fix: Remove the direct `tree-sitter` dependency from `crates/rskim-search/Cargo.toml`. The transitive dependency via `rskim-core` provides everything the classifier needs. If the direct dep was added for a planned future use, document the intent or defer until actually needed.

**Dead abstraction: `FieldClassifier` trait and `NodeInfo` type are defined but unused by the actual classification path** - `crates/rskim-search/src/types.rs:405-441`
**Confidence**: 85%
- Problem: The PR introduces `NodeInfo` (a language-neutral AST node representation) and `FieldClassifier` (a trait for classifying nodes into `SearchField` variants) in `types.rs`, and both are publicly exported from `lib.rs`. However, the actual classification logic in `lexical/classifier.rs` does not use either type. Instead, `classify_source()` directly walks tree-sitter nodes via `rskim_core::Parser::parse()` and uses `rskim_core::node_kind_priority()` + the local `map_priority_to_field()` function. This creates two parallel classification APIs:
  1. The trait-based path (`FieldClassifier` + `NodeInfo`) -- defined, exported, tested with a mock, but not wired into any production code.
  2. The function-based path (`classify_source()` using `map_priority_to_field()`) -- the actual working implementation.

  This violates SRP: the types module now contains abstractions that serve no consumer. The integration test in `crates/rskim/tests/search_api.rs` imports `FieldClassifier` and `NodeInfo`, but only exercises a trivial mock implementation -- it does not validate the real classification path.
- Fix: Either (a) wire `FieldClassifier` into the actual classification path by having `classify_source()` accept a `&dyn FieldClassifier` parameter (DIP-compliant), or (b) remove `NodeInfo` and `FieldClassifier` from the public API and defer their introduction until a consumer actually needs the abstraction. Option (b) is recommended per YAGNI: the current free-function design (`classify_source`) is simpler and testable. If trait-based extensibility is planned for non-tree-sitter classifiers, document the roadmap in a code comment.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Classifier directly couples to `rskim_core::node_kind_priority()` internals** - `crates/rskim-search/src/lexical/classifier.rs:43-78`
**Confidence**: 80%
- Problem: The `map_priority_to_field()` function accepts a `kind: &str` and a `priority: u8` (from `rskim_core::node_kind_priority()`), but then it also does its own `kind`-based matching for comments, strings, and identifiers (lines 46-68). This means the classification logic is split across two crates: `rskim_core::node_kind_info` handles priority mapping (1-5), while `rskim_search::map_priority_to_field` re-matches specific node kinds for finer-grained fields. If `rskim_core` ever changes its priority scheme or adds new node kinds, the hardcoded kind strings in `map_priority_to_field` may silently produce incorrect classifications.
- Fix: Consider one of: (a) extend the `node_kind_info` function in `rskim_core` to return a richer enum that includes comment/string/identifier categories (single source of truth), or (b) add a compile-time or test-time assertion that the kind-to-field mapping in `map_priority_to_field` stays consistent with `rskim_core`'s known node kinds. At minimum, document the coupling with a `// COUPLING: synced with rskim_core::transform::utils::node_kind_info` comment.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Classifier per-byte allocation may be expensive for large files** - `crates/rskim-search/src/lexical/classifier.rs:131` (Confidence: 65%) -- The classifier allocates a `Vec<SearchField>` of `source.len()` bytes. For the 100 MiB cap this is 100 MiB of u8 values. The `MAX_SOURCE_BYTES` constant guards against unbounded allocation, but 100 MiB is generous. For real source files (rarely >10 MiB), the current cap is fine. If memory pressure becomes a concern, a run-length approach during traversal (rather than post-traversal RLE) would avoid the large allocation entirely.

- **Multiple `HashMap` allocations in search hot path** - `crates/rskim-search/src/index/reader.rs:281-287` (Confidence: 70%) -- The `search()` method allocates four separate `HashMap`s per query (`doc_scores`, `doc_field_tfs`, `doc_positions`, `doc_meta_cache`). For small indexes this is fine, but for large corpuses the repeated hashing and allocation could become a bottleneck. A single struct per document (keyed once) would reduce hash table overhead.

- **`bm25_score` (non-F variant) is gated behind `#[cfg(test)]` but still compiled into the format module** - `crates/rskim-search/src/index/format.rs:410-426` (Confidence: 60%) -- The old flat `bm25_score()` function and its constants (`BM25_K1`, `BM25_B`) are now test-only. This is a clean decomposition, but leaving the dead production function around (even behind `cfg(test)`) may confuse future readers about which scoring function is authoritative. Consider adding a comment noting it's retained only for backward-compatibility testing of the old formula.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Architecture Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

### Rationale

The overall architecture of this PR is solid. The new `lexical/` module follows strong separation of concerns: `config.rs` (pure data + validation), `scoring.rs` (pure math), and `classifier.rs` (AST-aware classification) are each single-responsibility. The format v2 codec changes are clean -- well-documented byte layouts, roundtrip-tested, with proper CRC32 integrity checks and clear v1 rejection messages.

Two HIGH issues prevent APPROVED status:

1. **Unnecessary direct dependency**: `tree-sitter` is declared in `rskim-search/Cargo.toml` but never directly imported. This is a dependency hygiene issue -- the transitive dependency via `rskim-core` is sufficient.

2. **Dead abstraction**: `FieldClassifier` + `NodeInfo` are publicly exported but have no production consumers. The actual classifier uses a different code path entirely. This creates two parallel APIs for field classification, which will confuse contributors and may drift apart over time. The recommendation is to either wire the trait into the real classifier or remove it until needed.

The MEDIUM issue (classification logic split across crates) is a coupling concern that should be addressed with documentation or a consistency assertion, but is not blocking.

The module layout, error handling (Result types throughout, `#[deny(unwrap_used)]`), and test coverage are all strong. The `SearchField` enum with stable `#[repr(u8)]` discriminants and compile-time assertions (`FIELD_COUNT == SearchField::ALL.len()`) is excellent defensive design for a binary format.
