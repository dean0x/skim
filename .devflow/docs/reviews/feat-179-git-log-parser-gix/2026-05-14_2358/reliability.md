# Reliability Review Report

**Branch**: feat/179-git-log-parser-gix -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**Potential i64 overflow in lookback cutoff calculation** - `crates/rskim-search/src/temporal/git_parser.rs:101`
**Confidence**: 85%
- Problem: `SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64` performs an `as i64` cast on a `u64`. While `u64` epoch seconds will not overflow `i64` for billions of years (year ~2554), the cast is an unchecked narrowing conversion. More importantly, there is no assertion that the cast produced a valid value, and the `unwrap_or_default()` silently returns 0 on clock error, which would make `cutoff_secs` equal to `Some(-lookback_days * 86400)` -- a negative value that would include all commits regardless of age.
- Fix: Add a `debug_assert!` after the cast to catch future issues, and consider returning an error or at least logging when `SystemTime::now()` fails rather than defaulting to epoch 0:
```rust
let now_secs = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_err(|_| gix_err("system clock before Unix epoch"))?
    .as_secs();
debug_assert!(now_secs <= i64::MAX as u64, "epoch seconds overflow i64");
let now = now_secs as i64;
```

**Unbounded commit vector growth with lookback_days=0** - `crates/rskim-search/src/temporal/git_parser.rs:123`
**Confidence**: 82%
- Problem: When `lookback_days` is 0, the rev-walk traverses the entire repository history with no upper bound. For very large repositories (e.g. Linux kernel with 1M+ commits), this allocates a `Vec<CommitInfo>` that grows without limit, each entry containing its own `Vec<FileChangeInfo>`. This can exhaust memory on memory-constrained systems. There is no capacity hint, no upper bound, and no bail-out mechanism.
- Fix: Consider adding an optional `max_commits` upper bound to `TemporalSource::parse_history`, or at minimum pre-size the vector with `Vec::with_capacity()` using a reasonable estimate, and add a `const MAX_COMMITS` safety limit:
```rust
const MAX_COMMITS: usize = 100_000;
// ...
if commits.len() >= MAX_COMMITS {
    break;
}
```

### MEDIUM

**No assertion on `commit_count` invariant** - `crates/rskim-search/src/temporal/git_parser.rs:173-180`
**Confidence**: 88%
- Problem: `TemporalMetadata::commit_count` is set to `commits.len()` manually. This is a derived value that could drift from the actual vector length if refactored. The doc on `TemporalMetadata::commit_count` states "equals `commits.len()`" but there is no runtime assertion enforcing this invariant.
- Fix: Add a `debug_assert!` at the construction site:
```rust
let commit_count = commits.len();
debug_assert_eq!(commit_count, commits.len());
```
Or better yet, consider making `commit_count` a computed property or removing it in favor of `commits.len()` to eliminate the possibility of drift entirely.

**`timestamp as u64` cast in stability computation is unsound for negative timestamps** - `crates/rskim/src/cmd/heatmap/metrics.rs:215`
**Confidence**: 83%
- Problem: `commit.timestamp as u64` is an unchecked cast from `i64` to `u64`. `CommitInfo::timestamp` is `i64`, documented as "Unix timestamp (seconds since epoch, UTC)". While negative timestamps (pre-1970 commits) are rare, they are possible (the `git_parser.rs` documentation explicitly mentions "can be negative for pre-epoch commits"). A negative `i64` cast to `u64` wraps to a very large value, which would produce an extremely high stability score (since `days_since` would be near-zero or negative) -- the opposite of correct behavior.
- Fix: Clamp to zero before the cast:
```rust
.push(commit.timestamp.max(0) as u64);
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`unwrap_or("")` in coupling computation path-to-str conversion** - `crates/rskim/src/cmd/heatmap/metrics.rs:99`
**Confidence**: 80%
- Problem: `f.path.to_str().unwrap_or("")` silently converts non-UTF-8 paths to an empty string. Since `FileChangeInfo::path` is now a `PathBuf` (from the shared types), non-UTF-8 paths from the repository would all be grouped under the empty-string key, producing incorrect coupling metrics (all non-UTF-8 files would appear to always change together).
- Fix: Use `to_string_lossy()` for consistency with other metric functions in this file (e.g., `compute_churn` at line 36 already uses `to_string_lossy()`):
```rust
.map(|f| f.path.to_string_lossy())
```

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`parse_git_log_output` timestamp cast `as i64` from parsed `u64`** - `crates/rskim/src/cmd/heatmap/git_source.rs:216`
**Confidence**: 80%
- Problem: The `timestamp: u64` parsed from `parts[2]` is cast to `i64` via `timestamp as i64`. Since the value is parsed with `unwrap_or(0)`, this is safe for all realistic timestamps, but timestamps from unusual repos could in theory exceed `i64::MAX`, which would wrap to negative.

## Suggestions (Lower Confidence)

- **Double object decode in changed_files_for_commit** - `crates/rskim-search/src/temporal/git_parser.rs:192` (Confidence: 65%) -- `info.object()` is called once at line 133 (for commit decode) and again at line 192 (for tree access). Although gix caches objects, this is a redundant decode that could be avoided by passing the already-decoded commit object to `changed_files_for_commit`.

- **Missing `#[must_use]` on `is_fix_commit`** - Already has `#[must_use]` (Confidence: N/A -- confirmed correct, dropped).

- **Test infrastructure silently skips when git is unavailable** - `crates/rskim-search/src/temporal/git_parser_tests.rs:107-109` (Confidence: 70%) -- Tests that cannot find `git` silently return, meaning CI without git would pass all tests vacuously. Consider using `#[ignore]` or printing a warning so test reports reflect skipped tests rather than passed tests.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The implementation demonstrates solid reliability practices overall -- error handling uses Result types throughout, the `gix_err()` helper ensures no gix types leak, shallow clone handling is graceful with fallback behavior, and the `COUPLING_MAX_FILES` constant bounds the O(n^2) coupling computation. The main concerns are: (1) the unbounded commit vector when `lookback_days=0` on very large repos, (2) the unchecked `as i64`/`as u64` timestamp casts that can silently produce wrong values for edge-case inputs, and (3) the silent clock-failure fallback that makes the lookback filter ineffective rather than reporting the error. These are all fixable with small, targeted changes.
