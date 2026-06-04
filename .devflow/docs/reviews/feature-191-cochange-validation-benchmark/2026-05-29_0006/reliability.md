# Reliability Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29

## Issues in Your Changes (BLOCKING)

### HIGH

**`build_path_map` uses `assert!` instead of returning an error for capacity overflow** - `crates/rskim-bench/src/cochange/validate.rs:68-72`
**Confidence**: 88%
- Problem: `assert!(unique_paths.len() <= u32::MAX as usize, ...)` will panic and abort the entire benchmark run (including other repos being processed in parallel) if a single repository has more than 4 billion unique file paths. While the `MAX_FILES_FOR_EVALUATION` guard (20,000 files) makes this practically unreachable today, the assertion fires *before* that guard runs (since `build_path_map` is called on `train` commits, not filtered by the evaluation limit). If someone raises `MAX_FILES_FOR_EVALUATION` or uses `build_path_map` in another context, this becomes an unrecoverable panic inside a rayon thread pool.
- Fix: Convert `assert!` to an early-return `anyhow::Result`:
```rust
pub fn build_path_map(commits: &[CommitInfo]) -> anyhow::Result<HashMap<PathBuf, FileId>> {
    let unique_paths: BTreeSet<&PathBuf> = commits
        .iter()
        .flat_map(|c| c.changed_files.iter().map(|f| &f.path))
        .collect();
    if unique_paths.len() > u32::MAX as usize {
        anyhow::bail!("too many unique paths ({}) for FileId(u32)", unique_paths.len());
    }
    Ok(unique_paths
        .into_iter()
        .enumerate()
        .map(|(i, p)| (p.clone(), FileId(i as u32)))
        .collect())
}
```

**Detached thread in `capture_head_sha` leaks resources on timeout** - `crates/rskim-bench/src/cochange/validate.rs:663-666`
**Confidence**: 82%
- Problem: When `capture_head_sha` times out, the function sends SIGKILL to the child process but the background thread (`std::thread::spawn(move || { ... })`) that was waiting on the child remains detached. If the child process has already exited by the time SIGKILL is sent (race window), or if the kill signal does not arrive immediately, the thread holds the `Child` handle until it is dropped. In practice this leak is bounded (30s timeout, max 7 repos, 3 concurrent), so it is unlikely to be pathological. However, per the "every resource must have a known lifetime" principle, the detached thread has no explicit join, making its lifetime implicit.
- Fix: Store the `JoinHandle` and join it after killing the process (with a short secondary timeout or just allow it to complete since the process was killed):
```rust
Err(_timeout) => {
    #[cfg(unix)]
    {
        unsafe { libc::kill(child_id as libc::pid_t, libc::SIGKILL); }
    }
    // After kill, the thread will complete quickly — join to reclaim resources.
    let _ = handle.join();
    anyhow::bail!("git rev-parse HEAD timed out after {GIT_SHA_TIMEOUT_SECS}s");
}
```

### MEDIUM

**No upper bound on `test_commits` iteration count in `evaluate_at_thresholds`** - `crates/rskim-bench/src/cochange/validate.rs:217`
**Confidence**: 80%
- Problem: While `all_file_ids` is bounded at 20,000 by `MAX_FILES_FOR_EVALUATION`, there is no corresponding bound on the number of test commits processed. A repository with 100,000 test commits each touching 100 mapped files would produce 100,000 * 100 * 20,000 jaccard calls (200 billion). The quality gate only bounds *multi-file* commits to >= 50 but does not cap the upper end. In the current corpus (7 repos, 80/20 split), this is unlikely to be an issue, but for extensibility there is no circuit-breaker.
- Fix: Add a constant like `MAX_TEST_COMMITS` or `MAX_JACCARD_CALLS` with a bail:
```rust
const MAX_TEST_COMMITS: usize = 50_000;
if test_commits.len() > MAX_TEST_COMMITS {
    anyhow::bail!(
        "test commit count {} exceeds evaluation limit {}",
        test_commits.len(),
        MAX_TEST_COMMITS
    );
}
```

**`jaccard_cache` allocation per commit is unbounded by commit size** - `crates/rskim-bench/src/cochange/validate.rs:242-261`
**Confidence**: 80%
- Problem: For each test commit, `jaccard_cache` is allocated with capacity `known_ids.len()`, and each inner Vec can grow up to `all_file_ids.len()` entries (20,000). A single commit touching 1,000 mapped files creates a cache of 1,000 * 20,000 = 20 million `(FileId, f64)` tuples (~320 MB). There is no per-commit file count guard analogous to the `commits_skipped_too_large` logic used during matrix building. The deny-list filter removes lock files but does not cap the per-commit file count for evaluation.
- Fix: Add a per-commit file count guard before the inner loop:
```rust
const MAX_FILES_PER_COMMIT: usize = 500;
if known_ids.len() > MAX_FILES_PER_COMMIT {
    // Skip bulk refactors in test evaluation to avoid memory explosion.
    continue;
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`clone_with_history` existence check (`dest.exists()`) is not atomic with the clone** - `crates/rskim-research/src/clone.rs:301-303`
**Confidence**: 80%
- Problem: If the destination directory exists but the clone was previously interrupted (partial clone), `clone_with_history` returns `Ok(())` silently. A subsequent `GixSource.parse_history()` call may then fail with a confusing error or produce incomplete data. The idempotency check only verifies the directory exists, not that it is a valid git repo.
- Fix: Verify the directory is a valid git repo (e.g., check `.git/HEAD` exists) before returning early:
```rust
if dest.exists() {
    // Verify it's actually a valid git repo, not a partial clone artifact.
    if dest.join(".git").join("HEAD").exists() {
        return Ok(());
    }
    // Remove partial clone and re-clone.
    std::fs::remove_dir_all(dest)?;
}
```

## Pre-existing Issues (Not Blocking)

(none found at CRITICAL severity)

## Suggestions (Lower Confidence)

- **`chrono_now` leap-second ambiguity** - `crates/rskim-bench/src/bin/cochange_validate.rs:223-278` (Confidence: 65%) — The custom Gregorian calendar implementation does not account for leap seconds. Unix timestamps from `SystemTime` already exclude leap seconds so the output is correct for its stated purpose, but the implementation comment does not document this assumption.

- **`capture_head_sha` race between `child.id()` and process exit** - `crates/rskim-bench/src/cochange/validate.rs:661-681` (Confidence: 70%) — After `spawn()`, the child may exit before the background thread calls `wait_with_output()`. This is handled correctly (the thread simply returns the exit status), but the PID sent to `kill()` on timeout may refer to a recycled process if the original exited and a new process reused the PID. On modern systems PID recycling is rare within 30s, but worth noting.

- **No progress reporting or cancellation signal for long-running evaluation** - `crates/rskim-bench/src/cochange/validate.rs:217-313` (Confidence: 62%) — The evaluation loop can take minutes for large repos with no user-visible progress. If the user Ctrl+C's during evaluation, rayon's default handler panics, potentially leaving tempdir cleanup incomplete.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The code demonstrates strong reliability patterns overall: bounded thread pool (3 threads), subprocess timeouts (30s), quality gates, `MAX_FILES_FOR_EVALUATION` guard, NaN-safe fraction clamping, and degenerate-input handling in `temporal_split`. The main gaps are: (1) a panic-producing `assert!` inside a shared function that should return a Result, (2) detached threads with implicit lifetime, and (3) missing upper bounds on evaluation iteration that could cause memory exhaustion with adversarial corpus configurations. These are addressed by the findings above. (applies ADR-001 — all findings surfaced for immediate resolution, avoids PF-002 — no findings deferred)
