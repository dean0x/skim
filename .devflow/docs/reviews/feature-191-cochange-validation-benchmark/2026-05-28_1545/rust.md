# Rust Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-28T15:45:00Z

## Issues in Your Changes (BLOCKING)

### HIGH

**Incorrect year calculation in `chrono_now` — leap years not accounted for** - `crates/rskim-bench/src/bin/cochange_validate.rs:215`
**Confidence**: 95%
- Problem: The year calculation `1970 + (secs / 86400) / 365` does not account for leap years. After 55+ years from the epoch (2025+), the accumulated error is roughly 13-14 days, which can push the computed year off by 1. The function also replaces month/day with `XX-XX`, making the timestamp unusable for reproducibility (the stated purpose of `RunMetadata.timestamp`). If the field is for human readability only, the placeholder month/day defeats that too.
- Fix: Use a proper date formatting approach. Since this is a binary (not a library), adding a dependency like `chrono` or `time` is acceptable. Alternatively, use a simpler but correct calculation:
```rust
fn chrono_now() -> String {
    use std::process::Command;
    // Shell out to `date` for a correct ISO-8601 timestamp.
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
```
Or use the `time` crate which is already a transitive dependency of several crates in the workspace.

**Deny-list patterns duplicated between `deny_list.rs` constants and `deny_list_pattern_names()` in the binary** - `crates/rskim-bench/src/bin/cochange_validate.rs:238-261`, `crates/rskim-bench/src/cochange/deny_list.rs:14-45`
**Confidence**: 92%
- Problem: The `deny_list_pattern_names()` function in the binary manually re-lists the same patterns that are already defined as constants in `deny_list.rs` (`DENIED_FILENAMES`, `DENIED_DIRS`, `DENIED_EXTENSIONS`). These two lists can drift silently -- if a new pattern is added to the constants, `deny_list_pattern_names()` will not reflect it in reports, creating a false methodology description. This violates DRY and the project's single-responsibility principle.
- Fix: Expose a public function from `deny_list.rs` that returns the pattern names:
```rust
// In deny_list.rs:
#[must_use]
pub fn pattern_names() -> Vec<String> {
    let mut names = Vec::new();
    names.extend(DENIED_FILENAMES.iter().map(|s| s.to_string()));
    names.extend(DENIED_DIRS.iter().map(|d| format!("{d}/")));
    names.extend(DENIED_EXTENSIONS.iter().map(|e| format!("*.{e}")));
    names
}
```
Then in the binary: `let deny_list_patterns = rskim_bench::cochange::deny_list::pattern_names();`

### MEDIUM

**`evaluate_at_thresholds` has O(T * C * F * A) complexity with quadratic inner loop per commit** - `crates/rskim-bench/src/cochange/validate.rs:215-260`
**Confidence**: 85%
- Problem: For each threshold `T`, for each multi-file test commit `C`, for each query file `F` in the commit, the function iterates over ALL file IDs `A` in the matrix to compute Jaccard scores. This is `O(T * C * F * A)`. For a repo with 10K files, 500 test commits averaging 3 files each, and 6 thresholds, this is `6 * 500 * 3 * 10000 = 90 million Jaccard lookups`. The predicted set is also recomputed from scratch for every threshold rather than computing all Jaccard scores once and filtering by threshold.
- Fix: Restructure to compute Jaccard scores once per (query, candidate) pair, then sweep thresholds:
```rust
for &query_id in &known_ids {
    // Compute jaccard scores ONCE for this query.
    let scores: Vec<(FileId, f64)> = all_file_ids.iter()
        .filter(|&&c| c != query_id)
        .filter_map(|&c| reader.jaccard(query_id, c).ok().filter(|&j| j > 0.0).map(|j| (c, j)))
        .collect();

    for (ti, &threshold) in thresholds.iter().enumerate() {
        let predicted: HashSet<FileId> = scores.iter()
            .filter(|(_, j)| *j >= threshold)
            .map(|(id, _)| *id)
            .collect();
        // ... accumulate metrics
    }
}
```
This reduces Jaccard calls from `T * F * A` to `F * A` per commit.

