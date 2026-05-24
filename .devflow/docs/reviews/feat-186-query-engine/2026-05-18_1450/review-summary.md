# Code Review Summary

**Branch**: feat/186-query-engine -> main
**Date**: 2026-05-18_1450

## Merge Recommendation: CHANGES_REQUESTED

The `QueryEngine` decorator implementation is architecturally sound and follows the project's design principles well. However, **2 HIGH issues block merge**: (1) integration testing uses brittle real-index comparison instead of isolation with mock layers, and (2) validation duplicate documentation is missing. Additionally, **5 MEDIUM consistency issues** in the test file deviate from established codebase conventions. These are all straightforward fixes that do not require architectural changes.

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| Blocking | 0 | 2 | 5 | 1 | **8** |
| Should Fix | 0 | 0 | 1 | 0 | **1** |
| Pre-existing | 0 | 0 | 3 | 0 | **3** |

---

## Blocking Issues (Must Fix Before Merge)

### HIGH (2)

#### 1. Integration Test Does Not Isolate Decorator from Real Index
- **File**: `crates/rskim-search/src/lexical/query_tests.rs:126-161`
- **Test**: `test_search_delegates_to_inner_layer`
- **Confidence**: 85%
- **Problem**: The test constructs two independent `NgramIndexBuilder` instances on separate temp directories to verify `QueryEngine` vs the bare inner layer produce the same results. This validates index consistency, not decorator transparency. It cannot distinguish between:
  1. QueryEngine passing the query unchanged (correct decorator contract)
  2. QueryEngine modifying the query in a way the bare layer also handles
  3. Both paths hitting the inner layer correctly by coincidence

  The PR description explicitly states the decorator must "forward unchanged" -- this contract is untested. A non-deterministic inner layer (e.g., randomized scoring or file ordering) would break this test without catching a real delegation bug.

- **Fix**: Create a lightweight `SpyLayer` that records what it receives and assert the query passed through unchanged:
  ```rust
  struct SpyLayer {
      received: std::sync::Mutex<Option<SearchQuery>>,
  }

  impl SearchLayer for SpyLayer {
      fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
          *self.received.lock().unwrap() = Some(query.clone());
          Ok(vec![SearchResult { file_id: FileId(99), score: 1.0 }])
      }
      fn name(&self) -> &str { "spy" }
  }

  #[test]
  fn test_delegates_query_unchanged() {
      let spy = Box::new(SpyLayer { received: Mutex::new(None) });
      let engine = QueryEngine::new(spy.clone());
      let query = SearchQuery::new("test");
      let _ = engine.search(&query);
      assert_eq!(spy.received.lock().unwrap().as_ref(), Some(&query));
  }
  ```

#### 2. Validation Overlap Between QueryEngine and NgramIndexReader Not Documented
- **File**: `crates/rskim-search/src/lexical/query.rs:46-63` (decorator) + `crates/rskim-search/src/index/reader.rs:310-327` (inner layer)
- **Confidence**: 85%
- **Problem**: Both `QueryEngine::search()` and `NgramIndexReader::search()` perform identical validation checks:
  - Empty query short-circuit (line 47 in query.rs, line 310 in reader.rs)
  - BM25F config validation (line 58 in query.rs, line 323 in reader.rs)
  
  This is intentional defense-in-depth for a generic decorator (it cannot assume the inner layer validates), but lacks documentation. A future maintainer might see the duplication and remove one thinking it is dead code, breaking the guarantee. Architecture reviewer flagged as HIGH layering concern.

- **Fix**: Add doc comment in `QueryEngine::search()` clarifying the intentional overlap:
  ```rust
  // Intentional defense-in-depth: validation runs at the decorator boundary
  // so QueryEngine's behavior is independent of the inner layer's implementation.
  // Even if the inner layer also validates, we validate first to fail fast.
  // This ensures the same contract regardless of which SearchLayer wraps this.
  ```

### MEDIUM (5) – Consistency Issues in Test File

All consistency issues are in `crates/rskim-search/src/lexical/query_tests.rs`. Existing sibling test files in the same crate (`config_tests.rs`, `scoring_tests.rs`, `classifier_tests.rs`) follow these patterns; this file breaks them.

#### 1. Explicit Imports Instead of Glob Import
- **Lines**: 5-8
- **Confidence**: 95%
- **Current**:
  ```rust
  use super::MAX_QUERY_BYTES;
  use crate::lexical::{BM25FConfig, QueryEngine};
  use crate::{FileId, LayerBuilder, SearchError, SearchLayer, SearchQuery};
  ```
