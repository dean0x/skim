# Performance Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29

## Issues in Your Changes (BLOCKING)

### HIGH

**`train.to_vec()` clones entire training commit history** - `crates/rskim-bench/src/cochange/validate.rs:602`
**Confidence**: 85%
- Problem: In `build_and_evaluate`, the full training commit list (potentially thousands of commits, each with vectors of changed files) is cloned via `train.to_vec()` just to satisfy `CochangeMatrixBuilder::build(&HistoryResult)`. For a repo with 5000 commits averaging 5 files each, this is ~25000 `PathBuf` allocations plus the `CommitInfo` structs — potentially several MB of heap churn that runs once per repo (3 repos concurrently = 3x pressure).
- Fix: If the builder API can accept `&[CommitInfo]` directly, pass a reference. If it requires ownership of `HistoryResult`, consider taking `train` by value (move) into `build_and_evaluate` instead of borrowing it. The caller only uses `split.train.len()` after this call, which can be captured before the move:
```rust
// Capture count before moving
let train_count = split.train.len();
let eval = match build_and_evaluate(split.train, &split.test, thresholds) { ... };
// Use train_count instead of split.train.len() below
```

### MEDIUM

**O(Q x F) evaluation per test commit: HashSet allocation per query per threshold** - `crates/rskim-bench/src/cochange/validate.rs:286-290`
**Confidence**: 82%
- Problem: For each threshold, the inner loop creates a new `HashSet<FileId>` from the cached jaccard pairs via `.filter().collect()`. With Q queries per commit and T thresholds, this is Q*T HashSet allocations per commit. The HashSets are short-lived and could be replaced by a pre-allocated scratch set that is cleared and reused.
- Fix: Pre-allocate a scratch `HashSet<FileId>` before the threshold loop and reuse it:
```rust
let mut predicted_scratch = HashSet::with_capacity(all_file_ids.len());
for ti in 0..n_thresholds {
    let threshold = thresholds[ti];
    for q_idx in 0..known_ids.len() {
        predicted_scratch.clear();
        predicted_scratch.extend(
            jaccard_cache[q_idx].iter()
                .filter(|&&(_, j)| j >= threshold)
                .map(|&(cid, _)| cid)
        );
        let p = compute_precision(&predicted_scratch, &actual_sets[q_idx]);
        // ...
    }
}
```

**`is_denied` allocates a new String per call via `path.replace('\\', "/")`** - `crates/rskim-bench/src/cochange/deny_list.rs:60`
**Confidence**: 80%
- Problem: `is_denied` is called inside `filter_denied` for every file in every commit. On Unix, paths never contain `\\`, so the `replace('\\', "/")` allocates a new String unconditionally even though the result is identical to the input. For a repo with 50,000 file change events, this is 50,000 unnecessary heap allocations.
- Fix: Use `Cow<str>` or check for backslash presence before allocating:
```rust
pub fn is_denied(path: &str) -> bool {
    let normalised;
    let effective = if path.contains('\\') {
        normalised = path.replace('\\', "/");
        normalised.as_str()
    } else {
        path
    };
    // Use `effective` instead of `normalised` below...
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`filter_denied` calls `to_string_lossy()` per file, allocating when path contains non-UTF-8** - `crates/rskim-bench/src/cochange/deny_list.rs:117`
**Confidence**: 80%
- Problem: `to_string_lossy()` returns a `Cow<str>` which borrows when the path is valid UTF-8 (common case), but allocates when it contains non-UTF-8 bytes. This is acceptable for correctness but the documentation does not make it clear. Combined with the inner `is_denied` allocation (above), this means two potential allocations per file.
- Fix: Convert path once and pass the resulting `&str` directly. For completeness, consider operating on `&Path` throughout rather than converting to string, using `Path::file_name()` and `Path::components()`.

## Pre-existing Issues (Not Blocking)

(none identified in changed files)

## Suggestions (Lower Confidence)

- **Evaluate only against `pairs_for_file` results instead of all_file_ids** - `crates/rskim-bench/src/cochange/validate.rs:245` (Confidence: 70%) — The jaccard cache iterates over ALL file IDs (`all_file_ids`) as candidates for each query. Since `jaccard` returns 0.0 for any pair not in the matrix, the vast majority of calls return early. Using `reader.pairs_for_file(query_id)` would yield only non-zero partners directly, reducing wasted binary searches. However, the code already guards against large repos (MAX_FILES_FOR_EVALUATION=20000) and the doc comments indicate this was a deliberate design choice to avoid the O(n) prefix scan of `pairs_for_file`.

- **`build_path_map` uses BTreeSet for dedup but allocates HashMap via collect** - `crates/rskim-bench/src/cochange/validate.rs:64-77` (Confidence: 65%) — Could pre-size the HashMap with `HashMap::with_capacity(unique_paths.len())` to avoid rehashing during construction.

- **Parallel repo processing spawns threads for git subprocess timeout** - `crates/rskim-bench/src/cochange/validate.rs:661-666` (Confidence: 62%) — Each `capture_head_sha` call spawns a new OS thread just for timeout monitoring. With 3 concurrent repos, this is 3 ephemeral threads. Not a bottleneck at current scale but an architectural debt at higher concurrency.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code is well-structured with good performance awareness (pre-computed jaccard cache to avoid T-factor redundant work, MAX_FILES_FOR_EVALUATION guard, zero-copy temporal split, dedicated thread pool capped at 3). The blocking HIGH finding (`train.to_vec()`) is a straightforward fix that avoids cloning potentially large datasets. The MEDIUM findings are optimization opportunities that reduce allocation pressure in the hot evaluation loop — worth addressing per `applies ADR-001` (fix noticed issues immediately) but not critical for correctness or overall runtime given the 20k file cap.
