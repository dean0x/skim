# Consistency Review Report

**Branch**: feat/183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25T18:30

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Inconsistent assertion level for same precondition across sibling functions** - `scoring.rs:58`, `scoring.rs:99`
**Confidence**: 85%
- Problem: `decay_weight` uses `debug_assert!(half_life_days > 0.0)` (line 58, debug-only) while `compute_file_risk_scores` uses `assert!(half_life_days > 0.0, ...)` (line 99, all builds) for the identical precondition on the same parameter. Both are public API functions in the same module operating on the same domain constraint. The test at `scoring_tests.rs:174-183` explicitly documents this divergence, but the inconsistency means callers get different safety guarantees depending on which function they call directly.
- Fix: Either promote `decay_weight` to `assert!` for uniform boundary enforcement (matching the CLAUDE.md principle: "assert! at module boundaries"), or document a clear rationale why `decay_weight` intentionally uses the weaker guard (e.g., hot-path performance). The existing doc comment at line 41 says "Panics in debug builds" which is accurate for the current `debug_assert!`, but the project's Rust rule says to use `assert!` at module boundaries and `debug_assert!` only in hot paths. Since `decay_weight` is `#[inline]` and called in a tight loop, the current choice is defensible but should be explicitly documented as a performance exemption.

**Tuple positional access (`.0` / `.1`) instead of named destructuring** - `scoring.rs:141`, `scoring.rs:143`
**Confidence**: 82%
- Problem: The accumulator uses `entry.0 += w` and `entry.1 += w` for the `(weighted_total, weighted_fix_total)` tuple. The previous code used `let (weighted_total, weighted_fix_total) = accum.entry(path).or_insert(...)` with named destructuring, giving each field a meaningful name. The refactored Cow-based lookup lost the named bindings, leaving only positional `.0`/`.1` access. This reduces readability -- a reader must trace back to the `or_insert((0.0, 0.0))` call to know which field is which.
- Fix: Add a brief inline comment, or restore named access after the entry lookup:
  ```rust
  let (weighted_total, weighted_fix_total) = entry;
  *weighted_total += w;
  if is_fix {
      *weighted_fix_total += w;
  }
  ```
  Alternatively, a type alias `type Accum = (f64, f64);` with named constants would be even more consistent with the codebase's preference for explicit naming.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Duplicated "Naming note" doc blocks** - `scoring.rs:17-22` and `scoring.rs:34-37` (Confidence: 70%) -- The "Naming note" explaining the e-folding vs half-life distinction appears twice, nearly verbatim, on the constant and the function. Consider defining it once on the constant and cross-referencing from the function doc with a simple "See [`DEFAULT_HALF_LIFE_DAYS`] for naming rationale."

- **`capacity` heuristic uses magic fraction** - `scoring.rs:114` (Confidence: 65%) -- The `commits.len() / 4` divisor and `.clamp(64, 50_000)` bounds are undocumented magic numbers. The previous `commits.len().min(50_000)` was simpler. The new heuristic is reasonable but the `/ 4` factor has no inline justification for why 1/4 was chosen over 1/5 or 1/10.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code is well-structured and follows existing codebase patterns closely. Module doc style (`//!`), `#[must_use]`/`#[inline]` annotations, section separators, test organization, derive traits, and re-exports all match established conventions. The two MEDIUM findings are about internal consistency within the new module itself (assertion level divergence, positional tuple access) rather than divergence from the broader codebase. The assertion inconsistency is defensible as a hot-path optimization and is already documented in the test suite, but making the rationale explicit in the source would strengthen it.
