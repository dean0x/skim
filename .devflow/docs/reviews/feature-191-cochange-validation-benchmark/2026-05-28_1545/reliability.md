# Reliability Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-28T15:45:00Z

## Issues in Your Changes (BLOCKING)

### HIGH

**`capture_head_sha` lacks subprocess timeout -- unbounded I/O wait** - `crates/rskim-bench/src/cochange/validate.rs:618-629`
**Confidence**: 92%
- Problem: `capture_head_sha` calls `std::process::Command::new("git").output()` without any timeout. If the git subprocess hangs (e.g., waiting for credential input on a misconfigured repo, or hitting a kernel-level lock on the repo directory), this blocks the calling thread indefinitely. The `clone_with_history` function correctly uses `git_run_with_timeout` with a 300s deadline, but `capture_head_sha` does not follow this pattern. Since `validate_repo` is called from a rayon thread pool (capped at 3 threads), a single hanging `git rev-parse HEAD` would permanently consume one of those three slots.
- Fix: Use `git_run_with_timeout` or implement a similar timeout mechanism for `capture_head_sha`. Alternatively, use the `Command::spawn` + `mpsc::recv_timeout` pattern already established in `clone.rs`:
```rust
fn capture_head_sha(repo_path: &Path) -> anyhow::Result<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo_path).args(["rev-parse", "HEAD"]);
    // Re-use the existing timeout machinery or a simpler variant:
    let child = cmd.stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("git rev-parse spawn: {e}"))?;
    let output = child.wait_with_output()
        .map_err(|e| anyhow::anyhow!("git rev-parse: {e}"))?;
    // ... (or extract into a shared timeout helper)
```
A lightweight alternative: set `credential.helper=` on this command too to prevent credential prompts, matching the security pattern in `clone_with_history`.

**`validate_repo` repo name extraction lacks path traversal protection** - `crates/rskim-bench/src/cochange/validate.rs:327-335`
**Confidence**: 85%
- Problem: `validate_repo` extracts the repo name via `entry.url.rsplit('/').next().unwrap_or("unknown").trim_end_matches(".git")` and immediately joins it to `corpus_dir` at line 335: `let dest = corpus_dir.join(&repo_name)`. Unlike `clone.rs` which has `extract_repo_name` with explicit path-traversal rejection (checking for `.`, `..`, `/`, `\\`), this code path has no such validation. If a corpus TOML entry contained a URL ending in `..` (e.g., `https://github.com/owner/..`), the `dest` path would resolve to the parent of `corpus_dir`. While the corpus TOML is developer-controlled, the existing `extract_repo_name` function demonstrates this project's defense-in-depth approach -- the same invariant should hold here. Applies ADR-001 (fix noticed issues immediately regardless of scope).
- Fix: Reuse the existing `extract_repo_name` from `rskim_research::clone` or duplicate its validation checks:
```rust
let repo_name = rskim_research::clone::extract_repo_name(&entry.url)
    .unwrap_or_else(|_| "unknown".to_string());
```
Note: `extract_repo_name` is currently `fn` (private). Either make it `pub` or inline the validation here.

### MEDIUM

**`evaluate_at_thresholds` has O(C * T * F * F_all) complexity with no bound on F_all** - `crates/rskim-bench/src/cochange/validate.rs:165-310`
**Confidence**: 82%
- Problem: The inner loop iterates over `all_file_ids` (every unique file in the training set) for each `query_id` in each commit for each threshold. For large repositories (e.g., pydantic with thousands of files), this is quadratic in file count: O(commits * thresholds * files_in_commit * all_files). With 6 thresholds, 500 test commits of 5 files each, and 5000 total files, this is ~75M jaccard lookups. While each lookup is O(log n), the total work is substantial and unbounded by any explicit limit. The code comments note this is "O(F^2 log P)" but do not bound F.
- Fix: Consider adding a configurable upper bound on `all_file_ids.len()` with a warning when exceeded, or document the expected runtime characteristics more explicitly. For very large repos, a sampling strategy could be applied to test commits. At minimum, add an assertion:
```rust
const MAX_FILES_FOR_EVALUATION: usize = 20_000;
if all_file_ids.len() > MAX_FILES_FOR_EVALUATION {
    anyhow::bail!(
        "evaluation aborted: {} files exceeds max {} (would be too slow)",
        all_file_ids.len(), MAX_FILES_FOR_EVALUATION
    );
}
```

