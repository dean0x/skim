# Rust Review Report

**Branch**: feat/183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25T18:30
**Cycle**: 2 (incremental after 11/11 Cycle-1 fixes applied)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**NaN `half_life_days` not guarded in `decay_weight`** - `crates/rskim-search/src/temporal/scoring.rs:58`
**Confidence**: 82%
- Problem: The NaN guard added at line 61 sanitizes `elapsed_days` but does not sanitize `half_life_days`. If a caller passes `NaN` for `half_life_days`, the division `(-elapsed / NaN)` produces `NaN`, and `NaN.exp()` yields `NaN`, which `clamp()` does **not** catch (NaN comparisons are always false). The `debug_assert!(half_life_days > 0.0)` on line 58 also silently passes for NaN in debug builds (`NaN > 0.0` is `false`, triggering the assert in debug -- so it panics in debug but in release builds with `debug_assert` stripped, NaN would propagate). The public entry point `compute_file_risk_scores` uses `assert!` which catches `0.0` and negative values but also fires on NaN (since `NaN > 0.0` is false), so this is partially mitigated. However, `decay_weight` is a standalone `pub` function that callers can invoke directly.
- Fix: Add the same NaN guard pattern used for `elapsed_days`, or convert the `debug_assert!` to an explicit NaN check:
  ```rust
  pub fn decay_weight(elapsed_days: f64, half_life_days: f64) -> f64 {
      debug_assert!(half_life_days > 0.0 && !half_life_days.is_nan());
      let elapsed = if elapsed_days.is_nan() { 0.0 } else { elapsed_days };
      let hl = if half_life_days.is_nan() || half_life_days <= 0.0 {
          DEFAULT_HALF_LIFE_DAYS
      } else {
          half_life_days
      };
      (-elapsed / hl).exp().clamp(0.0, 1.0)
  }
  ```
  Alternatively, since `decay_weight` already documents a panic on `<= 0.0` in debug, the simplest symmetric fix is adding a NaN test to the existing test suite to document the expected behavior.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Capacity heuristic may under-allocate for few-commit cases** - `crates/rskim-search/src/temporal/scoring.rs:114` (Confidence: 65%) -- When `commits.len()` is small (e.g., 4 commits each touching 10 unique files), `commits.len() / 4` yields 1, which clamps to 64. The 64-entry floor handles this well in practice. However, the comment says "5-20x fewer" which is a heuristic assumption about real-world data; with synthetic or narrow inputs the ratio may differ. Not actionable unless profiling shows resize overhead.

- **`debug_assert!` vs `assert!` asymmetry between `decay_weight` and `compute_file_risk_scores`** - `crates/rskim-search/src/temporal/scoring.rs:58,99` (Confidence: 72%) -- `decay_weight` uses `debug_assert!` (stripped in release) while `compute_file_risk_scores` uses `assert!` (always fires) for the same `half_life_days > 0.0` precondition. The test `compute_scores_zero_half_life_panics` validates the `assert!` path, and `decay_zero_half_life_panics` is gated behind `#[cfg(debug_assertions)]`. This is intentional per the doc comments ("Panics in debug builds") and per the Cycle-1 fix that upgraded the public boundary to `assert!`. The asymmetry is documented but may surprise future maintainers who expect uniform behavior from the same precondition.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Condition

The single MEDIUM finding (NaN `half_life_days` in the public `decay_weight` API) is a narrow edge case since the main entry point `compute_file_risk_scores` already asserts positivity. Acceptable to merge as-is if the team considers the `debug_assert!` panic sufficient for direct `decay_weight` callers; otherwise add a one-line NaN guard or a test documenting the behavior.

### Positive Observations

- Cycle-1 fixes are correctly applied: module doc uses `//!`, public boundary uses `assert!`, hot-loop avoids `into_owned()`, NaN guard on `elapsed_days`, derive additions on `FileRiskScores`.
- Code is clean, pure, and well-documented with thorough doc comments including naming notes and algorithm overview.
- 34 tests pass covering decay math, edge cases (NaN, Infinity, negative timestamps, future timestamps), determinism, and fix-density specifics.
- Clippy passes with zero warnings under `deny(unwrap_used, expect_used, panic)`.
- No `unsafe`, no `unwrap`/`expect` outside `#[cfg(test)]`, no allocations in the hot loop for seen paths (Cow optimization).
- `#[must_use]` on both public functions.
- Capacity heuristic with clamp bounds prevents both under- and over-allocation.
