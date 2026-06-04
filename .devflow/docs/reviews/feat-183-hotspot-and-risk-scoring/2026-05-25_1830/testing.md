# Testing Review Report

**Branch**: feat/183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25T18:30

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Missing NaN half_life_days test for decay_weight** - `crates/rskim-search/src/temporal/scoring_tests.rs`
**Confidence**: 85%
- Problem: The PR added NaN sanitization for `elapsed_days` in `decay_weight` (lines 61-65 of scoring.rs), and tests for NaN/Infinity `elapsed_days` inputs. However, there is no test for `decay_weight(1.0, f64::NAN)` -- NaN in the `half_life_days` parameter. The `debug_assert!` on line 58 only fires in debug builds. In release mode, `decay_weight(1.0, f64::NAN)` produces `exp(-1.0 / NaN) = exp(NaN) = NaN`, which `clamp(0.0, 1.0)` does not sanitize (NaN fails all comparisons). This is an asymmetry: `elapsed_days` has NaN protection, `half_life_days` does not. The production function `compute_file_risk_scores` guards with `assert!(half_life_days > 0.0)` which correctly rejects NaN (NaN fails `> 0.0`), so the end-to-end path is safe. But `decay_weight` is public API and can be called directly without that guard.
- Fix: Add a test that documents the current behavior for NaN `half_life_days` in debug builds (panic via debug_assert), and consider whether release builds should also sanitize this parameter or document the precondition more explicitly:
```rust
/// `decay_weight` with NaN half_life_days panics in debug builds (debug_assert).
#[test]
#[cfg(debug_assertions)]
#[should_panic]
fn decay_nan_half_life_panics_debug() {
    let _ = decay_weight(1.0, f64::NAN);
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`all_files_have_valid_scores` test weakened by removing type-assertion lines without adding equivalent coverage** - `crates/rskim-search/src/temporal/scoring_tests.rs:550-561`
**Confidence**: 80%
- Problem: The diff shows the old test had `let _hotspot: f64 = s.hotspot; let _fix_density: f64 = s.fix_density;` lines (type assertions ensuring the struct fields exist and are f64) which were removed and replaced with range assertions (`assert!(s.hotspot >= 0.0 && ...)`). The range assertions are strictly stronger than the type bindings (they implicitly confirm the type AND check the range), so the test is not functionally weaker. However, the PR also added `PartialEq` to `FileRiskScores` (in types.rs) which means the old type-assertion pattern is no longer needed for compile-time guarantees. This is fine -- the change is correct. Downgrading this from a real issue to a note: no action needed. The new assertions are superior.
- Fix: No action required. The range assertions are strictly better.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**No test for negative `half_life_days` in `compute_file_risk_scores`** - `crates/rskim-search/src/temporal/scoring_tests.rs`
**Confidence**: 82%
- Problem: `compute_file_risk_scores` asserts `half_life_days > 0.0` which correctly rejects both zero and negative values, but only the zero case is tested (`compute_scores_zero_half_life_panics`). A test for negative `half_life_days` (e.g., -30.0) would confirm the assert fires for that case too.
- Fix:
```rust
#[test]
#[should_panic(expected = "half_life_days must be positive")]
fn compute_scores_negative_half_life_panics() {
    let commits = vec![make_commit(NOW, "feat", &["a.rs"])];
    let _ = compute_file_risk_scores(&commits, NOW, -30.0);
}
```

### LOW

**`make_commit` helper truncates `u64` to `i64` without overflow guard** - `crates/rskim-search/src/temporal/scoring_tests.rs:24`
**Confidence**: 80%
- Problem: `make_commit` takes `ts: u64` and casts to `i64` on line 25 (`timestamp: ts as i64`). If a test passes a `u64` value larger than `i64::MAX`, the cast silently wraps to a negative number. All current test values are well within range (around 1.7 billion), so this is not a bug today, but it is a footgun for future test authors.
- Fix: Consider using `i64::try_from(ts).expect("timestamp overflow")` or changing the parameter to `i64`.

## Suggestions (Lower Confidence)

- **No property-based tests for decay_weight** - `scoring_tests.rs` (Confidence: 65%) -- `decay_weight` is a pure function with a simple contract (output in [0,1], monotonically decreasing, finite). This is an ideal candidate for property-based testing with `proptest` or `quickcheck` to verify the invariants hold across random input space, beyond the hand-picked test vectors.

- **No integration test exercising `compute_file_risk_scores` with realistic commit volume** - `scoring_tests.rs` (Confidence: 70%) -- All tests use small commit sets (5-50 commits). A test with hundreds or thousands of commits would exercise the HashMap capacity heuristic (`commits.len() / 4`) and confirm performance characteristics hold. This is more relevant now that the capacity formula changed from `commits.len().min(50_000)` to `(commits.len() / 4).clamp(64, 50_000)`.

- **`decay_always_in_unit_range` could be parameterized** - `scoring_tests.rs:100-123` (Confidence: 62%) -- The test uses a fixed array of 10 input pairs. A parameterized or property-based approach would provide better coverage of the input space.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 1 | 1 |

**Testing Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

The test suite is well-structured with clear grouping (6 logical groups), good naming conventions describing expected behavior, consistent AAA structure, and deterministic design (fixed timestamps, no I/O). The new tests added in this PR (NaN/Infinity boundary tests, should_panic for zero half_life, multi-file weight sharing) fill important gaps identified in Cycle 1 review.

The single blocking MEDIUM issue is the asymmetric NaN handling between `elapsed_days` (sanitized + tested) and `half_life_days` (debug_assert only, no test). Since `compute_file_risk_scores` guards with `assert!` that catches NaN, the end-to-end risk is mitigated, but the public `decay_weight` API has undocumented release-mode NaN behavior.

**Condition for approval**: Add a test documenting `decay_weight` behavior with NaN `half_life_days` (either the should_panic debug test, or explicit NaN sanitization + test).
