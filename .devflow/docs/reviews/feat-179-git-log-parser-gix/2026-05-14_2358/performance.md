# Performance Review Report

**Branch**: feat/179-git-log-parser-gix -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**Redundant object lookup in `changed_files_for_commit` -- commit decoded twice per rev-walk iteration** - `crates/rskim-search/src/temporal/git_parser.rs:133,192`
**Confidence**: 90%
- Problem: In the main loop (line 133), `info.object()` is called to decode the commit for author/message fields. Then `changed_files_for_commit` is called (line 158), which calls `info.object()` again (line 192) to get the same commit's tree. Each `info.object()` call performs an object store lookup (hash lookup + decompression), even with the 4 MiB object cache. For repositories with thousands of commits, this doubles the object-store reads on the critical traversal path.
- Fix: Pass the already-decoded commit object (or its tree OID) into `changed_files_for_commit` instead of re-fetching it. For example:

```rust
// In parse_history_impl main loop:
let commit_obj = info.object().map_err(gix_err)?;
let new_tree_id = commit_obj.tree_id().map_err(gix_err)?;
let commit_ref = commit_obj.decode().map_err(gix_err)?;
// ... extract fields from commit_ref ...

// Pass tree_id and parent_ids to avoid second object() call
let changed_files = match changed_files_for_tree(repo, new_tree_id, &info.parent_ids) {
    // ...
};
```

**Repeated `to_string_lossy().into_owned()` allocations in heatmap hot loops (6 call sites)** - `crates/rskim/src/cmd/heatmap/metrics.rs:36,211,268,337` and `crates/rskim/src/cmd/heatmap/mod.rs:223,450`
**Confidence**: 85%
- Problem: The migration from `FileChange { path: String }` to `FileChange { path: PathBuf }` introduced `to_string_lossy().into_owned()` calls inside every inner loop of every metric computation function (`compute_churn`, `compute_stability`, `compute_authors`, `compute_fix_after_touch`, `compute_encapsulation`). Git paths are always UTF-8-valid on all platforms git supports, so `to_string_lossy()` performs a full UTF-8 validation scan plus a heap allocation (`into_owned()`) on every file in every commit. In `compute_stability` (line 211), the same path is converted twice per iteration (once for `file_commits`, once conditionally for `file_fix_count`), and the second conversion was partially optimized by cloning `path_str` but the first conversion still allocates. Across all five metric functions, this is O(total_files_across_all_commits) unnecessary allocations per function, times 5 functions.
- Fix: Two options, from least to most impactful:
  1. **Quick**: Use `file.path.to_str().unwrap_or("")` consistently (as already done in `compute_coupling` line 99) to avoid the `into_owned()` allocation. The `&str` borrows from the `PathBuf` with zero allocation.
  2. **Better**: Since all consumers of `FileChangeInfo` outside the `temporal` module use `String` paths, consider keeping `path: String` in the shared type, or adding a `path_str(&self) -> &str` method that caches or directly returns the inner `OsStr` as `&str`. Git paths are guaranteed UTF-8 by git itself.

### MEDIUM

**Double allocation in tree-diff closure: `to_str_lossy().into_owned()` then `PathBuf::from()`** - `crates/rskim-search/src/temporal/git_parser.rs:229,235`
**Confidence**: 82%
- Problem: Inside the tree-diff callback (called once per changed file per commit), `location.to_str_lossy().into_owned()` allocates a `String` (line 229), then `PathBuf::from(path)` on line 235 takes ownership of that `String` by converting it into a `PathBuf`. The intermediate `String` allocation is immediately consumed. While individually small, this runs in the inner-most loop of the traversal.
- Fix: Convert directly from the `BStr` to `PathBuf` without the intermediate `String`:

```rust
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

// On Unix (macOS/Linux -- the target platforms):
let path = PathBuf::from(OsStr::from_bytes(location.as_ref()));
// Or, since git paths are UTF-8:
let path = PathBuf::from(location.to_str_lossy().as_ref());
// (to_str_lossy returns Cow, PathBuf::from(&str) avoids the intermediate String)
```

Note: The `Cow<str>` returned by `to_str_lossy()` can be passed to `PathBuf::from()` via `as_ref()` without calling `.into_owned()`, avoiding one allocation when the path is already valid UTF-8 (which is virtually always the case for git paths).

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`unwrap_or("")` in `compute_coupling` silently drops non-UTF-8 paths** - `crates/rskim/src/cmd/heatmap/metrics.rs:99`
**Confidence**: 80%
- Problem: `f.path.to_str().unwrap_or("")` maps non-UTF-8 paths to the empty string `""`. If two files have non-UTF-8 paths, they would collide on the same empty-string key, producing incorrect coupling metrics. While non-UTF-8 git paths are extremely rare, the other metric functions use `to_string_lossy()` which replaces invalid sequences with a replacement character -- at least producing distinct keys. The inconsistency across functions means the same edge case is handled differently in different metrics.
- Fix: Use `to_string_lossy()` consistently across all functions, or document the UTF-8 assumption with a debug assertion.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Consider pre-sizing `commits` Vec** - `crates/rskim-search/src/temporal/git_parser.rs:123` (Confidence: 65%) -- `Vec::new()` starts at capacity 0 and will re-allocate as commits are pushed. For repositories with many commits (thousands+), pre-sizing with `Vec::with_capacity(hint)` based on the lookback window could reduce reallocations. However, the optimal hint is hard to compute without walking first, so this is marginal.

- **Object cache size may be undersized for large repos** - `crates/rskim-search/src/temporal/git_parser.rs:79` (Confidence: 62%) -- The 4 MiB object cache is reasonable for small-to-medium repos, but for large monorepos with many tree objects, a larger cache (e.g., 16-64 MiB) could reduce redundant decompression. The `_if_unset` suffix means this is a safe default that users can override, so the risk is low.

- **`first_parent_only()` is a good performance choice** - `crates/rskim-search/src/temporal/git_parser.rs:118` (Confidence: 70%) -- This is called out positively: `first_parent_only()` avoids exponential blowup in merge-heavy histories. However, it means commits from merged branches are invisible to the temporal parser, which may under-count file changes. This is a correctness/performance tradeoff that should be documented for consumers who expect full history. (Already partially documented in the module doc, but not in the trait's doc contract.)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The core gix traversal is well-architected -- `ByCommitTimeCutoff` sorting for efficient lookback filtering, `first_parent_only()` to avoid merge explosion, and a 4 MiB object cache. The main performance concerns are: (1) the redundant `info.object()` call that doubles object-store lookups on the hot traversal path, and (2) the systematic `to_string_lossy().into_owned()` pattern in all five heatmap metric functions, which introduces O(N) unnecessary heap allocations per function where N is total file entries across all commits. Both are straightforward to fix and would meaningfully improve throughput on large repositories.
