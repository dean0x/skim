# Complexity Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-28T15:45

## Issues in Your Changes (BLOCKING)

### HIGH

**`validate_repo` function is 168 lines with high cyclomatic complexity (9 sequential error-handling branches)** - `crates/rskim-bench/src/cochange/validate.rs:321-493`
**Confidence**: 85%
- Problem: `validate_repo` is the top-level per-repo orchestrator and spans 168 lines (lines 321-493). It contains 9 sequential match/if-let error-handling arms, each returning `Ok(error_result(...))`. While each arm is individually simple, the aggregate function length exceeds the 50-line warning threshold by 3x and makes the pipeline hard to follow. The cyclomatic complexity is approximately 11 (9 error arms + 2 conditional paths).
- Fix: The function is already well-documented and follows a numbered pipeline. Consider extracting logical sub-steps. For example, a `clone_and_parse` helper could handle steps 1-3 (clone, parse, deny-list filter) and a `build_and_evaluate` helper could handle steps 6-9 (matrix build, reader open, evaluate). Each would return a `Result` and the orchestrator would be reduced to ~50 lines. Example:

```rust
fn clone_and_parse(entry: &RepoEntry, corpus_dir: &Path) -> anyhow::Result<Result<(String, Vec<CommitInfo>), RepoCochangeResult>> {
    let repo_name = /* ... */;
    let dest = corpus_dir.join(&repo_name);
    if let Err(e) = clone_with_history(&entry.url, &dest) {
        return Ok(Err(error_result(entry, &repo_name, format!("clone failed: {e:#}"))));
    }
    let head_sha = capture_head_sha(&dest).unwrap_or_else(|_| "unknown".to_string());
    let history = match GixSource.parse_history(&dest, 0) {
        Ok(h) => h,
        Err(e) => return Ok(Err(error_result(entry, &repo_name, format!("parse_history failed: {e:#}")))),
    };
    let mut commits = history.commits;
    for c in &mut commits { filter_denied(&mut c.changed_files); }
    Ok(Ok((head_sha, commits)))
}
```

**`evaluate_at_thresholds` is 145 lines with 4 levels of nesting in the hot loop** - `crates/rskim-bench/src/cochange/validate.rs:165-310`
**Confidence**: 82%
- Problem: The function spans 145 lines with a 4-level nested loop structure: `for commit` > `for ti in thresholds` > `for query_id` > `for candidate_id`. The inner loop body (lines 224-240) contains a match with 4 arms. This is algorithmically necessary (O(T * C * Q * F) where T=thresholds, C=commits, Q=queries, F=files) but the nesting depth makes it hard to reason about which accumulator variables are scoped where. There are 10 accumulator vectors managed across the nested scopes.
- Fix: Extract the inner per-query evaluation into a helper function. This reduces nesting by one level and makes the accumulator logic easier to follow:

```rust
struct QueryResult {
    precision: f64,
    recall: f64,
    true_positives: usize,
    predicted_count: usize,
    actual_count: usize,
}

fn evaluate_query(
    reader: &CochangeMatrixReader,
    query_id: FileId,
    known_ids: &[FileId],
    all_file_ids: &[FileId],
    threshold: f64,
) -> anyhow::Result<QueryResult> {
    // Build predicted and actual sets, compute metrics
    // ...
}
```

### MEDIUM

**`RepoCochangeResult` struct has 14 fields** - `crates/rskim-bench/src/cochange/types.rs:46-81`
**Confidence**: 83%
- Problem: The struct has 14 public fields, which exceeds the 5-parameter warning threshold for function parameters. While this is a data struct (not a function parameter list), constructing it inline requires repeating all 14 fields at 5 call sites: `validate_repo` (2 sites), `error_result`, and 2 test helpers. The `error_result` helper already exists to reduce this pain but still manually lists all 14 fields with zero values.
- Fix: Add a `Default` implementation (or derive it) and use struct update syntax to reduce the boilerplate:

```rust
impl Default for RepoCochangeResult {
    fn default() -> Self {
        Self {
            repo_url: String::new(),
            repo_name: String::new(),
            head_sha: "unknown".to_string(),
            // ... all zero/empty defaults
        }
    }
}

// Then error_result becomes:
fn error_result(entry: &RepoEntry, repo_name: &str, error: String) -> RepoCochangeResult {
    RepoCochangeResult {
        repo_url: entry.url.clone(),
        repo_name: repo_name.to_string(),
        error: Some(error),
        ..Default::default()
    }
}
```

