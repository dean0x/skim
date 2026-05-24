# Regression Review Report

**Branch**: feat/186-query-engine -> main
**Date**: 2026-05-18
**Scope**: Incremental (4 commits since last review: 2a563b4..c4c3cef)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Delegation test does not assert all SearchQuery fields** - `query_tests.rs:219-234`
**Confidence**: 82%
- Problem: The rewritten `test_search_delegates_to_inner_layer` asserts `text`, `lang`, `limit`, and `offset` are forwarded unchanged, but `SearchQuery` has 7 fields total. Three fields are not checked: `ast_pattern`, `temporal_flags`, and `bm25f_config`. If a future change to `QueryEngine::search` accidentally strips or mutates one of these before forwarding, the test will not catch it.
- Impact: A regression in query forwarding for `ast_pattern`, `temporal_flags`, or `bm25f_config` would go undetected.
- Fix: Assert all remaining fields:
  ```rust
  assert_eq!(
      received.ast_pattern, original_query.ast_pattern,
      "QueryEngine must forward ast_pattern unchanged"
  );
  assert_eq!(
      received.temporal_flags, original_query.temporal_flags,
      "QueryEngine must forward temporal_flags unchanged"
  );
  assert_eq!(
      received.bm25f_config, original_query.bm25f_config,
      "QueryEngine must forward bm25f_config unchanged"
  );
  ```
  Alternatively, derive `PartialEq` for `SearchQuery` and assert `received == original_query` in a single check.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Error variant assertions weakened to Display string matching** - `query_tests.rs:112-116, 138-139, 164-165, 178-179`
**Confidence**: 80%
- Problem: The previous tests used `match result.unwrap_err() { SearchError::InvalidQuery(msg) => ... }` which asserted both the error variant and the message content. The refactored tests use `format!("{}", result.unwrap_err())` followed by `.contains()`, which only checks the Display output. If the error variant changes from `InvalidQuery` to a different variant that happens to produce matching Display text, the test would still pass -- masking a regression in error classification.
- Impact: A change from `SearchError::InvalidQuery` to another variant (e.g., `SearchError::Internal`) would not be caught if the Display output still contains the expected substring.
- Fix: Keep the `format!` + `contains()` pattern for message content checks, but add a variant-level assertion. For example:
  ```rust
  let err = result.unwrap_err();
  assert!(matches!(err, SearchError::InvalidQuery(_)), "expected InvalidQuery, got {err:?}");
  let msg = format!("{err}");
  assert!(msg.contains("k1"), "error message should mention k1: {msg}");
  ```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **SpyLayer Mutex poisoning on panic** - `query_tests.rs:31,37` (Confidence: 62%) -- `Mutex::lock().unwrap()` in SpyLayer will panic on poisoned lock. This is acceptable in tests (`#![allow(clippy::unwrap_used)]`) but could produce confusing cascading failures if a test panics mid-search. A minor concern given the test context.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Positive Regression Signals

1. **No public API changes** -- `QueryEngine`, `MAX_QUERY_BYTES`, and all export paths unchanged. Zero consumer impact.
2. **Test count increased** (15 -> 18) -- Three new tests: `test_empty_query_short_circuits_inner_layer` (PanicLayer proves no inner call), `test_infinity_bm25f_config_rejected`, `test_neg_infinity_bm25f_config_rejected`. All increase coverage.
3. **All 15 original test names preserved** -- No tests removed or renamed. Full behavioral continuity.
4. **Silent skip eliminated** -- `test_pagination_passes_through` previously silently returned if < 2 results; now fails with a clear assertion. This is strictly better.
5. **Delegation test improved** -- The SpyLayer approach directly verifies the forwarding contract rather than relying on result equality across two identical index builds (which was an indirect proxy). The new test is more focused and less brittle.
6. **`#[must_use]` on `QueryEngine::new`** -- Prevents callers from accidentally discarding the constructed engine. No breaking change; purely additive.
7. **Defense-in-depth comment** -- Documents intentional redundancy between decorator and inner layer validation. Protects against future "deduplication" refactors that could remove the safety net.
8. **Commit messages match implementation** -- All 4 commits accurately describe their changes (docs, test doubles, style alignment, Arc simplification).

### Conditions for Approval

1. Add assertions for `ast_pattern`, `temporal_flags`, and `bm25f_config` in `test_search_delegates_to_inner_layer` to ensure complete forwarding coverage.
2. Consider restoring error variant matching alongside the Display-based string checks in the validation tests.
