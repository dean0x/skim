# Reliability Review Report

**Branch**: feat-183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Release-build silent degradation with zero `half_life_days`** - `scoring.rs:47,81` (Confidence: 70%) -- Both `decay_weight` and `compute_file_risk_scores` guard `half_life_days > 0.0` with `debug_assert!`, which is stripped in release builds. A zero value would produce `exp(+inf)` clamped to `1.0` for positive elapsed times, and `NaN` for zero elapsed (0.0/0.0). The feature knowledge documents this as an intentional design choice ("debug_assert! on half_life_days > 0.0 ... zero half-life silently produces exp(+inf) clamped to 1.0"). Since these are internal library functions with no public API boundary exposed to untrusted input, `debug_assert!` is consistent with the project's Rust conventions (`debug_assert!` for invariants in hot paths, `assert!` at module boundaries). If this function is ever promoted to a public boundary where callers pass user-controlled values, upgrading to `assert!` or returning `Result` would be warranted.

- **String allocation per file per commit in hot loop** - `scoring.rs:112` (Confidence: 65%) -- `file.path_str().into_owned()` allocates a new `String` for every file in every commit, even when the entry already exists in the accumulator. For repositories with thousands of commits touching the same files, this creates many short-lived allocations that are immediately dropped after the `HashMap::entry()` lookup. In practice, git histories are bounded and this is unlikely to be a bottleneck, but a `Cow`-keyed map or pre-interning paths could eliminate redundant allocations if profiling shows this as hot.

- **`fix_flags` Vec mirrors commit slice** - `scoring.rs:88-91` (Confidence: 60%) -- The `fix_flags: Vec<bool>` pre-classifies all commits, duplicating the commit count as a separate allocation. This is a sound optimization (avoids repeated regex evaluation in the inner loop), but for very large commit slices the allocation is proportional to input size. The `min(50_000)` cap on the HashMap does not apply here. Not a practical concern for typical git histories, but worth noting for completeness.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 9/10
**Recommendation**: APPROVED

## Rationale

This is an exceptionally well-bounded pure-computation module with strong reliability properties:

1. **Bounded iteration** -- Single pass over the input commit slice with no loops, retries, or recursion that could diverge. The inner loop over `changed_files` is bounded by the slice length. The determinism test loop is fixed at 49 iterations.

2. **Assertion density** -- `debug_assert!` guards on the critical `half_life_days > 0.0` invariant in both public functions. Division-by-zero guards (`max_total > 0.0`, `total > f64::EPSILON`) protect normalization arithmetic. Negative timestamps are clamped to 0. Future commits are handled explicitly (elapsed = 0).

3. **Allocation discipline** -- HashMap capacity is pre-allocated with a `min(50_000)` cap preventing excessive reservation from large commit counts. The `fix_flags` Vec is proportional to input size but bounded by the caller's slice.

4. **No I/O, no side effects** -- Pure computation with deterministic output given deterministic input (tested by `deterministic_results` with 50 iterations). No file access, no network, no global state mutation.

5. **Numeric stability** -- `clamp(0.0, 1.0)` on decay weight output prevents out-of-range values. Very large elapsed times produce small but finite positive weights (tested by `decay_very_large_elapsed` and `very_old_commits`). NaN cannot arise from valid inputs because the `debug_assert!` catches zero half-life in development.

6. **Comprehensive edge-case testing** -- Tests cover: empty input, negative timestamps, future timestamps, very old commits (10,000 days), zero/all fix commits, monotonicity, range invariants, and parameter sensitivity. This test suite exercises the defensive bounds thoroughly.

The only items noted are suggestions at 60-70% confidence, all relating to hypothetical scenarios (zero half-life from a caller, allocation under extreme input sizes) rather than concrete bugs. The code follows project conventions (debug_assert for hot-path invariants per CLAUDE.md Rust rules) and the feature knowledge confirms these design choices are intentional.
