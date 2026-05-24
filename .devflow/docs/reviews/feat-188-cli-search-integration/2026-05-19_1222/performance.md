# Performance Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19
**Scope**: Incremental review of 459d0af...HEAD (2 commits)

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Duplicate `std::fs::metadata` syscall in `extract_snippet`** - `crates/rskim/src/cmd/search/snippet.rs:124,137`
**Confidence**: 90%
- Problem: The newly added size guard at line 137 calls `std::fs::metadata(&abs_path)` independently from the mtime guard at line 124, which also calls `std::fs::metadata(&abs_path)`. When both guards fire (the common case: manifest entry has an mtime), this issues two separate `stat(2)` syscalls for the same file per search result. With `--limit 20`, that is 40 syscalls instead of 20.
- Impact: Two stat syscalls per result instead of one. On NFS or other high-latency filesystems, this can add measurable latency to query response time.
- Fix: Hoist a single metadata call and share the result between both guards:
```rust
let meta = std::fs::metadata(&abs_path).ok();

// Mtime guard
if let Some(stored_mtime) = manifest_entry.and_then(|e| e.mtime) {
    let current_mtime = meta.as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    if current_mtime != Some(stored_mtime) {
        return SnippetOutcome::Stale;
    }
}

// Size guard
const MAX_SNIPPET_FILE_BYTES: u64 = 5 * 1024 * 1024;
let file_size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
if file_size > MAX_SNIPPET_FILE_BYTES {
    return SnippetOutcome::Unavailable;
}
```

## Issues in Code You Touched (Should Fix)

### LOW

**Double iteration over lines in `extract_context_window`** - `crates/rskim/src/cmd/search/snippet.rs:66,83`
**Confidence**: 80%
- Problem: `content.lines().count()` (line 66) iterates the entire string to count lines, then `content.lines().skip(skip).take(take)` (line 83) iterates again. For typical source files (<1000 lines) this is negligible, but this function runs once per search result (up to 20 times per query), and the 5 MB size guard admits files up to ~100K lines.
- Impact: Low in practice -- the double scan is O(n) in file length but avoids the old `Vec<&str>` allocation (a net improvement). Still, a single-pass approach is possible.
- Fix (optional): For files within the snippet window, the line count only serves to clamp `match_line` to `[1, total_lines]`. A single-pass approach could iterate once, breaking early after the window end:
```rust
// Single-pass: iterate once, collecting only the window.
let match_line = match_line.max(1);
let start = match_line.saturating_sub(context).max(1);
let tentative_end = match_line.saturating_add(context);

let skip = (start - 1) as usize;
let max_take = (tentative_end - start + 1) as usize;

let result: Vec<SnippetLine> = content.lines()
    .enumerate()
    .skip(skip)
    .take(max_take)
    .map(|(idx, line_text)| {
        let ln = (idx + 1) as u32;
        SnippetLine {
            line_number: ln,
            content: line_text.to_string(),
            is_match: ln == match_line,
        }
    })
    .collect();
```
This eliminates the pre-count entirely and still clamps naturally (`.take()` stops at EOF).

## Pre-existing Issues (Not Blocking)

(none found at CRITICAL severity in unchanged code)

## Suggestions (Lower Confidence)

- **BTreeMap vs HashMap tradeoff in manifest** - `crates/rskim/src/cmd/search/manifest.rs:112` (Confidence: 65%) -- Switching from `HashMap` to `BTreeMap` trades O(1) lookup/insert for O(log n) lookup/insert. The benefit is free sorted iteration (no `sort_unstable()` in `sorted_paths` and `save`). For typical project sizes (<50K entries), insertion overhead is likely negligible and the sort elimination is a clear win, but for very large manifests the constant-factor difference could matter. Worth profiling if >50K-file projects are common.

- **Advisory file lock blocks indefinitely** - `crates/rskim/src/cmd/search/index.rs:176` (Confidence: 60%) -- `lock_file.lock()` blocks without a timeout. If a zombie build process holds the lock (e.g., SIGKILL leaves a stale flock on Linux -- though flock is released on process exit), another `skim search` query will hang. This is unlikely in practice (OS flock semantics release on close/exit), but a timeout-and-retry pattern would be more robust for CLI tools where responsiveness is expected.

- **`auto_refresh_if_stale` re-loads manifest after rebuild** - `crates/rskim/src/cmd/search/staleness.rs:308` (Confidence: 70%) -- After a successful `build_index`, the function loads the manifest from disk again via `FileManifest::load`. The `build_index` function already builds the manifest in memory (in `Pipeline::run`), but does not return it. Returning the manifest from `build_index` would avoid this extra I/O round-trip on rebuild. This is a cold-path optimization (only on stale/first-build), so impact is low.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 1 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 8/10

The incremental changes are overwhelmingly positive for performance: the `HashMap` to `BTreeMap` swap eliminates redundant sorting on every `sorted_paths()` and `save()` call; the snippet `extract_context_window` rewrite avoids a full-file `Vec<&str>` allocation; the size guard bounds peak memory during snippet extraction; the advisory build lock prevents concurrent index corruption; and `check_staleness` / `auto_refresh_if_stale` now return the loaded manifest to callers, eliminating a duplicate manifest load in both `run_stats` and `execute_query`. The only actionable finding is the duplicate `metadata` syscall in `extract_snippet` introduced by the new size guard.

**Recommendation**: APPROVED_WITH_CONDITIONS
- Fix the duplicate `std::fs::metadata` syscall in `extract_snippet` (MEDIUM/blocking) before merge.
