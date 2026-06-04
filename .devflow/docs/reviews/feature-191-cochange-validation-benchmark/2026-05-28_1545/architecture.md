# Architecture Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-28T15:45:00Z

## Issues in Your Changes (BLOCKING)

### HIGH

**Deny-list pattern duplication between binary and module** - `cochange_validate.rs:238-262`, `deny_list.rs:14-45`
**Confidence**: 95%
- Problem: The `deny_list_pattern_names()` function in the binary (`cochange_validate.rs:238-262`) manually enumerates the same 21 patterns that are defined as constants in `deny_list.rs` (`DENIED_FILENAMES`, `DENIED_DIRS`, `DENIED_EXTENSIONS`). These two sources of truth will inevitably drift apart when patterns are added or removed. This is a DRY / Single Source of Truth violation.
- Fix: Expose a public function from `deny_list.rs` that returns the pattern names (e.g., `pub fn pattern_names() -> Vec<String>`) by reading from the existing constants. Replace the `deny_list_pattern_names()` function in the binary with a call to the module's function.

```rust
// In deny_list.rs — add:
#[must_use]
pub fn pattern_names() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    names.extend(DENIED_FILENAMES.iter().map(|s| s.to_string()));
    names.extend(DENIED_DIRS.iter().map(|s| format!("{s}/")));
    names.extend(DENIED_EXTENSIONS.iter().map(|s| format!("*.{s}")));
    names
}

// In cochange_validate.rs — replace deny_list_pattern_names() with:
use rskim_bench::cochange::deny_list;
let deny_list_patterns = deny_list::pattern_names();
```

**Inaccurate `chrono_now()` timestamp generation** - `cochange_validate.rs:207-222`
**Confidence**: 90%
- Problem: The `chrono_now()` function hand-rolls a timestamp using `secs / 86400 / 365` for year calculation. This ignores leap years and produces an incorrect year for most dates (the function is off by ~13 days per year since 1970, meaning by 2026 it accumulates ~200 days of drift). The year, month, and day are approximated or omitted (`XX-XX`), producing output like `2026-XX-XXT15:45:00Z` which is not a valid ISO-8601 timestamp despite `RunMetadata::timestamp` being documented as "ISO-8601 timestamp." This undermines the reproducibility manifest.
- Fix: Use `std::process::Command` to call `date -u +%Y-%m-%dT%H:%M:%SZ` (available on all target platforms), or add a minimal time formatting function that properly handles epoch-to-date conversion. Alternatively, since this is a benchmark binary (not a library hot path), adding a `time` or `chrono` dependency is justified for correctness.

```rust
fn chrono_now() -> String {
    // Shell out to `date` — acceptable for a benchmark binary's one-time call.
    std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
```

### MEDIUM

**`validate_repo` function is a 170-line god function with 9 sequential steps** - `validate.rs:321-494`
**Confidence**: 82%
- Problem: `validate_repo` orchestrates cloning, history parsing, deny-list filtering, quality gating, temporal splitting, path map building, matrix building, reader opening, and threshold evaluation -- all in one function. Each step has its own error-to-`Ok(error_result(...))` pattern (repeated 6 times). While the comments clearly mark each step, this function does too much for a single unit: it has 9 distinct responsibilities and the error-wrapping pattern is heavily duplicated. This is an SRP concern and increases the effort required to test or modify any individual step.
- Fix: Extract the clone-and-parse phase (steps 1-3) and the build-and-evaluate phase (steps 6-9) into helper functions. Introduce a macro or helper for the repetitive `error_result` wrapping pattern.

```rust
// Example: extract a helper to reduce error-wrapping boilerplate
macro_rules! try_or_error_result {
    ($expr:expr, $entry:expr, $name:expr, $msg:expr) => {
        match $expr {
            Ok(v) => v,
            Err(e) => return Ok(error_result($entry, $name, format!("{}: {e:#}", $msg))),
        }
    };
}
```

