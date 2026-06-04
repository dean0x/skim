# Reliability Review Report

**Branch**: feat/183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25T18:30

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`decay_weight` uses `debug_assert!` for `half_life_days` on a `pub` function** - `crates/rskim-search/src/temporal/scoring.rs:58`
**Confidence**: 92%
- Problem: `decay_weight` is a public function (exported via `mod.rs:17`) and validates its `half_life_days > 0.0` precondition with `debug_assert!`, which is stripped in release builds. In release mode, passing `half_life_days = 0.0` produces `(-elapsed / 0.0).exp()` which is `NaN` or `Inf`, and while `.clamp(0.0, 1.0)` catches `Inf`, it does NOT catch `NaN` (NaN comparisons are always false, so `clamp` passes NaN through). Passing a negative `half_life_days` also produces incorrect results silently. The sister function `compute_file_risk_scores` correctly uses `assert!` at line 99 (fixed in cycle 1), but `decay_weight` itself remains unguarded at the public boundary.
- Impact: Any caller invoking `decay_weight(x, 0.0)` in release mode gets NaN propagation — exactly the class of bug the NaN guard on `elapsed_days` (lines 61-65) was added to prevent. The function's own doc comment at line 41 says "Panics in debug builds" acknowledging this is debug-only, but the CLAUDE.md Rust rules say `assert! at module boundaries` and the feature knowledge confirms this was a deliberate fix for `compute_file_risk_scores`. The same rationale applies here.
- Fix:
  ```rust
  pub fn decay_weight(elapsed_days: f64, half_life_days: f64) -> f64 {
      assert!(half_life_days > 0.0, "half_life_days must be positive");
      // ... rest unchanged
  }
  ```
  Update the doc comment at line 41 from "Panics in debug builds" to "Panics when `half_life_days <= 0.0`." and update the test at line 126-131 to remove `#[cfg(debug_assertions)]` since it would now fire unconditionally.

### MEDIUM

(none)

## Issues in Code You Touched (Should Fix)

### HIGH

**NaN guard on `elapsed_days` but not on `half_life_days` in `decay_weight`** - `crates/rskim-search/src/temporal/scoring.rs:61-65`
**Confidence**: 85%
- Problem: The NaN guard explicitly handles `elapsed_days.is_nan()` (lines 61-65) to prevent NaN propagation. However, if `half_life_days` is NaN, the division `(-elapsed / NaN)` produces NaN, and `.exp()` of NaN is NaN, and `.clamp()` does not sanitize NaN. The `debug_assert!` at line 58 does not catch NaN because `NaN > 0.0` is false — it would panic in debug but only for NaN, not for the stated precondition reason. In release builds, NaN `half_life_days` silently propagates.
- Impact: Inconsistent defensive posture — one parameter is guarded against NaN, the other is not. If the function is called with computed `half_life_days` that turns out NaN due to upstream arithmetic, the NaN propagates into the accumulator HashMap.
- Fix: If upgrading to `assert!` (per the BLOCKING item above), add NaN to the check:
  ```rust
  assert!(
      half_life_days > 0.0 && half_life_days.is_finite(),
      "half_life_days must be positive and finite, got {half_life_days}"
  );
  ```
  This rejects NaN, Inf, zero, and negative values in a single assertion. Add a corresponding test:
  ```rust
  #[test]
  #[should_panic(expected = "half_life_days must be positive and finite")]
  fn decay_nan_half_life_panics() {
      let _ = decay_weight(1.0, f64::NAN);
  }
  ```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Capacity heuristic `commits.len() / 4` lower bound of 64 may over-allocate for tiny inputs** - `crates/rskim-search/src/temporal/scoring.rs:114` (Confidence: 65%) — For 1-10 commits, allocating 64 HashMap slots is wasteful (64 entries * ~80 bytes each). A `min(commits.len(), 64)` as the lower clamp instead of a fixed 64 would be more precise, though the memory impact is negligible in practice.

- **`fix_flags` Vec allocation could be avoided with iterator zipping** - `crates/rskim-search/src/temporal/scoring.rs:106-109` (Confidence: 62%) — The `fix_flags: Vec<bool>` pre-allocates a boolean per commit. For very large histories (50K+ commits), this is 50KB of allocation that could be avoided by computing `is_fix` inline during the main loop. However, the separation improves readability and the cost is trivial relative to the HashMap.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 1 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The overall reliability posture is strong — bounded allocations, NaN guards, future-timestamp clamping, negative-timestamp clamping, max_total == 0.0 division guard, and a proper `assert!` on the main entry point. The single gap is that the public `decay_weight` function uses `debug_assert!` instead of `assert!` for its precondition, creating an asymmetry with `compute_file_risk_scores` which was already upgraded in cycle 1. The NaN guard on `elapsed_days` is good but incomplete without matching coverage on `half_life_days`. Both issues are straightforward to fix.