- **Expected** (matching all 8 sibling test files):
  ```rust
  use super::*;
  use crate::index::NgramIndexBuilder;
  use crate::{FileId, LayerBuilder, SearchError, SearchLayer, SearchQuery};
  ```

#### 2. Section Dividers Use `====` Instead of `-----`
- **Lines**: 10, 28, 106, 202
- **Confidence**: 90%
- **Problem**: Source files (`query.rs`, `classifier.rs`) use `// ====...` but test files in the same directory (`config_tests.rs`, `scoring_tests.rs`) use `// -----...`. New test file inconsistently uses `====`.
- **Fix**: Replace all `// ============================================================================` with `// -----------------------------------------------------------------------`

#### 3. Error Assertion Pattern Inconsistent
- **Lines**: 43-51, 73-81, 94-97
- **Confidence**: 85%
- **Current**: Match on error variant:
  ```rust
  match result.unwrap_err() {
      SearchError::InvalidQuery(msg) => assert!(msg.contains(...)),
      other => panic!("unexpected error: {other}"),
  }
  ```
- **Expected** (matching 11 instances in `config_tests.rs`, 2 in `builder_tests.rs`):
  ```rust
  let msg = format!("{}", result.unwrap_err());
  assert!(msg.contains(&MAX_QUERY_BYTES.to_string()), "error should contain: {msg}");
  ```

#### 4. Module Doc Comment Not Updated
- **File**: `crates/rskim-search/src/lexical/mod.rs:1-13`
- **Confidence**: 90%
- **Problem**: The doc comment enumerates public exports but omits `QueryEngine` and `MAX_QUERY_BYTES`, which were added at lines 17 and 22.
- **Fix**: Update the doc comment to include:
  ```rust
  //! - [`QueryEngine`] — a [`SearchLayer`] decorator for query validation.
  //! - [`MAX_QUERY_BYTES`] — upper bound on query text length.
  ```

#### 5. Missing `#[must_use]` on `QueryEngine::new`
- **File**: `crates/rskim-search/src/lexical/query.rs:40`
- **Confidence**: 82%
- **Problem**: `SearchQuery::new` has `#[must_use]`; `QueryEngine::new` does not, despite being a pure constructor returning `Self`.
- **Fix**: Add `#[must_use]` above the constructor.

### MEDIUM (3) – Testing Issues

#### 1. Missing Test for Empty Query Short-Circuiting
- **File**: `crates/rskim-search/src/lexical/query_tests.rs:31-35`
- **Test**: `test_empty_query_returns_empty_vec`
- **Confidence**: 80%
- **Problem**: The test verifies the return value but not the contract "short-circuits without touching the inner layer." With the current real-index setup, calling the inner layer with empty string also returns `Ok(vec![])`, so this test cannot distinguish. The PR description explicitly requires this contract.
- **Fix**: Use a `PanicLayer` to prove short-circuit behavior:
  ```rust
  #[test]
  fn test_empty_query_short_circuits() {
      struct PanicLayer;
      impl SearchLayer for PanicLayer {
          fn search(&self, _: &SearchQuery) -> Result<Vec<SearchResult>> {
              panic!("should not be called for empty queries");
          }
          fn name(&self) -> &str { "panic" }
      }
      let engine = QueryEngine::new(Box::new(PanicLayer));
      let result = engine.search(&SearchQuery::new(""));
      assert!(result.is_ok() && result.unwrap().is_empty());
  }
  ```

#### 2. Missing Test for `Infinity` BM25F Config
- **File**: `crates/rskim-search/src/lexical/query_tests.rs`
- **Confidence**: 82%
- **Problem**: Tests cover `NaN` and negative values but not `f32::INFINITY`, another non-finite value rejected by validation. The testing suite should comprehensively show all invalid-config categories are caught at the decorator boundary.
- **Fix**: Add test with `k1 = f32::INFINITY`.

#### 3. Silent Failure Skip in `test_pagination_passes_through`
- **File**: `crates/rskim-search/src/lexical/query_tests.rs:266-269`
- **Confidence**: 82%
- **Problem**: Early `return` converts test failure into silent pass. If the index changes and returns fewer results, the test becomes a no-op rather than failing loudly.
- **Fix**: Replace early return with `assert!(all_results.len() >= 2, "expected at least 2 results, got {}", all_results.len());`

