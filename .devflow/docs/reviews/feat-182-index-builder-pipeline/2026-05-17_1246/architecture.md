# Architecture Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17
**Commits reviewed**: 8a1bef5, 7a7a39e, 3d2a37b, cdc0a22

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Error discrimination via string matching in walk.rs** - `walk.rs:183-184`
**Confidence**: 82%
- Problem: The `open_and_read` error path uses `e.to_string().contains("too large")` to distinguish the custom "too large" error from other `ErrorKind::Other` errors. This couples the caller to an arbitrary error message string defined one function away. If the message is ever changed, the classification silently breaks and errors are misreported as `SkipReason::ReadError` instead of `SkipReason::TooLarge`.
- Fix: Use a dedicated error type or a custom `io::Error` with a typed payload instead of string matching:
```rust
// In open_and_read:
use std::io;

#[derive(Debug)]
struct FileTooLarge;
impl std::fmt::Display for FileTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "too large")
    }
}
impl std::error::Error for FileTooLarge {}

fn open_and_read(path: &Path) -> io::Result<String> {
    // ...
    if size > MAX_FILE_BYTES {
        return Err(io::Error::new(io::ErrorKind::Other, FileTooLarge));
    }
    // ...
}

// In caller:
} else if e.get_ref().map_or(false, |inner| inner.is::<FileTooLarge>()) {
    skipped.push(SkipReason::TooLarge { ... });
}
```
Alternatively, since `open_and_read` is private and adjacent to its single call site, returning a domain enum (e.g., `enum ReadOutcome { Ok(String), TooLarge, NonUtf8, IoError(io::Error) }`) would be cleaner and eliminate type-erasure entirely.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Manifest coherence gap on build failure** - `index.rs:228-229` (Confidence: 70%) -- If `builder.build()` fails after partial `add_file_classified` calls, the function returns `Err` and `new_manifest.save()` is never called, leaving the old manifest on disk. This is likely fine (stale cache just triggers a full re-classify next run), but documenting this invariant in the module doc or adding a brief comment would clarify intent for future maintainers.

- **`walk_and_read` does not pre-size the `files` Vec** - `walk.rs:100` (Confidence: 64%) -- With 50K files possible, the Vec grows via repeated doublings. Pre-allocating `Vec::with_capacity(max_files.min(8192))` would reduce allocations for large repos. Minor performance consideration, not correctness.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

---

## Architecture Assessment

**Strengths of this PR:**

1. **Clean module decomposition** -- The search pipeline is split into `types` (pure data), `walk` (I/O + traversal), `manifest` (persistence), and `index` (orchestration). Each module has a single responsibility and clear boundaries.

2. **Correct dependency direction** -- The orchestrator (`index.rs`) depends on `walk`, `manifest`, and `types`, but none of those modules depend on each other or the orchestrator. Dependencies point inward toward abstractions (`rskim_search` crate types like `FileId`, `SearchField`).

3. **Two-phase pipeline design** -- Parallel classification (CPU-bound, embarrassingly parallel via rayon) followed by sequential builder accumulation (NgramIndexBuilder is !Sync) is the right architectural choice. Data parallelism where safe, sequential where required.

4. **Atomic write ordering** -- The `.skpost` / `.skidx` are written by `builder.build()` first, then the manifest is written last. This "manifest marks coherence" invariant ensures readers never observe a partial index.

5. **Fail-soft error handling** -- `run_classify` falls back to an empty field map on error rather than aborting the entire pipeline. This is the correct trade-off for an indexer (partial results are better than no results).

6. **Clean separation of CLI parsing from business logic** -- The `IndexCli` struct handles argument parsing via clap derive, then `into_config()` converts it to the domain `IndexConfig` type. CLI concerns do not leak into `build_index`.

7. **Deep module pattern** -- `FileManifest` encapsulates all persistence complexity (JSONL format, header versioning, atomic writes, wrong-root detection, best-effort recovery) behind a simple `new/load/insert/lookup/save` interface.

**The single blocking issue (string-based error discrimination) is MEDIUM severity and does not warrant blocking the merge if addressed promptly in a follow-up.**
