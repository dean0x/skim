# Performance Review Report

**Branch**: feat/183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25T18:30

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

- **`fix_flags` Vec allocation could use `with_capacity`** - `scoring.rs:106` (Confidence: 65%) — The `fix_flags: Vec<bool>` is collected from an iterator over `commits`, so the iterator's `size_hint` already tells `collect()` the exact length. No manual `with_capacity` needed; the compiler handles this. Mentioning only for completeness — no action required.

- **Two-pass over accumulator (fold + into_iter)** - `scoring.rs:149-176` (Confidence: 60%) — The normalization step iterates the accumulator HashMap twice: once to find `max_total`, once to build final scores. For extremely large unique-file counts this could be fused into a single pass by tracking the max during accumulation. In practice, unique files are orders of magnitude fewer than commits, so the second pass is negligible. No action required unless profiling shows otherwise.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Performance Score**: 9/10
**Recommendation**: APPROVED

## Detailed Analysis

### What was reviewed

- `crates/rskim-search/src/temporal/scoring.rs` (177 lines) — core scoring algorithm
- `crates/rskim-search/src/temporal/scoring_tests.rs` (615 lines) — test suite
- `crates/rskim-search/src/types.rs` — `FileRiskScores` derive changes
- `crates/rskim-search/src/temporal/mod.rs` — `is_fix_commit` with LazyLock regex

### Performance strengths observed

1. **Single-pass accumulation**: `compute_file_risk_scores` iterates commits exactly once for the hot loop, with only a lightweight second pass over the (much smaller) accumulator for normalization. This is optimal for the problem.

2. **Allocation-efficient hot loop** (Cycle 1 fix confirmed): The `get_mut` probe before `into_owned()` at `scoring.rs:136-139` reduces String allocations from O(total_file_touches) to O(unique_files). This is the correct pattern for Cow-keyed HashMap accumulation.

3. **HashMap capacity heuristic** (Cycle 1 fix confirmed): `(commits.len() / 4).clamp(64, 50_000)` at `scoring.rs:114` is a reasonable estimate — unique files are typically 5-20x fewer than commits. The clamp prevents both under-allocation (floor 64) and runaway allocation (cap 50k).

4. **Pre-classified fix flags**: `fix_flags: Vec<bool>` at `scoring.rs:106-109` evaluates the LazyLock regex once per commit outside the per-file inner loop. This avoids redundant regex evaluation — each commit's fix status is O(1) to look up in the inner loop.

5. **`#[inline]` on `decay_weight`**: The function is small (one branch + exp + clamp) and called once per commit in the outer loop. Inlining eliminates call overhead and enables the compiler to optimize the exp() call site.

6. **`debug_assert!` vs `assert!` layering**: `decay_weight` uses `debug_assert!` (hot path, stripped in release) while `compute_file_risk_scores` uses `assert!` (module boundary, always fires). This is the correct Rust idiom per the project's engineering rules.

7. **`Copy` derive on `FileRiskScores`**: Adding `Copy` to a struct of two `f64` fields (16 bytes) eliminates Clone overhead — the struct is register-sized and benefits from pass-by-value semantics.

8. **NaN guard is branch-predicted**: The `elapsed_days.is_nan()` check at `scoring.rs:61` is a single comparison that will almost always be false. Branch prediction makes this effectively free on modern CPUs.

### Cross-Cycle Awareness

All 11 issues from Cycle 1 were verified as fixed in the current HEAD:
- Hot loop `into_owned()` allocation → replaced with `get_mut` first pattern
- HashMap capacity over-allocation → replaced with `/4` heuristic with clamp

No regressions detected from the fixes.
