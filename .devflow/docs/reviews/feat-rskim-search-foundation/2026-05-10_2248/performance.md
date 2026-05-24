# Performance Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10

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

- **SearchResult struct size may matter in hot-path result vectors** - `crates/rskim-search/src/types.rs:159-172` (Confidence: 65%) — `SearchResult` contains two heap-allocating fields (`Vec<Range<usize>>` for `match_positions`, `Option<String>` for `snippet`). When `SearchLayer::search()` returns `Vec<SearchResult>`, each result carries its own allocations. For large result sets (thousands of matches), this creates allocation pressure. Consider whether `snippet` could use `Cow<'a, str>` or whether `match_positions` could use a `SmallVec` once the hot path materializes. Not actionable yet — the type has no runtime consumers, so there is nothing to measure.

- **NodeInfo clones a Range and &'static str — effectively zero-cost** - `crates/rskim-search/src/types.rs:241` (Confidence: 60%) — `NodeInfo` derives `Clone` with fields `&'static str` (16 bytes, Copy), `Range<usize>` (16 bytes, Copy), and `usize` (8 bytes, Copy). The clone is a 40-byte memcpy with no heap allocation. This is fine. Noted only to confirm it was checked.

- **SearchQuery::new allocates via Into<String>** - `crates/rskim-search/src/types.rs:138` (Confidence: 62%) — `SearchQuery::new(text: impl Into<String>)` will allocate when called with `&str`. In a future interactive search loop (keystroke-per-query), this allocation per query could add up. A `Cow<'_, str>` or borrowed lifetime on `SearchQuery` could avoid this, but the current design is idiomatic for a foundation crate with no callers yet.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Performance Score**: 9/10
**Recommendation**: APPROVED

### Rationale

This PR introduces pure type definitions, traits, and a CLI stub with no runtime I/O paths. There are no performance-impacting code paths to evaluate because nothing runs at query time yet. The foundation design is sound from a performance perspective:

1. **Zero-allocation field naming**: `SearchField::name()` returns `&'static str` by exhaustive match — explicitly documented as designed for hot-path BM25F weighting loops. Good.

2. **Copy-friendly core types**: `FileId` (4 bytes, Copy), `SearchField` (1 byte, Copy) are both `Copy` — no clone overhead when passing around in index structures.

3. **NodeInfo decouples from tree-sitter without overhead**: The `NodeInfo` adapter struct is 40 bytes of stack data (`&'static str` + `Range<usize>` + `usize`), all Copy types. The `from_ts_node` constructor reads three fields from a tree-sitter `Node` with no heap allocation. This is a clean abstraction boundary that costs nothing at runtime.

4. **Trait signatures are allocation-aware**: `SearchLayer::search()` returns `Vec<SearchResult>` (owned, single allocation for the vec backing), and `FieldClassifier::classify()` returns `SearchField` by value (Copy, zero-cost). `LayerBuilder::add_file()` borrows `&str` for content — no unnecessary copies.

5. **No I/O in library crate**: All I/O is correctly isolated in the CLI stub (`crates/rskim/src/cmd/search.rs`). The library crate is pure computation + types, matching the project's streaming-first architecture.

The three suggestions are forward-looking notes for when runtime consumers materialize and profiling becomes possible. None are actionable today.
