# Rust Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29

## Issues in Your Changes (BLOCKING)

### HIGH

**`assert!` in library code can panic in production** - `crates/rskim-bench/src/cochange/validate.rs:68`
**Confidence**: 85%
- Problem: `build_path_map` uses `assert!(unique_paths.len() <= u32::MAX as usize, ...)` which panics at runtime instead of returning a `Result`. While hitting u32::MAX unique files is improbable in practice, this violates the project's "Never throw in business logic" principle. The crate has `#[lints.clippy] panic = "deny"` in its Cargo.toml, but `assert!` bypasses this lint (clippy only flags `panic!()` macro, not `assert!`).
- Fix: Return `anyhow::Result` from `build_path_map` and replace the assert with a bail:
```rust
pub fn build_path_map(commits: &[CommitInfo]) -> anyhow::Result<HashMap<PathBuf, FileId>> {
    let unique_paths: BTreeSet<&PathBuf> = commits
        .iter()
        .flat_map(|c| c.changed_files.iter().map(|f| &f.path))
        .collect();
    if unique_paths.len() > u32::MAX as usize {
        anyhow::bail!(
            "too many unique paths ({}) for FileId(u32)",
            unique_paths.len()
        );
    }
    Ok(unique_paths
        .into_iter()
        .enumerate()
        .map(|(i, p)| (p.clone(), FileId(i as u32)))
        .collect())
}
```
Then propagate the `?` at the call site in `build_and_evaluate`.

---

**`train.to_vec()` clones the entire training commit set unnecessarily** - `crates/rskim-bench/src/cochange/validate.rs:602`
**Confidence**: 88%
- Problem: `build_and_evaluate` receives `train: &[CommitInfo]` and immediately does `train.to_vec()` to construct a `HistoryResult`. Each `CommitInfo` contains a `Vec<FileChangeInfo>` with `PathBuf` members, so this deep-clones potentially hundreds of thousands of path buffers for large repositories. The CLAUDE.md explicitly states "Prefer borrowing over cloning" and "Zero-copy when possible".
- Fix: If `CochangeMatrixBuilder::build` accepts `&HistoryResult`, construct a `HistoryResult` that borrows. If the builder API requires ownership, consider changing `build_and_evaluate` to take `train: Vec<CommitInfo>` (owned) since `validate_repo` already has `split.train` as an owned Vec it could move. The caller at line 406 would use `&split.train` for counting and pass ownership for matrix building:
```rust
fn build_and_evaluate(
    train: Vec<CommitInfo>,
    test: &[CommitInfo],
    thresholds: &[f64],
) -> anyhow::Result<EvalResult> {
    let path_map = build_path_map(&train);
    // ...
    let history_for_builder = rskim_search::HistoryResult {
        commits: train, // move, no clone
        metadata: rskim_search::TemporalMetadata { ... },
    };
    // ...
}
```
The caller would record `split.train.len()` before moving.

### MEDIUM

**`unsafe` block SAFETY comment is inaccurate** - `crates/rskim-bench/src/cochange/validate.rs:679-681`
**Confidence**: 82%
- Problem: The comment states "SAFETY: kill(2) is always safe to call with a valid pid." However, after the timeout fires, the spawned thread may have already reaped the child process (between the timeout check and the kill call). In that case, the pid could have been reassigned by the OS to an unrelated process. On modern Linux/macOS with short-lived processes, pid recycling within 30 seconds is unlikely but not impossible under heavy load. The `child_id` is captured before the thread starts but the thread owns the `Child` and calls `wait()` which reaps it.
- Fix: Document the race more precisely and accept it as a known acceptable risk given the benchmark's non-production use:
```rust
// SAFETY: kill(2) is safe to call. Race note: the background thread
// may have already reaped this pid, and the pid could theoretically
// be recycled. Acceptable for a benchmark tool; a production daemon
// would use pidfd_open(2) on Linux 5.3+.
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`clone_with_history` skips fetch when dest exists but may be stale** - `crates/rskim-research/src/clone.rs:301-303`
**Confidence**: 80%
- Problem: The idempotency check `if dest.exists() { return Ok(()); }` means that if a prior run was interrupted mid-clone (leaving a partial `.git` directory), or if the remote has new commits since last clone, the function will happily return success. For a benchmark that evaluates against HEAD, a stale clone can silently produce outdated results. The cochange-corpus.toml commits pin specific SHAs, but `clone_with_history` does not check out a specific commit (it stays at HEAD of the default branch), so the actual HEAD may drift between runs.
- Fix: After the existence check, verify the clone is valid (e.g., `git -C dest rev-parse --git-dir` succeeds). If the corpus pins to a specific commit, optionally verify HEAD matches expectations. At minimum, document the limitation.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`test_utils` module is always compiled in release** - `crates/rskim-bench/src/cochange/mod.rs:35` (Confidence: 70%) -- The `test_utils` module is unconditionally compiled (not gated by `#[cfg(test)]`) because integration tests need it. Consider gating it with a `#[cfg(any(test, feature = "test-support"))]` feature flag to avoid including test helpers in production binary size.

- **`chrono_now` reimplements calendar math** - `crates/rskim-bench/src/bin/cochange_validate.rs:223-248` (Confidence: 65%) -- The manual Gregorian calendar algorithm avoids a `chrono` dependency, which is a valid trade-off. However, the algorithm does not handle negative timestamps (pre-1970) and returns "unknown" for them. Documenting the valid range (1970-9999) would prevent surprising behavior if system clock issues occur.

- **`ThresholdMetrics` missing `Default` derive** - `crates/rskim-bench/src/cochange/types.rs:18` (Confidence: 62%) -- Unlike `RepoCochangeResult` which derives `Default`, `ThresholdMetrics` does not. This means any code that wants to construct a partial/default instance (for zero-filling) must manually specify all fields, as done in the `zero_metrics` helper. Adding `Default` would simplify those call sites.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The implementation is well-structured with excellent documentation, proper `#[must_use]` annotations, clean error handling with `anyhow`, and good use of the type system. The crate-level clippy `deny` lints are strong. The two HIGH findings (panicking assert and unnecessary deep clone) should be addressed before merge per applies ADR-001. The unsafe block comment is minor but worth improving. Overall, this is solid Rust code for a benchmark tool with good architectural separation.
