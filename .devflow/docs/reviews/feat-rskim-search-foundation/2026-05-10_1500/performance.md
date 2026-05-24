# Performance Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10T15:00

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**SearchResult::match_positions uses Vec<Range<usize>> -- potential allocation per result** - `crates/rskim-search/src/types.rs:145`
**Confidence**: 80%
- Problem: `SearchResult` contains `match_positions: Vec<Range<usize>>` and `snippet: Option<String>`. When search returns many results (hundreds or thousands), each result allocates a separate `Vec` and possibly a `String` on the heap. For a high-throughput search layer returning ranked results, this creates allocation pressure proportional to `results * avg_positions_per_result`.
- Impact: At the foundation-types level this is a design-time concern, not a runtime bug. The trait signature `fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>>` returns an owned `Vec<SearchResult>`, so every query allocates N results each with their own inner `Vec`. For the stated <50ms target on 1000-line files, this is unlikely to be a bottleneck for small result sets, but could become one at scale.
- Fix: This is a forward-looking consideration. The current types are correct for a v0.1 foundation. When implementing the actual search layers, consider:
  - A `SmallVec<[Range<usize>; 4]>` for `match_positions` (most matches have few positions)
  - A builder pattern that pre-allocates the results `Vec` based on the `limit` field
  - An arena allocator for batch result construction if profiling shows allocation as a hotspot

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **SearchQuery::new allocates via Into<String>** - `crates/rskim-search/src/types.rs:116` (Confidence: 65%) -- `SearchQuery::new(text: impl Into<String>)` always takes ownership. For callers that already have a `&str` this forces allocation. A `Cow<'a, str>` could avoid it, but this adds lifetime complexity that may not be justified at v0.1.

- **tree-sitter dependency in rskim-search may not be needed yet** - `crates/rskim-search/Cargo.toml:15` (Confidence: 70%) -- `tree-sitter` is listed as a dependency but is only used for the `FieldClassifier` trait signature (`tree_sitter::Node<'_>`). Since no implementations exist yet, this pulls in a non-trivial C compilation dependency that increases build times. It could be feature-gated or deferred until the first classifier is implemented.

- **Edition 2024 if-let chaining: no performance delta but watch codegen** - (multiple files, 52 clippy fixes) (Confidence: 60%) -- The bulk of this PR is mechanical if-let chain flattening (edition 2024). These are semantically equivalent transformations with no runtime cost difference. No concern here -- just noting for completeness that the generated MIR should be identical.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 9/10
**Recommendation**: APPROVED

## Rationale

This PR is fundamentally a **foundation-types crate** (`rskim-search`) plus a **mechanical edition migration** (2024 if-let chaining) and a **thiserror 2.0 upgrade**. From a performance perspective:

1. **New types are well-designed for performance**: `FileId` is `Copy` (u32 newtype), `SearchField` is `Copy` (fieldless enum), `TemporalFlags` is small and stack-allocated. The trait design cleanly separates the mutable build phase (`LayerBuilder`) from the immutable query phase (`SearchLayer: Send + Sync`), enabling lock-free concurrent queries.

2. **No I/O in the library crate**: The `rskim-search` crate is explicitly documented as pure types/traits with no I/O. All file access is deferred to the CLI layer. This is the correct architecture for a performance-sensitive search library.

3. **Edition 2024 changes are zero-cost**: The if-let chain transformations across ~30 files are semantically identical to the nested-if patterns they replace. The Rust compiler generates the same control flow.

4. **thiserror 2.0 upgrade**: No performance-relevant changes. thiserror 2.0 uses the same proc-macro derive approach with improved diagnostics.

5. **The one MEDIUM finding** (Vec allocation in SearchResult) is a design-time consideration for when search layers are implemented, not a current performance issue. The type layout is reasonable for v0.1.

No blocking performance issues. The architecture makes correct performance-oriented decisions (Copy types, Send+Sync traits, no I/O in library). Approved.
