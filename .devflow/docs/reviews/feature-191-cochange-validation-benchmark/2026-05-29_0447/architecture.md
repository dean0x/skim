# Architecture Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29T04:47Z

## Issues in Your Changes (BLOCKING)

### HIGH

**`sweep_thresholds` has 12 mutable slice parameters — exceeds reasonable function signature complexity** - `crates/rskim-bench/src/cochange/validate.rs:702-714`
**Confidence**: 85%
- Problem: `sweep_thresholds` accepts 12 parameters (4 data inputs, 1 scratch buffer, 7 mutable accumulator slices). While there is already an `#[allow(clippy::too_many_arguments)]` annotation acknowledging this, the function signature reveals a missing intermediate type. The accumulators form a cohesive concept ("threshold-level metric accumulators") that should be a struct. This is an ISP/SRP concern: callers must prepare and pass many loosely-typed slices that are implicitly index-aligned, rather than a single well-typed accumulator.
- Fix: Extract a `ThresholdAccumulators` struct that owns the 7 mutable accumulator vecs (`macro_precision_sum`, `macro_recall_sum`, `macro_commit_count`, `micro_tp`, `micro_predicted`, `micro_actual`, `micro_query_count`) and exposes methods like `accumulate_commit(ti, precision, recall, tp, predicted_len, actual_len)` and `into_metrics(thresholds) -> Vec<ThresholdMetrics>`. This reduces `sweep_thresholds` to 5 parameters (cache, actual_sets, known_ids, thresholds, accumulators) and encapsulates the invariant that all slices share the same length/index alignment.

```rust
struct ThresholdAccumulators {
    macro_precision_sum: Vec<f64>,
    macro_recall_sum: Vec<f64>,
    macro_commit_count: Vec<usize>,
    micro_tp: Vec<usize>,
    micro_predicted: Vec<usize>,
    micro_actual: Vec<usize>,
    micro_query_count: Vec<usize>,
}

impl ThresholdAccumulators {
    fn new(n: usize) -> Self { /* ... */ }
    fn accumulate_query(&mut self, ti: usize, tp: usize, predicted: usize, actual: usize, p: f64, r: f64) { /* ... */ }
    fn finalize_commit(&mut self, ti: usize, query_count: usize) { /* ... */ }
    fn into_metrics(self, thresholds: &[f64]) -> Vec<ThresholdMetrics> { /* ... */ }
}
```

### MEDIUM

**`cochange::report::to_json` signature diverges from sibling `report::to_json`** - `crates/rskim-bench/src/cochange/report.rs:20`
**Confidence**: 82%
- Problem: The existing bench `report::to_json` takes `(result, tuning: Option<&TuningResult>)` and bundles both into a combined JSON object. The new `cochange::report::to_json` takes only `(&CochangeValidationResult)`. While the PR description notes this was flagged previously as a valid design divergence (different domain, no tuning concept), the Markdown counterpart shows the same pattern divergence: existing `to_markdown(result, tuning)` vs new `to_markdown(result)`. This is architecturally fine since co-change has no tuning concept, but the module-level naming creates ambiguity when both `report` modules are in the same crate. A developer importing at the crate level sees two `report` modules with identically-named public functions but different signatures.
- Fix: No code change required — the module nesting (`cochange::report` vs top-level `report`) provides sufficient disambiguation. This is noted for awareness. If the crate grows a third benchmark domain, consider a trait-based report interface.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

### MEDIUM

**Duplicate timeout/kill/join pattern in `clone.rs`** - `crates/rskim-research/src/clone.rs:74-119` and `crates/rskim-research/src/clone.rs:128-169`
**Confidence**: 85%
- Problem: `git_run_with_timeout` and `git_output_with_timeout` share ~40 lines of identical structure: spawn, channel, background thread, recv_timeout, SIGKILL, join. The only difference is `child.wait()` vs `child.wait_with_output()`. This is the kind of duplication that leads to divergence when one copy gets a bug fix and the other doesn't (which already happened in the prior cycle — the `handle.join()` fix was applied to both, but required manual attention).
- Fix: Extract a generic `run_with_timeout<F, T>(cmd, label, timeout_secs, wait_fn: F) -> Result<T>` that parameterizes the wait strategy. The two public functions become thin wrappers.