**Aggregate micro metrics averaged by count of repos, not by total queries** - `validate.rs:531-590`
**Confidence**: 85%
- Problem: `aggregate_metrics` computes the "micro" aggregate by averaging per-repo micro precision/recall values (`mip_sum / count`). This is mathematically a macro-average of micro metrics, not a true micro-average. A correct micro-average would sum TP/FP/FN across all repos and compute precision/recall from those totals. The field names `micro_precision` and `micro_recall` in `ThresholdMetrics` are therefore misleading at the aggregate level -- the values they hold at the aggregate level are not computed with the same methodology as at the per-repo level.
- Fix: Either (a) rename the aggregate-level computation to clarify it is a macro-average-of-micro, or (b) propagate raw TP/predicted/actual counts from per-repo metrics so the aggregate can compute true micro-averages. Option (b) is architecturally cleaner since it preserves the semantic meaning of "micro" across both levels.

```rust
// Option (b): Add raw counts to ThresholdMetrics
pub struct ThresholdMetrics {
    // ... existing fields ...
    /// Raw true-positive count (for micro aggregation across repos).
    pub micro_tp: usize,
    /// Raw predicted-set size (for micro aggregation across repos).
    pub micro_predicted_total: usize,
    /// Raw actual-set size (for micro aggregation across repos).
    pub micro_actual_total: usize,
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`clone_with_history` shares security hardening but not the clone timeout** - `clone.rs:295-323`
**Confidence**: 80%
- Problem: The new `clone_with_history` function correctly reuses the HTTPS-only check and `credential.helper=` / `transfer.fsckObjects=true` security args. However, it does a full clone (no `--depth 1`) which is significantly slower. The function delegates to `git_run_with_timeout` (good), but the timeout constant is shared with shallow clones. For large repos like `pydantic` (20k+ commits), a full clone may approach or exceed the shared timeout. The function also doesn't pass `--single-branch` which would reduce clone time by skipping non-default branches.
- Fix: Consider adding `--single-branch` to reduce clone size, and verify the shared timeout is sufficient for the largest repos in the corpus.

## Pre-existing Issues (Not Blocking)

(none at CRITICAL severity)

## Suggestions (Lower Confidence)

- **`evaluate_at_thresholds` has O(Q * F_all * T) complexity** - `validate.rs:222-240` (Confidence: 70%) -- For each query file, the function iterates over ALL known file IDs (`all_file_ids`) to find predictions. For repos with thousands of files, this is O(Q * F) per threshold. Since `CochangeMatrixReader` stores sorted pairs, the function could instead use `pairs_for_file(query_id)` to get only non-zero Jaccard partners (typically sparse), reducing work dramatically. The docstring at line 18-21 explicitly notes this was a deliberate choice to avoid O(n) prefix scans, but the current approach is already O(F) per query.

- **`RepoCochangeResult` has 15 public fields -- approaching data-clump territory** - `types.rs:46-81` (Confidence: 65%) -- The struct mixes identity fields (url, name, sha), split metadata (train/test counts, timestamps), evaluation results (metrics, quality gate), and error state. Grouping into nested sub-structs (e.g., `SplitInfo`, `EvaluationResult`) would improve clarity.

- **`aggregate_metrics` uses positional index matching** - `validate.rs:546-547` (Confidence: 75%) -- The function matches per-repo `ThresholdMetrics` to aggregate thresholds via `.get(ti).filter(|m| (m.threshold - threshold).abs() < 1e-9)`. This requires that every repo's `metrics_by_threshold` vector is in exactly the same order and length as the `thresholds` parameter. If a repo ever produces metrics in a different order or skips a threshold, the aggregation silently drops that repo's data. A `HashMap<OrderedFloat<f64>, ThresholdMetrics>` lookup would be more robust.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | - | 0 | 1 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Architecture Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The overall architecture is sound: clean module separation (types/deny_list/temporal_split/validate/report), correct layering (bench crate depends on search crate, not the reverse), well-documented public APIs, and proper use of the Strategy pattern for output formats. The main concerns are the deny-list duplication (a DRY violation that will cause divergence, applies ADR-001), the inaccurate timestamp that undermines reproducibility claims, and the micro-metric averaging methodology mismatch at the aggregate level. The `validate_repo` orchestrator function is large but follows a clear pipeline pattern -- extracting helpers would improve testability. All findings are in new code added by this PR (avoids PF-002 -- no pre-existing issues suppressed).