---

## Should Fix (1 Issue)

### MEDIUM – Module Docs Drift

**File**: `crates/rskim-search/src/lexical/mod.rs:1-13`
**Confidence**: 90%
**Problem** (same as blocking issue #4 above): The module-level doc comment lists public exports but does not mention the new `QueryEngine` and `MAX_QUERY_BYTES`. This creates drift between documented and actual API surface.
**Fix**: Include the new items in the enumerated list.

---

## Pre-existing Issues (Not Blocking)

### 1. Duplicated Validation in Inner Layer
- **File**: `crates/rskim-search/src/index/reader.rs:310-327`
- **Severity**: MEDIUM
- **Confidence**: 80%
- **Note**: This is addressed by the HIGH blocking issue #2 above (documentation fix). The validation duplication is intentional defense-in-depth and is documented.

### 2. Unbounded `limit`/`offset` Not Validated
- **File**: `crates/rskim-search/src/lexical/query.rs` (borderline -- not in changed lines)
- **Severity**: MEDIUM
- **Confidence**: 60%
- **Note**: `QueryEngine` does not validate `limit`/`offset` ranges. However, the inner layer clamps these safely, so this is informational. Flag for future Wave 4 input validation hardening.

### 3. `SearchQuery::Deserialize` Has Unbounded `text` Field
- **File**: `crates/rskim-search/src/` (pre-existing)
- **Severity**: MEDIUM
- **Confidence**: 65%
- **Note**: If a future HTTP endpoint deserializes `SearchQuery` directly, an attacker could exhaust memory before `QueryEngine` validation runs. Informational: mitigated by the current in-process-only architecture. Flag for the network layer design in future waves.

---

## Summary by Reviewer

| Reviewer | Score | Finding | Recommendation |
|----------|-------|---------|-----------------|
| Security | 9/10 | No blocking issues; excellent boundary validation | APPROVED |
| Architecture | 8/10 | Decorator pattern sound; one HIGH layering concern (documentation) | APPROVED_WITH_CONDITIONS |
| Performance | 10/10 | No performance impact; validation is negligible | APPROVED |
| Complexity | 10/10 | Clean, minimal surface area; cyclomatic complexity 4 | APPROVED |
| Consistency | 7/10 | 5 MEDIUM test-file deviations from crate conventions | APPROVED_WITH_CONDITIONS |
| Regression | 10/10 | No lost functionality; all exports preserved | APPROVED |
| Reliability | 9/10 | All five Power of Ten rules followed | APPROVED |
| Testing | 7/10 | Good structure but brittle integration test; missing isolation | CHANGES_REQUESTED |
| Rust | 9/10 | Safe code, no unsafe blocks; minor lint consistency | APPROVED |

---

## Recommended Action Plan

1. **Fix blocking HIGH issues** (0.5 hour):
   - Replace `test_search_delegates_to_inner_layer` with spy/mock-based delegation test
   - Add doc comment to `QueryEngine::search()` explaining intentional validation duplication

2. **Fix MEDIUM consistency issues** (0.5 hour):
   - Align test imports, section dividers, and error patterns with sibling test files
   - Update `mod.rs` doc comment
   - Add `#[must_use]` to `QueryEngine::new`

3. **Fix MEDIUM testing issues** (0.5 hour):
   - Add `PanicLayer` test for empty-query short-circuit
   - Add test for `f32::INFINITY` config validation
   - Replace silent skip with loud assertion in pagination test

4. **Re-review** (0.25 hour):
   - Confirm all issues addressed
   - Re-run test suite and lints

---

## Key Strengths

- **Solid architectural foundation**: Decorator pattern correctly implements `SearchLayer` trait, follows Dependency Inversion Principle
- **Excellent security**: Boundary validation is defense-in-depth with correct byte-length checking, NaN/Infinity rejection, and Result-based error handling
- **No performance regression**: Validation is negligible overhead; no hot-path allocations
- **Backward compatible**: Pure additive change; all existing exports preserved
- **Clear intent**: Commits and PR description accurately match implementation

## Merge Blocker Clarification

The two HIGH issues are **not architectural problems** -- they are **clarity problems**. The code works correctly; it just needs:
1. Better test isolation to prove the decorator contract
2. A comment explaining why validation runs twice

Once these are addressed, this PR can merge with high confidence.
