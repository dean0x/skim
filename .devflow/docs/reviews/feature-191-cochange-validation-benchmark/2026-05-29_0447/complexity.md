# Complexity Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29T04:47Z

## Issues in Your Changes (BLOCKING)

### HIGH

**`sweep_thresholds` has 12 parameters** - `crates/rskim-bench/src/cochange/validate.rs:702-715`
**Confidence**: 95%
- Problem: `sweep_thresholds` accepts 12 parameters (4 input slices + 1 scratch set + 4 macro accumulator slices + 3 micro accumulator slices). This exceeds the complexity threshold of 5 parameters by a wide margin. The `#[allow(clippy::too_many_arguments)]` annotation suppresses the lint rather than addressing the root cause. While the prior review cycle decomposed `evaluate_at_thresholds` from 175 lines into helpers (good), the decomposition pushed all accumulator state into the parameter list instead of encapsulating it.
- Fix: Introduce an `EvalAccumulators` struct to bundle the 8 mutable accumulator slices (macro_precision_sum, macro_recall_sum, macro_commit_count, micro_tp, micro_predicted, micro_actual, micro_query_count, and the predicted_scratch set). This reduces the signature to 4 parameters: `(jaccard_cache, actual_sets, known_ids, thresholds, accumulators)`.

```rust
/// Mutable accumulators for threshold evaluation.
struct EvalAccumulators {
    predicted_scratch: HashSet<FileId>,
    macro_precision_sum: Vec<f64>,
    macro_recall_sum: Vec<f64>,
    macro_commit_count: Vec<usize>,
    micro_tp: Vec<usize>,
    micro_predicted: Vec<usize>,
    micro_actual: Vec<usize>,
    micro_query_count: Vec<usize>,
}

fn sweep_thresholds(
    jaccard_cache: &[Vec<(FileId, f64)>],
    actual_sets: &[HashSet<FileId>],
    known_ids: &[FileId],
    thresholds: &[f64],
    acc: &mut EvalAccumulators,
) { ... }
```

**`chrono_now` is 61 lines of manual Gregorian calendar arithmetic** - `crates/rskim-bench/src/bin/cochange_validate.rs:223-283`
**Confidence**: 85%
- Problem: This function reimplements Howard Hinnant's `civil_from_days` algorithm with 13 named constants and dense arithmetic. While the doc comment explains the intent ("avoid adding a chrono dependency"), the resulting function is the most cognitively demanding in the PR. A single off-by-one in any of the era/year/month/day calculations would produce silently wrong timestamps in the reproducibility manifest. The naming of constants is thorough, but the arithmetic itself (`doe - doe / DAYS_PER_4_YEARS + doe / DAYS_PER_100_YEARS - doe / DAYS_PER_ERA_MINUS_ONE`) / DAYS_PER_YEAR` is hard to audit for correctness.
- Fix: Consider using the `time` crate (already in the Rust ecosystem, lightweight) or `chrono` for formatting. If the zero-dependency constraint is firm, extract the Gregorian conversion into a standalone `unix_to_civil(secs: u64) -> (i64, i64, i64, u64, u64, u64)` function with its own focused unit tests that validate known epoch dates (not just format checks). The existing test only validates string format, not date correctness.

### MEDIUM

**`evaluate_at_thresholds` is 142 lines despite decomposition** - `crates/rskim-bench/src/cochange/validate.rs:200-341`
**Confidence**: 82%
- Problem: Even after the cycle-2 decomposition (from 175 lines), this function is still 142 lines. It initializes 8 parallel accumulator vectors (lines 236-244), loops over test commits with early-continue guards, delegates to helpers, then assembles metrics in a 35-line closure. The accumulator initialization and metrics assembly phases could be extracted to further reduce the function's cognitive load.
- Fix: Extract the metrics assembly (lines 303-339) into a `fn assemble_metrics(thresholds, macro_precision_sum, ...) -> Vec<ThresholdMetrics>` helper. Combined with the `EvalAccumulators` struct suggested above, this would bring `evaluate_at_thresholds` closer to 60-70 lines -- well within the 50-line ideal but acceptable for an orchestrator function.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`validate_repo` is 74 lines with 3 early-return error branches** - `crates/rskim-bench/src/cochange/validate.rs:352-425` (Confidence: 65%) -- The function orchestrates 4 phases with 3 separate `match ... Err => return Ok(error_result(...))` branches. Each branch has the same pattern. A macro or helper like `try_phase!(expr, entry, repo_name)` could reduce boilerplate, but the current form is readable and each branch is clearly documented.

- **`main` is 73 lines** - `crates/rskim-bench/src/bin/cochange_validate.rs:100-172` (Confidence: 62%) -- Slightly above the 50-line guideline for a `main()` function, but all it does is parse CLI, load config, process repos, render output, and optionally save. Each step is 3-5 lines. Not a real maintainability concern given the linear flow.

- **`aggregate_metrics` closure has 7 local accumulators** - `crates/rskim-bench/src/cochange/validate.rs:467-474` (Confidence: 60%) -- Similar accumulator-proliferation pattern as `sweep_thresholds` but scoped to a closure with only 7 variables. Borderline -- a struct would add clarity but the scope is small.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The codebase shows strong decomposition discipline -- the prior review cycle's work to break down `evaluate_at_thresholds` and `to_markdown` is evident and appreciated. Module boundaries are clean (deny_list, temporal_split, types, validate, report are each well-scoped). Function-level doc comments are thorough. The two HIGH findings are actionable: `sweep_thresholds`'s 12-parameter signature is a direct consequence of the decomposition pushing state outward instead of encapsulating it, and `chrono_now` carries audit risk from manual calendar arithmetic. Applies ADR-001 (fix noticed issues immediately). Avoids PF-002 (all findings surfaced for resolution, none deferred).