```rust
fn run_with_timeout<F, T>(
    mut cmd: Command, label: &str, timeout_secs: u64, wait_fn: F,
) -> anyhow::Result<T>
where
    F: FnOnce(Child) -> std::io::Result<T> + Send + 'static,
    T: Send + 'static,
{ /* shared spawn/channel/timeout/kill/join logic */ }

pub fn git_run_with_timeout(cmd: Command, label: &str) -> anyhow::Result<bool> {
    let output = run_with_timeout(cmd, label, GIT_SUBPROCESS_TIMEOUT_SECS, |mut c| {
        c.wait().map(|s| s.success())
    })?;
    // ...
}
```

## Suggestions (Lower Confidence)

- **`chrono_now` reinvents date formatting** - `crates/rskim-bench/src/bin/cochange_validate.rs:223-283` (Confidence: 70%) — 60 lines of Gregorian calendar arithmetic to avoid a `time` crate dependency. The `time` crate is lightweight (no-std compatible, no allocations) and already used transitively by several workspace dependencies. Worth considering if this binary grows more date logic.

- **`validate_repo` converts all errors to `Ok(error_result)` — error details may be lost** - `crates/rskim-bench/src/cochange/validate.rs:352-425` (Confidence: 65%) — The orchestrator catches `Err` from `clone_and_parse` and `build_and_evaluate` and converts to `Ok(RepoCochangeResult { error: Some(msg) })`. This is intentional (one broken repo should not abort the run), but the error chain is flattened to a string via `format!("{e:#}")`. If the caller later needs structured error classification (e.g., "was it a clone timeout vs a matrix build failure?"), the string representation is insufficient. Currently acceptable since the report just displays it.

- **`EvalResult` is a private struct without documentation** - `crates/rskim-bench/src/cochange/validate.rs:538-544` (Confidence: 62%) — Minor: `EvalResult` is undocumented, while every other type in this module has doc comments. Adding a one-liner keeps consistency.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

The cochange validation benchmark is architecturally sound. Key strengths:

1. **Clean module decomposition** — The `cochange/` module mirrors the existing bench crate pattern (types, report, validate, split) with focused responsibilities per file. Each module has a clear single purpose. (applies ADR-001 — the module structure was built right the first time rather than deferring organization.)

2. **Correct layering** — Domain logic (`validate.rs`, `temporal_split.rs`, `deny_list.rs`) has no knowledge of CLI or output formatting. The binary (`cochange_validate.rs`) is a thin CLI shell that delegates to library functions. Report generation (`report.rs`) depends only on types, not on the pipeline.

3. **Dependency direction** — `rskim-bench` depends on `rskim-search` (for `CochangeMatrixBuilder/Reader`, `CommitInfo`, `FileId`) and `rskim-research` (for `clone_with_history`, `RepoEntry`). No reverse dependencies. The `rskim-search` types are used as-is without leaky abstractions.

4. **Bounded concurrency** — The rayon pool is explicitly capped at 3 threads with a dedicated `ThreadPoolBuilder`, isolating it from the global pool. All computation bounds (MAX_FILES_FOR_EVALUATION, MAX_TEST_COMMITS, MAX_FILES_PER_COMMIT) are documented with rationale.

5. **Graceful degradation** — `validate_repo` converts errors to soft failures so one broken repo does not abort the entire benchmark run. Quality gates prevent degenerate repos from skewing aggregate metrics.

6. **Test-utils feature gating** — Test helpers are behind `#[cfg(any(test, feature = "test-utils"))]`, preventing test infrastructure from leaking into production builds while sharing helpers between unit and integration tests.

The one blocking HIGH issue is the `sweep_thresholds` 12-parameter function, which should be refactored into a struct-based accumulator to improve maintainability and reduce the risk of index-alignment bugs. The pre-existing timeout helper duplication in `clone.rs` (avoids PF-002 — surfacing rather than deferring) should be addressed while the code is fresh.
