# Testing Review Report

**Branch**: feat-183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25T14:29

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Missing test for `compute_file_risk_scores` with zero `half_life_days` in debug mode** - `scoring_tests.rs`
**Confidence**: 85%
- Problem: `compute_file_risk_scores` contains `debug_assert!(half_life_days > 0.0)` at line 81 of `scoring.rs`, but there is no `#[should_panic]` test that exercises this path. The `decay_zero_half_life_panics` test (line 129) only covers `decay_weight`, not the outer function. In release mode, a zero half-life would produce `NaN` from `0.0/0.0` division, silently corrupting all scores.
- Fix: Add a `#[should_panic]` test for `compute_file_risk_scores` with `half_life_days = 0.0`:
```rust
#[test]
#[cfg(debug_assertions)]
#[should_panic]
fn compute_scores_zero_half_life_panics() {
    let commits = vec![make_commit(NOW, "feat", &["a.rs"])];
    let _ = compute_file_risk_scores(&commits, NOW, 0.0);
}
```

**Missing test for commit touching multiple files simultaneously** - `scoring_tests.rs`
**Confidence**: 82%
- Problem: Most tests use single-file commits or multi-commit-single-file scenarios. Only `hotspot_max_is_one` (line 209) and `all_files_have_valid_scores` (line 464) test commits with multiple files, but neither verifies that the same decay weight is correctly applied to all files within a single commit. This is a key behavior of the algorithm's inner loop (lines 111-118 of `scoring.rs`).
- Fix: Add a test that verifies a single commit touching N files gives all files identical scores:
```rust
#[test]
fn single_commit_multiple_files_same_weight() {
    let commits = vec![make_commit(NOW - 10 * DAY, "feat: wide change", &["a.rs", "b.rs", "c.rs"])];
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    assert_eq!(scores.len(), 3);
    let expected_hotspot = scores["a.rs"].hotspot;
    assert!(approx_eq(expected_hotspot, 1.0));
    assert!(approx_eq(scores["b.rs"].hotspot, expected_hotspot));
    assert!(approx_eq(scores["c.rs"].hotspot, expected_hotspot));
}
```

### MEDIUM

**`decay_weight` not tested with `f64::NAN` or `f64::INFINITY` inputs** - `scoring_tests.rs`
**Confidence**: 80%
- Problem: The edge case group (Group 5) tests future timestamps, negative timestamps, and very old timestamps, but `decay_weight` is a public API that accepts arbitrary `f64` values. There are no tests for `NaN` or `Infinity` as `elapsed_days`. While `compute_file_risk_scores` would not produce these values (it derives elapsed from integer timestamps), direct callers of `decay_weight` could pass them.
- Fix: Add boundary tests:
```rust
#[test]
fn decay_nan_elapsed_returns_nan() {
    let w = decay_weight(f64::NAN, HALF_LIFE);
    // NaN input produces NaN output (not clamped to a valid range)
    assert!(w.is_nan() || (w >= 0.0 && w <= 1.0));
}

#[test]
fn decay_infinity_elapsed_returns_zero() {
    let w = decay_weight(f64::INFINITY, HALF_LIFE);
    assert!(approx_eq(w, 0.0));
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Doc comment says "half-life" but formula implements e-folding time** - `scoring.rs:17-19,46-48`
**Confidence**: 80%
- Problem: The parameter is named `half_life_days` and the constant is `DEFAULT_HALF_LIFE_DAYS`, but the formula `exp(-elapsed/half_life)` gives weight `1/e` (~0.368) after one "half-life", not `0.5`. A true half-life would use `exp(-elapsed * ln(2) / half_life)`. The doc on line 18 correctly states "~37%", but the naming creates a semantic mismatch for anyone expecting standard half-life semantics.
- Fix: This is acknowledged in the documentation ("contribute ~37% as much weight"), so the tests are internally consistent. Consider either: (a) renaming the parameter to `decay_time_days` or `e_fold_days` for precision, or (b) adding a brief note in the doc comment: "Note: this is an e-folding time, not a true half-life (weight reaches 1/e, not 1/2, after one period)." No test changes needed since tests already verify against `E.recip()`.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Test count mismatch in PR/feature knowledge** (Confidence: 75%) -- The PR description and feature knowledge both claim "47 tests across 6 groups," but only 30 `#[test]` functions exist in `scoring_tests.rs` (31 including the doc test on `decay_weight`). This may cause confusion for future contributors or review tooling.

- **Consider property-based testing for invariants** - `scoring_tests.rs` (Confidence: 65%) -- The range invariant ([0,1] for both hotspot and fix_density) and monotonicity of `decay_weight` are properties that would benefit from `proptest` or `quickcheck` to generate random commit histories. The current tests use hand-crafted inputs that may miss surprising combinations.

- **Redundant range-check tests could be consolidated** (Confidence: 60%) -- `decay_always_in_unit_range`, `scores_in_unit_range`, `all_files_have_valid_scores`, and parts of `negative_timestamp` and `very_old_commits` all verify the same [0,1] range invariant. These overlap significantly. Consolidating into fewer parametric assertions would reduce noise without losing coverage.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The test suite is well-structured with 6 logical groups covering decay math, basic cases, acceptance criteria, fix density specifics, edge cases, and determinism. Tests follow Arrange-Act-Assert, use deterministic timestamps (NOW=1,700,000,000), and have a clean `make_commit` helper that avoids boilerplate. The `approx_eq` helper with EPSILON=1e-9 is appropriate for f64 comparison. The `#[should_panic]` test is correctly guarded by `#[cfg(debug_assertions)]`.

Conditions for approval:
1. Add a `#[should_panic]` test for `compute_file_risk_scores` with zero half-life (mirrors the existing `decay_zero_half_life_panics` test for `decay_weight`)
2. Add a test verifying that a single commit touching multiple files applies the same weight to all files
