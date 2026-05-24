# Performance Review Report

**Branch**: feat/186-query-engine -> main
**Date**: 2026-05-18
**Scope**: Incremental (4 commits since last review)

## Issues in Your Changes (BLOCKING)

No blocking performance issues found.

## Issues in Code You Touched (Should Fix)

No should-fix performance issues found.

## Pre-existing Issues (Not Blocking)

No pre-existing performance issues found.

## Suggestions (Lower Confidence)

- **Heap allocation for error formatting** - `query.rs:56-59` (Confidence: 65%) -- The `format!` call on the error path allocates a `String` for every oversized query rejection. In a hot-path decorator that sits in front of every search call, a static error message or interned constant could avoid the allocation entirely. However, the rejection path is inherently rare (only triggers on queries > 4 KiB), so the practical impact is negligible. The current approach is fine for diagnostics.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 9/10
**Recommendation**: APPROVED

### Rationale

The `QueryEngine` decorator is well-designed from a performance perspective:

1. **Zero overhead on the happy path.** The validation checks (`is_empty()`, `.len() > MAX_QUERY_BYTES`, and BM25F config validate) are all O(1) operations with no allocation. The decorator adds only a few nanoseconds of branch-prediction-friendly comparisons before delegating to the inner layer. This is textbook cheap validation gating.

2. **Early rejection prevents expensive work.** Empty queries short-circuit immediately to `Ok(Vec::new())` without touching the inner index layer. Oversized queries are rejected before any I/O or index traversal. Invalid BM25F configs are caught before the scorer runs. This is defense-in-depth that also serves as a performance guard -- malformed queries never reach the ngram index.

3. **No unnecessary cloning.** The `search` method takes `&SearchQuery` by reference and forwards it unchanged to the inner layer. No `clone()` on the query in production code. The `SearchQuery` clone in the SpyLayer test double is test-only and correctly scoped.

4. **`Box<dyn SearchLayer>` is the right abstraction cost.** One vtable dispatch per search call is the correct trade-off for composable layers. The alternative (monomorphization via generics) would provide marginal gains on a method that already does significant I/O work downstream.

5. **`MAX_QUERY_BYTES = 4096` is a reasonable bound.** It caps the byte-length check, preventing degenerate queries from causing excessive ngram generation or memory pressure in the inner layer.

6. **Test improvements reduce unnecessary I/O.** The refactored `test_search_delegates_to_inner_layer` replaced two full `NgramIndexBuilder` + tempdir constructions with a lightweight `SpyLayer`, eliminating redundant disk I/O in the test suite. This is a meaningful test-suite performance improvement.
