# Regression Review Report

**Branch**: feat/179-git-log-parser-gix -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Inconsistent path-to-string conversion in `compute_coupling`** - `crates/rskim/src/cmd/heatmap/metrics.rs:99`
**Confidence**: 85%
- Problem: `compute_coupling` uses `f.path.to_str().unwrap_or("")` while every other function in the same file uses `f.path.to_string_lossy().into_owned()`. The `to_str().unwrap_or("")` approach maps non-UTF8 paths to an empty string `""`, which could cause false coupling matches if multiple files have non-UTF8 paths (they all map to the same key). The old code used `f.path.as_str()` (when `path` was `String`) so non-UTF8 was impossible; the migration to `PathBuf` introduces this edge case.
- Fix: Use `to_string_lossy()` consistently across all functions:
```rust
let files: Vec<std::borrow::Cow<'_, str>> = commit
    .changed_files
    .iter()
    .map(|f| f.path.to_string_lossy())
    .collect();
```
  Note: this changes the borrowed `&str` to `Cow<str>`, which may require downstream adjustments in the `co_occur` and `weighted_total` maps (they borrow `&str` from commit data). An alternative is to pre-convert to owned strings once at the top of the loop, consistent with the other functions.

### MEDIUM

**Lossy `i64` to `u64` cast for timestamps** - `crates/rskim/src/cmd/heatmap/metrics.rs:215`
**Confidence**: 82%
- Problem: `commit.timestamp as u64` silently wraps negative `i64` values to large `u64` values. The `CommitInfo.timestamp` field is now `i64` (to support pre-epoch commits per `git_parser.rs:170`), but `compute_stability` stores timestamps in `Vec<u64>` and compares against `now_epoch: u64`. A negative timestamp (e.g., a malformed commit or pre-1970 date) would produce a very large `u64`, making the file appear extremely old (recency = 0) rather than flagging an error or being skipped.
- Fix: Clamp or filter negative timestamps:
```rust
.push(commit.timestamp.max(0) as u64);
```
  Or convert `file_commits` to `Vec<i64>` and `now_epoch` to `i64` for consistent signed arithmetic throughout.

**Lossy `u64` to `i64` cast for parsed timestamps** - `crates/rskim/src/cmd/heatmap/git_source.rs:216`
**Confidence**: 80%
- Problem: `timestamp as i64` where `timestamp` is parsed as `u64` silently wraps values larger than `i64::MAX`. While unlikely in practice (timestamps past year 292 billion), this is a lossy cast that would produce a negative timestamp, contradicting the `u64` parse. The cast exists because `CommitRecord` (now aliased from `CommitInfo`) changed its timestamp field from `u64` to `i64`.
- Fix: Use saturating or checked conversion:
```rust
timestamp: i64::try_from(timestamp).unwrap_or(i64::MAX),
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`rskim-search` dependency promoted without version pin** - `crates/rskim/Cargo.toml:17` (Confidence: 65%) -- `rskim-search` moved from `[dev-dependencies]` to `[dependencies]` without a version pin (only `path = "../rskim-search"`). Since `publish = false` on rskim-search this is fine today, but if it is ever published, consumers would need a version bound. Low-priority since this is an internal workspace dependency.

- **`SearchError::Git` variant added to non-exhaustive enum** - `crates/rskim-search/src/types.rs:430` (Confidence: 62%) -- `SearchError` is not marked `#[non_exhaustive]`, so adding the `Git` variant is technically a breaking change for any external exhaustive `match`. Since `rskim-search` has `publish = false`, this has no external impact today. Consider adding `#[non_exhaustive]` for future-proofing.

- **`test_search_result_serialization` test data changed** - `crates/rskim-search/src/types.rs:474` (Confidence: 60%) -- The test changed `match_positions: vec![5..10]` to `vec![5..8, 12..15]` (to fix clippy `single_range_in_vec_init`). The assertions were updated to match. This is a test-data-only change and does not affect actual serialization behavior, but changing test fixtures alongside production code can mask regressions -- ideally the clippy fix would be in a separate commit.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The PR successfully adds the new `GixSource` temporal parser and migrates heatmap from local duplicate types to shared canonical types. The migration is complete -- no leftover references to old field names (`subject`, `files`), all 3750 tests pass, and the JSON output format is unaffected since `HeatmapResult` uses computed `FileMetrics`, not `CommitRecord` directly.

The blocking HIGH-severity issue is the inconsistent path-to-string conversion in `compute_coupling` (`to_str().unwrap_or("")` vs `to_string_lossy()` everywhere else). The MEDIUM timestamp cast issues are low-probability in practice but represent lossy conversions introduced by the `u64` to `i64` type migration that could silently produce incorrect values for edge-case timestamps.