**`deny_list_pattern_names` duplicates the deny-list constants** - `crates/rskim-bench/src/bin/cochange_validate.rs:238-262`
**Confidence**: 85%
- Problem: The `deny_list_pattern_names()` function in the binary manually reconstructs the deny-list as a `Vec<String>` with 21 entries that must stay in sync with the three const arrays in `deny_list.rs` (`DENIED_FILENAMES`, `DENIED_DIRS`, `DENIED_EXTENSIONS`). If someone adds a new deny pattern to `deny_list.rs`, this function will silently become stale. This is a maintainability issue that increases the cost of future changes.
- Fix: Expose the deny-list constants from `deny_list.rs` as public and build the pattern list programmatically:

```rust
// In deny_list.rs:
pub const DENIED_FILENAMES: &[&str] = &[ /* ... */ ];
pub const DENIED_DIRS: &[&str] = &[ /* ... */ ];
pub const DENIED_EXTENSIONS: &[&str] = &[ /* ... */ ];

// In cochange_validate.rs:
fn deny_list_pattern_names() -> Vec<String> {
    let mut patterns: Vec<String> = Vec::new();
    patterns.extend(deny_list::DENIED_FILENAMES.iter().map(|s| s.to_string()));
    patterns.extend(deny_list::DENIED_DIRS.iter().map(|s| format!("{s}/")));
    patterns.extend(deny_list::DENIED_EXTENSIONS.iter().map(|s| format!("*.{s}")));
    patterns
}
```

**`full_pipeline_synthetic_repo` integration test is 136 lines with 5 early-return bail-out points** - `crates/rskim-bench/tests/cochange_validation.rs:295-430`
**Confidence**: 80%
- Problem: This test function spans 136 lines and contains 5 separate `return` bail-outs (git not available, repo init fails, parse_history fails, too few commits, test split empty). Each bail-out silently skips the test with an eprintln message, which means CI could silently pass without actually running the test. The function also mixes infrastructure setup (git commit creation loop) with assertion logic.
- Fix: Extract the git repo creation into a helper function and consider using `#[ignore]` or a macro to mark environment-dependent tests rather than silent skips. Also consider splitting setup into a reusable fixture builder:

```rust
fn build_synthetic_cochange_repo() -> Option<(TempDir, Vec<CommitInfo>)> {
    if !git_available() { return None; }
    let dir = init_git_repo()?;
    // ... create commits ...
    // ... parse history ...
    Some((dir, history.commits))
}

#[test]
fn full_pipeline_synthetic_repo() {
    let Some((dir, commits)) = build_synthetic_cochange_repo() else {
        eprintln!("SKIPPED: synthetic repo setup failed");
        return;
    };
    // ... pure evaluation logic ...
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`chrono_now` produces approximate timestamps** - `crates/rskim-bench/src/bin/cochange_validate.rs:207-222` (Confidence: 65%) -- The hand-rolled timestamp function uses `secs / 86400 / 365` for year calculation which drifts from real dates (no leap year handling) and fills month/day with "XX". For a reproducibility manifest this is surprising. Consider using the `time` or `chrono` crate, or at minimum outputting the raw Unix timestamp alongside.

- **`aggregate_metrics` uses index-based threshold alignment** - `crates/rskim-bench/src/cochange/validate.rs:544-548` (Confidence: 70%) -- The function aligns per-repo metrics to aggregate thresholds by vector index (`get(ti)`) with a floating-point epsilon check as a safety net. If a repo's threshold list ever differs in length or ordering from the aggregate's, the index-based lookup silently skips it. This is fragile but currently safe because all repos use the same threshold list.

- **10 accumulator vectors in `evaluate_at_thresholds`** - `crates/rskim-bench/src/cochange/validate.rs:183-191` (Confidence: 72%) -- The function manages 10 parallel `Vec` accumulators indexed by threshold. A struct like `ThresholdAccumulator` grouping all 5 macro/micro fields would reduce the cognitive overhead of tracking which `vec[ti]` goes with which metric.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 6/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new cochange validation benchmark module is well-structured with clear separation of concerns across 5 submodules (`types`, `deny_list`, `temporal_split`, `validate`, `report`). Each submodule has a focused responsibility and the code is well-documented with doc comments and section separators. The pure metric functions (`compute_precision`, `compute_recall`, `compute_f1`) are clean and simple.

The two HIGH findings (`validate_repo` at 168 lines and `evaluate_at_thresholds` at 145 lines) are the primary complexity concerns. Both functions have high line counts driven by sequential error handling and nested iteration respectively. The algorithmic nesting in `evaluate_at_thresholds` is inherent to the O(T*C*Q*F) evaluation, but extracting a per-query helper would reduce nesting from 4 to 3 levels and make the accumulator logic more readable. The `validate_repo` orchestrator would benefit from sub-step extraction to stay under the 50-line guideline.

The deny-list duplication (MEDIUM) is a maintainability concern that should be addressed to prevent future drift between the filtering logic and the report metadata (applies ADR-001 -- fix noticed issues immediately).
