# Regression Review Report

**Branch**: feat/183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25T18:30

## Issues in Your Changes (BLOCKING)

No blocking regression issues found.

## Issues in Code You Touched (Should Fix)

No should-fix regression issues found.

## Pre-existing Issues (Not Blocking)

No pre-existing regression issues found.

## Suggestions (Lower Confidence)

- **`debug_assert!` vs `assert!` inconsistency in `decay_weight`** - `scoring.rs:58` (Confidence: 70%) — `compute_file_risk_scores` was upgraded from `debug_assert!` to `assert!` for `half_life_days > 0.0` (line 99), but `decay_weight` retains `debug_assert!` (line 58). Since `decay_weight` is a public function that callers can invoke directly, passing `half_life_days <= 0.0` in a release build would cause division by zero or incorrect results without any guard. This is a pre-existing asymmetry, but the PR widened the gap by intentionally hardening `compute_file_risk_scores` while leaving `decay_weight` soft. The test `decay_zero_half_life_panics` is `#[cfg(debug_assertions)]`, confirming the release build gap. Lower confidence because this is a deliberate design choice documented in the code (hot-path performance vs. boundary check), and `decay_weight` is always called from `compute_file_risk_scores` where the assert already fires.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED

## Detailed Analysis

### Regression Checklist

- [x] **No exports removed** — All public functions (`decay_weight`, `compute_file_risk_scores`, `DEFAULT_HALF_LIFE_DAYS`) and types (`FileRiskScores`) remain exported with identical signatures. `lib.rs` and `temporal/mod.rs` are unchanged.
- [x] **Return types backward compatible** — No function signatures changed. `decay_weight` and `compute_file_risk_scores` retain identical parameter lists and return types.
- [x] **Default values unchanged** — `DEFAULT_HALF_LIFE_DAYS` remains `30.0`.
- [x] **Side effects preserved** — These are pure functions with no side effects. No change.
- [x] **All consumers updated** — Zero external consumers found outside the module itself.
- [x] **Migration complete** — N/A (no API migration).
- [x] **CLI options preserved** — No CLI changes in this PR.
- [x] **Commit messages match implementation** — All 5 commits accurately describe their changes.
- [x] **Full test suite passes** — 392 tests pass, 0 fail, 3 skip (skips are pre-existing).

### Behavioral Changes Analyzed

1. **`decay_weight` NaN guard** (`scoring.rs:59-65`): New behavior — NaN `elapsed_days` now returns `1.0` instead of propagating NaN. This is a **robustness improvement**, not a regression. The prior behavior (NaN propagation) was a latent bug. New tests (`decay_nan_elapsed_does_not_propagate`, `decay_positive_infinity_elapsed`, `decay_negative_infinity_elapsed`) validate the edge cases.

2. **`compute_file_risk_scores` assert upgrade** (`scoring.rs:99`): Changed from `debug_assert!` to `assert!` for `half_life_days > 0.0`. This means release builds now panic on invalid input instead of silently producing garbage. This is an intentional hardening per the CLAUDE.md rule ("assert! at module boundaries"). The `compute_scores_zero_half_life_panics` test validates it fires in both debug and release builds. No external caller passes zero, so no regression.

3. **HashMap capacity heuristic** (`scoring.rs:114`): Changed from `commits.len().min(50_000)` to `(commits.len() / 4).clamp(64, 50_000)`. This is a pure performance optimization (reduces over-allocation). Does not change functional behavior — HashMap grows automatically if needed.

4. **Cow-based path deduplication** (`scoring.rs:131-144`): Replaced `file.path_str().into_owned()` with a borrow-first-then-own pattern. Functionally equivalent — same keys end up in the HashMap. Reduces allocations from O(total_file_touches) to O(unique_files).

5. **`FileRiskScores` derive additions** (`types.rs:268`): Added `Copy`, `PartialEq`, `Serialize`, `Deserialize` to existing `Debug, Clone`. All additive traits — strictly expand the API surface, cannot break existing code. `Copy` is safe because the struct contains only two `f64` fields.

6. **Doc comment style change** (`scoring.rs:1-11`): Converted `///` to `//!` module-level doc comments. Purely cosmetic — affects rustdoc rendering location but not behavior.

### Intent vs. Reality Verification

The PR description states: "Adds per-file hotspot and bug-fix density metrics to the temporal module. All new additions are additive public API — no breaking changes."

This matches the implementation. All changes are either:
- Additive (new derive traits, new tests, new NaN guard)
- Performance improvements (capacity heuristic, Cow optimization)
- Documentation improvements (e-folding clarification, module docs)

No breaking changes detected.
