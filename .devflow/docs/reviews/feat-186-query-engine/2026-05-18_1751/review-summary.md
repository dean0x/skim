# Code Review Summary

**Branch**: feat/186-query-engine -> main
**Date**: 2026-05-18_1751
**Reviewers**: 9 (architecture, complexity, consistency, performance, regression, reliability, rust, security, testing)

## Merge Recommendation: CHANGES_REQUESTED

**Reason**: Two MEDIUM blocking issues in test assertions require fixes before merge. Both are straightforward completions of existing test contracts. Code architecture and implementation quality are excellent (9/10 average across all reviewers).

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| Blocking | 0 | 0 | 2 | 0 | 2 |
| Should Fix | 0 | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 | 0 |

---

## Blocking Issues (Must Fix Before Merge)

### 1. Delegation Test Omits 3 of 7 SearchQuery Fields

**Location**: `crates/rskim-search/src/lexical/query_tests.rs:207-234`
**Severity**: MEDIUM
**Confidence**: 95% (3 reviewers independently flagged; consistency 85%, regression 82%, testing 90%)

**Problem**:
`test_search_delegates_to_inner_layer` asserts the query is forwarded "unchanged" (line 210 comment), but only verifies 4 of 7 fields: `text`, `lang`, `limit`, `offset`. The three missing fields are `ast_pattern`, `temporal_flags`, and `bm25f_config`.

If a future change accidentally strips or mutates one of these fields before forwarding to the inner layer, this test will not catch it.

**Fix**:
Add assertions for the three missing fields:

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
    format!("{:?}", received.bm25f_config),
    format!("{:?}", original_query.bm25f_config),
    "QueryEngine must forward bm25f_config unchanged"
);
```

(Use Debug formatting for `bm25f_config` since it contains `f32` fields without `PartialEq`)

---

### 2. Error Variant Assertions Weakened to Display String Matching

**Location**: `crates/rskim-search/src/lexical/query_tests.rs:112-116, 138-139, 164-165, 178-179`
**Severity**: MEDIUM
**Confidence**: 80% (regression reviewer flagged)

**Problem**:
Previous tests used `match result.unwrap_err() { SearchError::InvalidQuery(msg) => ... }` which asserted both the error variant AND the message. The refactored tests use `format!("{}", result.unwrap_err())` + `.contains()`, which only validates the Display output.

If an error variant changes from `SearchError::InvalidQuery` to a different variant (e.g., `SearchError::Internal`) that happens to produce matching Display text, the test will still pass -- masking a regression in error classification.

**Affected tests**:
1. `test_invalid_bm25f_config_rejected_before_search` (line 112-116)
2. `test_nan_bm25f_config_rejected` (line 138-139)
3. `test_infinity_bm25f_config_rejected` (line 164-165)
4. `test_neg_infinity_bm25f_config_rejected` (line 178-179)

**Fix**:
Restore error variant matching alongside the Display-based string checks:

```rust
let err = result.unwrap_err();
assert!(matches!(err, SearchError::InvalidQuery(_)), 
    "expected InvalidQuery variant, got {err:?}");
let msg = format!("{err}");
assert!(msg.contains("k1"), "error message should mention k1: {msg}");
```

This validates both the variant and the message content.

---

## Suggestions (Lower Confidence — Informational)

| Item | Location | Confidence | Note |
|------|----------|------------|------|
| PR description / code mismatch | `query.rs:15` | 70% | PR says "64KB default, configurable" but code uses 4096 (4 KiB) const. Not a code bug; update external docs. |
| Oversized-query short-circuit test | `query_tests.rs:106-117` | 65% | Consider adding `PanicLayer`-based test to prove this path short-circuits, matching the pattern for empty queries. |
| Populated optional fields test | `query_tests.rs:213` | 70% | Current delegation test uses all-None optional fields. A second test with populated fields (ast_pattern, temporal_flags, bm25f_config set) would strengthen coverage. |
| Arc<SpyLayer> impl pattern | `query_tests.rs:46-54` | 65% | Test-only workaround for orphan rule. A newtype wrapper would be more idiomatic, but this is low priority (test code only). |

---

## Cross-Reviewer Consensus

**9 reviewers evaluated this branch:**

| Focus Area | Score | Recommendation | Key Findings |
|-----------|-------|-----------------|--------------|
| Architecture | 9/10 | APPROVED | Decorator pattern correctly applies Open/Closed Principle; clean dependency direction; defense-in-depth comment documents intent |
| Complexity | 9/10 | APPROVED | Production code: 17-line `search()` method with cyclomatic complexity 4. Test improvements reduced complexity via SpyLayer/PanicLayer |
| Consistency | 8/10 | APPROVED_WITH_CONDITIONS | Alignment with codebase conventions is excellent; blocking issue is test assertion completeness |
| Performance | 9/10 | APPROVED | Zero overhead on happy path; early rejection prevents expensive work; test refactoring improved test-suite performance |
| Regression | 9/10 | APPROVED_WITH_CONDITIONS | Public API unchanged; test count increased; two blocking issues identified in test assertions |
| Reliability | 9/10 | APPROVED | Bounded iteration; strong assertion density at trust boundary; allocation discipline correct |
| Rust | 9/10 | APPROVED | Ownership/borrowing correct; error handling via `?` operator; type-driven design with `#[must_use]` |
| Security | 9/10 | APPROVED | Input validation boundary strong; defense-in-depth good; no injection vectors or secret leakage |
| Testing | 8/10 | APPROVED_WITH_CONDITIONS | Test doubles (SpyLayer, PanicLayer) properly used; edge cases covered well; blocking issue is field coverage gap |

**Consensus**: 6 unconditional approvals, 3 conditional approvals, 0 change requests outside the blocking issues.

---

## Positive Signals

1. **Architecture**: Decorator pattern correctly encapsulates validation without modifying inner layers. Single responsibility maintained.
2. **Test Quality**: Shift from concrete index setups to test doubles (SpyLayer, PanicLayer) improves test focus and eliminates unnecessary I/O.
3. **Edge Case Coverage**: NaN, Infinity, NEG_INFINITY, boundary values, unicode, whitespace, pagination all covered.
4. **No Public API Changes**: Backwards compatible; zero consumer impact.
5. **Commit Hygiene**: 4 commits with clear, accurate messages. No test removals; test count increased (15 → 18).
6. **Defense-in-Depth**: Inline comment documents intentional redundancy between decorator and inner layer validation.
7. **`#[must_use]` Convention**: Applied correctly to constructor; prevents accidental discards.

---

## Action Plan

1. **Add assertions for `ast_pattern`, `temporal_flags`, `bm25f_config`** in `test_search_delegates_to_inner_layer` (line 207-234)
2. **Restore error variant matching** in the four BM25F validation tests (lines 112-116, 138-139, 164-165, 178-179)
3. **Optional improvements** (lower priority):
   - Add PanicLayer test for oversized-query short-circuit (mirrors empty-query test pattern)
   - Add delegation test variant with populated optional fields
   - Update PR description to clarify 4KB, non-configurable (or update code if aspiration is different)

---

## Summary

This is a strong, well-designed increment with excellent architecture and test coverage. The two MEDIUM blocking issues are straightforward assertions completeness gaps — the code itself is correct, but the test contracts need completion. Once assertions are added, this PR is mergeable. No architecture, security, performance, or reliability concerns. Expected merge after fixes: **APPROVED**.
