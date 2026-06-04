# Rust Review Report

**Branch**: feat-183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25
**Files reviewed**: 5 (scoring.rs, scoring_tests.rs, types.rs, mod.rs, lib.rs)

## Issues in Your Changes (BLOCKING)

### MEDIUM

**`half_life_days` parameter naming misleads callers** - `scoring.rs:46-48`
**Confidence**: 82%
- Problem: The parameter is named `half_life_days` but the formula `exp(-t / half_life_days)` is actually a 1/e time constant (e-folding time), not a true half-life. A true half-life uses `exp(-t * ln(2) / half_life)` so that the value reaches 0.5 at `t = half_life`. At `t = half_life_days`, the current implementation returns ~0.368, not 0.5. The doc comment correctly says "~37%", so the behavior is documented -- but a caller who skips the doc and reads only the parameter name will expect 50% decay at 30 days.
- Fix: Either rename the parameter to `decay_constant_days` / `tau_days` to match the formula, or change the formula to a true half-life: `(-elapsed_days * std::f64::consts::LN_2 / half_life_days).exp().clamp(0.0, 1.0)`. The constant `DEFAULT_HALF_LIFE_DAYS` doc should be updated to match whichever approach is chosen.

**`debug_assert!` silently allows division by zero in release builds** - `scoring.rs:47, scoring.rs:81`
**Confidence**: 85%
- Problem: `debug_assert!(half_life_days > 0.0)` only fires in debug builds. In release, passing `half_life_days = 0.0` causes division by zero in the `exp(-elapsed / 0.0)` expression, producing `NaN` or `Inf` which `.clamp()` then maps to `NaN`. This means `compute_file_risk_scores` would return `NaN` scores silently. The project's CLAUDE.md states "debug_assert! for invariants in hot paths -- assert! at module boundaries." Since `compute_file_risk_scores` is a public API boundary (re-exported from lib.rs), an `assert!` or a guard returning early would be more appropriate.
- Fix: Add a runtime guard at the function boundary. For the public function, either:
  ```rust
  // Option A: assert at module boundary (per CLAUDE.md)
  assert!(half_life_days > 0.0, "half_life_days must be positive");
  ```
  or:
  ```rust
  // Option B: defensive fallback (avoids panic in library code)
  let half_life_days = if half_life_days > 0.0 { half_life_days } else { DEFAULT_HALF_LIFE_DAYS };
  ```
  Keep the `debug_assert!` in `decay_weight` since that is the hot-path inner function.

## Issues in Code You Touched (Should Fix)

_No issues found._

## Pre-existing Issues (Not Blocking)

_No issues found._

## Suggestions (Lower Confidence)

- **Unnecessary `into_owned()` allocation in hot loop** - `scoring.rs:112` (Confidence: 65%) -- `file.path_str().into_owned()` allocates a `String` for every file in every commit, even when the `Cow` is already `Borrowed`. Consider accumulating by `Cow<str>` or interning paths. However, this depends on how HashMap handles `Cow` keys, and the practical dataset sizes may make this negligible.

- **Parameter naming: `half_life_days` is a "decay constant"** - `scoring.rs:20` (Confidence: 70%) -- The constant `DEFAULT_HALF_LIFE_DAYS = 30.0` is described correctly in the doc ("~37%") but the name suggests a radiometric half-life (50% at t=half_life). This is a terminology precision issue that only matters when consumers build on top of this API.

- **Missing `#[must_use]` on `FileRiskScores` struct** - `types.rs:268` (Confidence: 62%) -- Feature knowledge notes "#[must_use] on all pub fns." Other data structs in `types.rs` do not carry `#[must_use]`, so this is consistent with codebase patterns -- but the struct is a computation result that should rarely be discarded.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code is well-structured: pure functions, no I/O, Entry API for single-probe HashMap updates, `#[must_use]` on public functions, comprehensive doc comments with examples, and thorough test coverage (30 tests across 6 groups). The exponential decay implementation is numerically sound with proper clamping. The two MEDIUM findings are: (1) the `half_life_days` parameter name slightly misrepresents the formula (documented correctly but naming is misleading), and (2) `debug_assert!` on a public API boundary should be an `assert!` or defensive guard per the project's own conventions.
