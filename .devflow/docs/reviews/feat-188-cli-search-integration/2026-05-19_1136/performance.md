# Performance Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### HIGH

**Duplicate manifest load on the query hot path** - `query.rs:55,70`
**Confidence**: 92%
- Problem: `execute_query` calls `auto_refresh_if_stale()` at line 55, which internally calls `FileManifest::load()` (staleness.rs:161) to check the stored git HEAD. Then at line 70, `execute_query` calls `FileManifest::load()` again to build the sorted paths for result resolution. On a project with thousands of indexed files, this parses the entire JSONL manifest file twice in the same request path -- two full file reads, two full JSON parse passes, two HashMap constructions.
- Impact: For a 10K-file manifest, this doubles the manifest parse cost per query. The manifest is the largest sidecar file in the search pipeline. On spinning disks or cold filesystem caches this is especially costly.
- Fix: Have `auto_refresh_if_stale` return the loaded manifest (or accept a pre-loaded one), so `execute_query` can reuse it:

```rust
// staleness.rs — return the manifest when index is current
pub(super) fn auto_refresh_if_stale(
    root: &Path,
    cache_dir: &Path,
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<(bool, FileManifest)> {
    let index_path = cache_dir.join("index.skidx");
    if !index_path.exists() {
        // ... rebuild, then reload manifest after build
    }

    let manifest = FileManifest::load(root.to_path_buf(), cache_dir.to_path_buf())?;
    let stored = manifest.stored_git_head();
    // ... check staleness using `manifest` directly ...
    // On Current: return (false, manifest) — caller reuses it
    // On rebuild: reload after build, return (true, fresh_manifest)
}

// query.rs — reuse the manifest
let (_, manifest) = auto_refresh_if_stale(root, cache_dir, analytics)?;
// ... skip the second FileManifest::load() at line 70
```

---

**Per-result file I/O in snippet extraction (sequential reads)** - `query.rs:74` calling `snippet.rs:101-146`
**Confidence**: 85%
- Problem: `resolve_paths_and_snippets` iterates search results sequentially and calls `extract_snippet` for each one, which performs up to 2 syscalls per result: `fs::metadata` (mtime check at line 116) and `fs::read` (file content at line 127). With the default limit of 20 results, this is 20-40 sequential syscalls in the query hot path. These are independent reads that could overlap.
- Impact: On NFS, networked filesystems, or cold disk caches, sequential file reads add ~100us-10ms each. For 20 results this can add 2-200ms to query latency, potentially exceeding the 50ms target for the search response.
- Fix: For the default limit of 20 this is acceptable for local SSDs (20 x ~10us = ~200us). However, consider a note/TODO acknowledging this as a future optimization opportunity if users increase --limit or latency targets tighten. For now, this is architecturally fine given the bounded limit.

### MEDIUM

**`extract_context_window` collects all lines then indexes a window** - `snippet.rs:66`
**Confidence**: 82%
- Problem: `content.lines().collect()` at line 66 allocates a `Vec<&str>` for every line in the file, but only 7 lines (3 context + 1 match + 3 context) are ever used. For a 10K-line file this allocates a 10K-element vector just to index into 7 entries.
- Impact: Unnecessary allocation proportional to file size. For the typical case (files under 1K lines, 20 results), total overhead is modest (~160KB of pointers). But for large files this scales linearly.
- Fix: Skip lines until reaching the window start, then collect only the needed lines:

```rust
pub(super) fn extract_context_window(
    content: &str,
    match_line: u32,
    context: u32,
) -> Vec<SnippetLine> {
    let match_line = match_line.max(1);
    let start = match_line.saturating_sub(context).max(1);
    let end = match_line.saturating_add(context);

    content
        .lines()
        .enumerate()
        .skip((start - 1) as usize)
        .take((end - start + 1) as usize)
        .map(|(i, line)| {
            let ln = (i as u32) + 1;
            SnippetLine {
                line_number: ln,
                content: line.to_string(),
                is_match: ln == match_line,
            }
        })
        .collect()
}
```