**`chrono_now` produces incorrect timestamps (year calculation ignores leap years)** - `crates/rskim-bench/src/bin/cochange_validate.rs:207-222`
**Confidence**: 90%
- Problem: The year calculation `1970 + (secs / 86400) / 365` does not account for leap years, which causes a cumulative drift of approximately 1 day per 4 years. After 56 years (2026), this is off by ~14 days, which means the year boundary could be wrong by 1 year near year-end. The month and day are already replaced with `XX-XX`, so the year is the only meaningful date component -- and it is unreliable. This affects the `timestamp` field in `RunMetadata` and the report filename when `--save-report` is used, which is used for reproducibility manifests.
- Fix: Either use `time` or `chrono` crate (already in the workspace dependency tree via other crates), or compute the year correctly by accounting for leap years:
```rust
fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Correct year accounting for leap years
    let days = secs / 86400;
    let mut year = 1970u64;
    let mut remaining_days = days;
    loop {
        let days_in_year = if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) { 366 } else { 365 };
        if remaining_days < days_in_year { break; }
        remaining_days -= days_in_year;
        year += 1;
    }
    format!(
        "{year}-XX-XXT{:02}:{:02}:{:02}Z",
        (secs % 86400) / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`build_path_map` can silently produce u32 overflow for FileId on very large repos** - `crates/rskim-bench/src/cochange/validate.rs:51-63`
**Confidence**: 80%
- Problem: `FileId(i as u32)` at line 61 will silently wrap if a repository has more than 2^32 unique file paths across all commits. While this is unlikely for the corpus repos selected, the code has no assertion guarding against this. Given the project's reliability principles, an explicit check is warranted.
- Fix: Add a precondition assertion:
```rust
assert!(
    paths.len() <= u32::MAX as usize,
    "too many unique paths ({}) for FileId(u32)",
    paths.len()
);
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Temporal split clones entire commit vec twice** - `crates/rskim-bench/src/cochange/temporal_split.rs:88-106` (Confidence: 65%) -- `temporal_split` calls `.to_vec()` on the input (line 88), then `.to_vec()` on each slice (lines 103-104), producing 3 full copies of the commit data. For repos with thousands of commits each containing file lists, this is material memory usage. Could use `Vec::split_off` or return indices instead of owned vecs.

- **`clone_with_history` idempotency check does not verify clone integrity** - `crates/rskim-research/src/clone.rs:300-303` (Confidence: 70%) -- The `dest.exists()` check returns early without verifying the directory is a valid git repo. A previous interrupted clone could leave a partial directory that passes `exists()` but fails `parse_history`. Consider checking for `.git/HEAD` or running `git -C dest rev-parse --is-inside-work-tree`.

- **`quality_gate` checks `changed_files.len()` which may include 0 files after deny-list filtering** - `crates/rskim-bench/src/cochange/validate.rs:117-121` (Confidence: 62%) -- The quality gate comment says ">=2 files after deny-list filtering" but the check counts all commits whose `changed_files.len() >= 2`. Since `filter_denied` is called before `check_quality_gates` in `validate_repo`, the files array is already filtered. However, a commit that originally had 3 files but had 2 denied would show `changed_files.len() == 1` post-filter. This is actually correct behavior but the comment is slightly misleading.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The code demonstrates strong reliability practices overall: bounded thread pool concurrency (3 threads), explicit quality gates, NaN guards on `train_fraction`, graceful error recovery via `error_result` that prevents panics from propagating through rayon, and subprocess timeouts on git clone operations. However, the missing timeout on `capture_head_sha` creates an unbounded I/O wait that could permanently block a rayon thread, and the repo name extraction in `validate_repo` lacks the path traversal protection that exists in the sister function in `clone.rs`. The incorrect year calculation in `chrono_now` undermines the reproducibility manifest's accuracy.
