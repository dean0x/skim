# Consistency Review Report

**Branch**: feat/179-git-log-parser-gix -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**Duplicate fix-detection logic not consolidated** - `crates/rskim/src/cmd/heatmap/metrics.rs:20-22` vs `crates/rskim-search/src/temporal/mod.rs:22-23`
**Confidence**: 95%
- Problem: The PR introduces `is_fix_commit()` in `rskim-search::temporal` as a shared, canonical predicate (compiled once via `Lazy<Regex>`), yet the heatmap module retains its own `build_fix_regex()` function at `metrics.rs:20-22` with the identical regex pattern `(?i)\b(fix|bug|hotfix|patch|revert)\b`. The heatmap functions `compute_stability` and `compute_fix_after_touch` still accept a `&Regex` parameter and call `fix_regex.is_match()` instead of using the shared `is_fix_commit()`. The PR description states it "eliminates the duplication" of types, but this regex duplication remains.
- Fix: Replace `build_fix_regex()` usage in heatmap with `rskim_search::is_fix_commit()`. The `compute_stability` and `compute_fix_after_touch` signatures should accept a predicate `Fn(&str) -> bool` or call `is_fix_commit` directly, removing the `fix_regex: &Regex` parameter. Then delete `build_fix_regex()`.

### MEDIUM

**Inconsistent `PathBuf`-to-`&str` conversion strategy across heatmap metrics functions** - `crates/rskim/src/cmd/heatmap/metrics.rs:99` vs `metrics.rs:36,211,268,337,450,462`
**Confidence**: 85%
- Problem: After the migration to `FileChangeInfo { path: PathBuf }`, the heatmap metrics module uses two different strategies to convert `PathBuf` to `&str`:
  1. `to_string_lossy().into_owned()` -- used in `compute_churn` (line 36), `compute_stability` (line 211), `compute_authors` (line 268), `compute_fix_after_touch` (line 337), and `compute_encapsulation` (line 450, 462). This allocates a new `String` every time.
  2. `to_str().unwrap_or("")` -- used in `compute_coupling` (line 99). This borrows but silently replaces non-UTF-8 paths with an empty string, which would create phantom coupling entries keyed on `""`.

  The two approaches also have different behavior for non-UTF-8 paths: `to_string_lossy` replaces invalid bytes with the Unicode replacement character, while `to_str().unwrap_or("")` silently drops the entire path.
- Fix: Pick one approach consistently. Since git paths are almost always UTF-8 and `compute_coupling` benefits from borrowing (zero-allocation in the hot path comment at line 114), `to_str().unwrap_or_default()` with a filter to skip empty results would be more consistent with the function's performance intent. Alternatively, use `to_string_lossy()` everywhere for uniform behavior.

**`timestamp` type mismatch: `i64` stored as `u64` via `as` cast** - `crates/rskim/src/cmd/heatmap/metrics.rs:215`
**Confidence**: 82%
- Problem: `CommitInfo.timestamp` is now `i64` (to support pre-epoch commits per the git_parser docs), but `compute_stability` stores timestamps in `HashMap<String, Vec<u64>>` (line 205) and casts via `commit.timestamp as u64` (line 215). For negative timestamps, this wrapping cast produces very large `u64` values, breaking the recency calculation at lines 231-233 (`now_epoch >= last_ts` would be false, resulting in `days_since = 0.0`). The `make_commit` test helper also does `ts as i64` (line 526), propagating the mismatch.
- Fix: Either change the internal `file_commits` map to `HashMap<String, Vec<i64>>` and adjust the `now_epoch` parameter to `i64`, or explicitly clamp/filter negative timestamps before the cast: `commit.timestamp.max(0) as u64`.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Comment references borrowed `&str` keys but `path` is now `PathBuf`** - `crates/rskim/src/cmd/heatmap/metrics.rs:89-90`
**Confidence**: 80%
- Problem: The comment says `"&str keys borrow from CommitRecord.changed_files[].path"` but after the migration, `path` is `PathBuf`, and the borrowing now goes through `to_str()` which returns a temporary `&str` from the `PathBuf`. The comment is still technically correct in that the data outlives the map, but the mechanism changed. The comment should reflect that `to_str()` is used to borrow from the `PathBuf`.
- Fix: Update comment to: `"&str keys borrow from CommitRecord.changed_files[].path via PathBuf::to_str() -- valid for entire function because commits (and therefore all PathBufs) outlive these maps."`

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`once_cell` vs `std::sync::LazyLock`** - `crates/rskim-search/src/temporal/mod.rs:15` (Confidence: 65%) -- Rust 1.80+ provides `std::sync::LazyLock` in std, making `once_cell` unnecessary as an external dependency for this use case. The project's MSRV may preclude this, but if the project targets Rust 1.80+, the external dependency could be dropped.

- **`make_commit` test helper `ts` parameter is `u64` but `timestamp` field is `i64`** - `crates/rskim/src/cmd/heatmap/metrics.rs:517-526` (Confidence: 70%) -- The test helper accepts `u64` and casts to `i64`, obscuring the actual type. Changing the parameter to `i64` would be clearer and allow negative-timestamp test cases without confusion.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The shared temporal types (`CommitInfo`, `FileChangeInfo`, `HistoryResult`, `TemporalMetadata`, `TemporalSource`) are well-designed and the type alias approach in `heatmap/types.rs` (`CommitInfo as CommitRecord`, `FileChangeInfo as FileChange`) is a clean migration strategy. However, the PR's stated goal of eliminating duplication is only partially achieved: the fix-detection regex remains duplicated between `rskim-search::temporal::is_fix_commit()` and `rskim::cmd::heatmap::metrics::build_fix_regex()`. Additionally, the `PathBuf`-to-string conversion strategy is inconsistent across the heatmap metrics functions, and the `i64`-to-`u64` timestamp cast introduces a subtle behavioral inconsistency for negative timestamps.