This allocates only the 7 needed `SnippetLine`s regardless of file size.

---

**`is_hex_sha` uses `.chars()` iterator for ASCII-only check** - `staleness.rs:136`
**Confidence**: 80%
- Problem: `s.chars().all(|c| c.is_ascii_hexdigit())` decodes UTF-8 codepoints, but SHA-1 hex is pure ASCII. Using `.bytes().all(|b| b.is_ascii_hexdigit())` avoids the UTF-8 decode overhead.
- Impact: Marginal -- this runs once per query during staleness check. The 40-byte string is tiny. But it is a micro-optimization that makes the code's intent clearer (we expect ASCII bytes, not Unicode codepoints).
- Fix:
```rust
fn is_hex_sha(s: &str) -> bool {
    s.len() == 40 && s.as_bytes().iter().all(|b| b.is_ascii_hexdigit())
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`check_staleness` and `run_stats` both load the manifest redundantly** - `mod.rs:259,261`
**Confidence**: 85%
- Problem: In `run_stats`, line 259 loads `FileManifest::load()` to extract `git_head`, and then line 261 calls `check_staleness()` which internally loads `FileManifest::load()` again (staleness.rs:161). Same double-load pattern as the query path.
- Impact: This is a diagnostic command (`--stats`), not the query hot path, so user-facing latency impact is lower. Still, on large projects this parses the manifest twice unnecessarily.
- Fix: Extract the manifest load once and pass it to a staleness check variant that accepts a pre-loaded manifest, or inline the staleness comparison since `run_stats` already has the git_head.

---

**`sorted_paths()` allocates and sorts on every query** - `manifest.rs:268-272`
**Confidence**: 82%
- Problem: `sorted_paths()` collects all keys into a `Vec<&str>` and calls `sort_unstable()` every time it is invoked. On the query path this runs once per query. For a 10K-file index, this is a 10K-element sort.
- Impact: `sort_unstable` on 10K strings is ~100-200us. Not a bottleneck today but it compounds with the manifest double-load. If the manifest were cached or reused across queries (e.g., in a server mode), pre-computing the sorted paths on load would eliminate repeated sorts.
- Fix: Consider caching the sorted paths lazily inside `FileManifest` (e.g., `OnceCell<Vec<String>>`) or computing them at load time. Low priority given single-shot CLI usage.

## Pre-existing Issues (Not Blocking)

(none found)

## Suggestions (Lower Confidence)

- **`packed-refs` full scan for ref resolution** - `staleness.rs:114-129` (Confidence: 65%) -- `resolve_symbolic_ref` reads the entire `packed-refs` file and linearly scans all lines to find the matching ref. In repositories with thousands of refs (common in monorepos), this is O(n) on every staleness check. A future optimization could binary-search the sorted packed-refs file or cache the result.

- **Blocking index rebuild on query path** - `staleness.rs:222-248` (Confidence: 70%) -- When the index is stale, `auto_refresh_if_stale` performs a full synchronous rebuild before returning results. For large projects this could take seconds, blocking the query. This is arguably correct behavior (return fresh results), but a future enhancement could return stale results with a warning while rebuilding in the background.

- **`match_positions` cloned into `ResolvedResult`** - `query.rs:116` (Confidence: 62%) -- `r.match_positions.clone()` copies byte-range vectors for every result, but the field is marked `#[serde(skip)]` and is not used after construction. If it is truly dead, removing it would eliminate the clone allocation.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | - | 0 | 2 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The query hot path has a clear redundant manifest load that doubles I/O for the largest sidecar file. The snippet extraction collects all file lines when only ~7 are needed. Both are straightforward fixes. The sequential per-result file reads are acceptable given the bounded default limit of 20. The staleness detection via direct git file I/O (no subprocess) is a good design choice for latency. Overall the architecture is sound with a well-bounded data flow; the main issues are redundant I/O that can be eliminated by threading the manifest through the call chain.