**`build_path_map` uses `BTreeSet`-like behavior via sort+dedup on `Vec` but allocates all paths first** - `crates/rskim-bench/src/cochange/validate.rs:52-63`
**Confidence**: 80%
- Problem: The PR description states "BTreeSet for sorted file collections" but `build_path_map` collects all paths into a `Vec`, sorts, and deduplicates. This allocates every path before deduplication. For repos with thousands of commits touching the same files repeatedly, this creates significant temporary allocation pressure. A `BTreeSet` (as the PR description suggests) would deduplicate during insertion.
- Fix: Use `BTreeSet` as stated in the PR description:
```rust
pub fn build_path_map(commits: &[CommitInfo]) -> HashMap<PathBuf, FileId> {
    let paths: BTreeSet<PathBuf> = commits
        .iter()
        .flat_map(|c| c.changed_files.iter().map(|f| f.path.clone()))
        .collect();
    paths
        .into_iter()
        .enumerate()
        .map(|(i, p)| (p, FileId(i as u32)))
        .collect()
}
```

**`aggregate_metrics` micro averaging is incorrectly computed as mean-of-ratios instead of ratio-of-sums** - `crates/rskim-bench/src/cochange/validate.rs:548-576`
**Confidence**: 88%
- Problem: Micro-averaged precision and recall should be computed as the ratio of aggregated counts (total TP / total predicted, total TP / total actual) across all repos. Instead, the code averages per-repo micro-precision and micro-recall values (`mip_sum / count`), which is actually a macro average of micro metrics -- a mathematically different quantity. This produces incorrect aggregate micro metrics when repos have different numbers of queries.
- Fix: Accumulate raw TP, predicted, and actual counts from each repo, then compute ratios:
```rust
// This requires adding total_tp, total_predicted, total_actual fields
// to ThresholdMetrics or computing from per-query data.
// As a simpler approach, rename the fields to make the averaging strategy clear:
micro_precision: micro_p, // NOTE: this is macro-averaged micro-P, not true micro-P
```
Or restructure to pass raw counts through `ThresholdMetrics` for correct aggregation.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`temporal_split` clones the entire commit list for reversal** - `crates/rskim-bench/src/cochange/temporal_split.rs:88-89`
**Confidence**: 82%
- Problem: `commits.to_vec()` followed by `.reverse()` clones all `CommitInfo` structs (each containing a `Vec<FileChangeInfo>` with `PathBuf`s) just to reverse iteration order. For a repo with 10K+ commits each touching several files, this is a significant allocation. Since the caller (`validate_repo`) already owns `all_commits` as a mutable `Vec`, the function could accept `&mut Vec<CommitInfo>` or the caller could reverse before calling.
- Fix: Accept owned data or reverse in-place. For example, change the signature:
```rust
pub fn temporal_split(mut commits: Vec<CommitInfo>, train_fraction: f64) -> TemporalSplit {
    commits.reverse();
    // ... split_at logic on commits directly
}
```
Then the caller passes `all_commits` by value (which it already owns and does not use after the split).

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`save_to_devflow` uses relative path** - `crates/rskim-bench/src/bin/cochange_validate.rs:265` (Confidence: 70%) -- The path `.devflow/docs` is relative to the current working directory, which may not be the workspace root when the binary is invoked from elsewhere. Consider resolving relative to `CARGO_MANIFEST_DIR` or accepting a `--output-dir` flag.

- **`FileId` cast from `usize` to `u32` without overflow check** - `crates/rskim-bench/src/cochange/validate.rs:61` (Confidence: 65%) -- `FileId(i as u32)` will silently truncate if a repo has more than 4 billion unique paths. Unlikely in practice but the cast is unchecked.

- **`check_quality_gates` counts multi-file commits on the full (unfiltered) changed_files** - `crates/rskim-bench/src/cochange/validate.rs:118-121` (Confidence: 72%) -- The quality gate is checked on `all_commits` after deny-list filtering (line 368), so the count is correct. But the function itself does not document this precondition -- a caller passing unfiltered commits would get inflated counts. Consider documenting the assumption in the function's doc comment.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | - | 2 | 3 | - |
| Should Fix | - | - | 1 | - |
| Pre-existing | - | - | - | - |

**Rust Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The code is well-structured with good module separation, comprehensive error handling (no `.unwrap()` in non-test code), proper use of `#[must_use]`, and thorough test coverage. The `anyhow` error handling is appropriate for this application-level binary. The deny-list duplication is the most concerning maintainability issue (applies ADR-001 -- fix immediately rather than defer). The timestamp calculation bug and micro-averaging error are correctness issues that should be addressed before merge. The performance concern in `evaluate_at_thresholds` is worth addressing given that this benchmark will run against real repositories with potentially large file sets.
