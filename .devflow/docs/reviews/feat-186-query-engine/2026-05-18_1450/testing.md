# Testing Review Report

**Branch**: feat/186-query-engine -> main
**Date**: 2026-05-18

## Issues in Your Changes (BLOCKING)

### HIGH

**Integration test `test_search_delegates_to_inner_layer` does not isolate the decorator from the real index** - `query_tests.rs:126-161`
**Confidence**: 85%
- Problem: This test constructs two independent `NgramIndexBuilder` instances on separate temp directories to compare results from `QueryEngine` vs the bare inner layer. This does not truly verify that `QueryEngine` delegates unchanged -- it verifies that two independently-built indexes from identical content produce the same results. If the inner layer were ever non-deterministic across builds (different temp dirs, file ordering, etc.), this test would be brittle. More importantly, it does not verify that `QueryEngine` passes the query **unchanged** to the inner layer, which is the stated decorator contract. A spy or recording wrapper around `SearchLayer` would directly prove delegation.
- Fix: Create a lightweight `SpyLayer` that records the query it receives and returns a fixed result. Assert the query passed through unchanged:
```rust
struct SpyLayer {
    received: std::sync::Mutex<Option<SearchQuery>>,
}

impl SearchLayer for SpyLayer {
    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        *self.received.lock().unwrap() = Some(query.clone());
        Ok(vec![SearchResult { file_id: FileId(99), score: 1.0, .. }])
    }
    fn name(&self) -> &str { "spy" }
}
// Assert: spy.received == original query, result == spy's canned response
```

### MEDIUM

**No test for `Infinity` BM25F values** - `query_tests.rs:84-98`
**Confidence**: 82%
- Problem: The tests cover `NaN` and negative `k1`, but `f32::INFINITY` and `f32::NEG_INFINITY` are also non-finite values that `BM25FConfig::validate()` rejects. While BM25F validation is owned by `config.rs` (and presumably tested there), the QueryEngine integration tests should confirm that all categories of invalid config are rejected at the decorator boundary, not just two specific cases. This is a gap in the validation integration coverage.
- Fix: Add a test with `k1 = f32::INFINITY`:
```rust
#[test]
fn test_infinity_bm25f_config_rejected() {
    let (_dir, engine) = build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    let mut query = SearchQuery::new("foo");
    let mut bad_config = BM25FConfig::default();
    bad_config.k1 = f32::INFINITY;
    query.bm25f_config = Some(bad_config);

    let result = engine.search(&query);
    assert!(matches!(result, Err(SearchError::InvalidQuery(_))));
}
```

**No test verifying empty query short-circuits without touching inner layer** - `query_tests.rs:31-35`
**Confidence**: 80%
- Problem: `test_empty_query_returns_empty_vec` asserts the return value is an empty `Vec`, but does not prove the inner layer was never called. The PR description explicitly states: "Empty `query.text` -> `Ok(vec![])` -- short-circuits without touching the inner layer." This contract is untested. With the current real-index setup, calling the inner layer with an empty string would also return `Ok(vec![])`, making this test unable to distinguish between short-circuit and delegation.
- Fix: Use a `PanicLayer` (an inner layer whose `search` panics if called) to prove the short-circuit:
```rust
struct PanicLayer;
impl SearchLayer for PanicLayer {
    fn search(&self, _query: &SearchQuery) -> Result<Vec<SearchResult>> {
        panic!("inner layer should not be called for empty queries");
    }
    fn name(&self) -> &str { "panic" }
}

#[test]
fn test_empty_query_short_circuits_inner_layer() {
    let engine = QueryEngine::new(Box::new(PanicLayer));
    let result = engine.search(&SearchQuery::new("")).unwrap();
    assert!(result.is_empty());
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`test_pagination_passes_through` silently skips on insufficient results** - `query_tests.rs:266-269`
**Confidence**: 82%
- Problem: The test has an early `return` at line 266-269 if `all_results.len() < 2`. This converts a potential failure (the index not returning enough results for the test data) into a silent pass. If the ngram index behavior changes such that "alpha" no longer matches all three files, this test would silently become a no-op. The PR description calls pagination pass-through an explicit contract ("all other queries forwarded unchanged").
- Fix: Replace the early return with `assert!(all_results.len() >= 2, ...)` so the test fails loudly if preconditions aren't met:
```rust
assert!(
    all_results.len() >= 2,
    "expected at least 2 results to test pagination, got {}",
    all_results.len()
);
```

## Pre-existing Issues (Not Blocking)

No pre-existing testing issues identified in the changed files.

## Suggestions (Lower Confidence)

- **Missing test for `valid` BM25F config pass-through** - `query_tests.rs` (Confidence: 70%) -- No test confirms that a valid custom `BM25FConfig` is forwarded to the inner layer and used in scoring. All BM25F tests only check rejection of invalid configs.

- **Heavy setup for simple validation tests** - `query_tests.rs:14-24` (Confidence: 65%) -- Every test, including pure validation tests (empty query, oversized query), builds a real `NgramIndexBuilder` with temp directory and file I/O. For the 6 Phase 1 validation tests, a lightweight mock/fake `SearchLayer` would be faster, simpler, and more focused. The current helper is 10 lines with 4 `.unwrap()` calls per test invocation.

- **No test for `temporal_flags` or `ast_pattern` pass-through** - `query_tests.rs` (Confidence: 62%) -- `SearchQuery` has `temporal_flags` and `ast_pattern` fields. While QueryEngine does not touch these fields, testing that they pass through unchanged would document the decorator's transparency contract for all query fields.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | - | 1 | 2 | - |
| Should Fix | - | - | 1 | - |
| Pre-existing | - | - | - | - |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

**Rationale**: The test suite is well-structured with clear phasing (validation, integration, edge cases), good naming, and reasonable coverage of the happy path and error conditions. The 15 tests cover the stated validation rules and several edge cases (unicode, whitespace, single-char, boundary length). However, the tests use only real index infrastructure and never verify the decorator's core contract -- that it delegates unchanged to the inner layer and short-circuits without calling it. The delegation test (`test_search_delegates_to_inner_layer`) tests index consistency across two independent builds rather than decorator transparency. Adding a spy/panic layer for 2-3 critical tests would make the suite significantly more robust and precisely aligned with the decorator pattern being implemented.
