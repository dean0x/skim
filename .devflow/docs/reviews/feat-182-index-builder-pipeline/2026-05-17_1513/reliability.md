# Reliability Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**Mutex unwrap in parallel walker can panic and poison locks** - `walk.rs:261,272,276,287`
**Confidence**: 85%
- Problem: Multiple `.lock().unwrap()` calls inside the parallel walker closure. If any thread panics (e.g., during `classify_entry` or inside the closure), the Mutex becomes poisoned and subsequent `.unwrap()` calls on other threads will panic with "PoisonError". This cascading panic could abort the entire process rather than gracefully degrading.
- Fix: Use `.lock().unwrap_or_else(|e| e.into_inner())` to recover from poisoned locks, or handle the poison error explicitly. Given this is a diagnostic-only data path (files and skipped vectors), recovering the inner data is safe:
```rust
files.lock().unwrap_or_else(|e| e.into_inner()).push(file);
```

**Arc::try_unwrap + expect can panic on incomplete parallel walk** - `walk.rs:300-307`
**Confidence**: 82%
- Problem: `Arc::try_unwrap(...).expect("all parallel walker threads completed")` will panic if any Arc clone still exists (e.g., if `build_parallel().run()` returned before all threads dropped their Arc clones due to a panic in the thread pool). The expect message claims all threads completed, but that invariant is not verified before the assertion.
- Fix: Use `match` and return an error rather than panic:
```rust
let mut files = Arc::try_unwrap(files)
    .map_err(|_| anyhow::anyhow!("walker threads did not complete cleanly"))?
    .into_inner()
    .unwrap_or_else(|e| e.into_inner());
```

### MEDIUM

**Relaxed ordering on file_count allows over-collection beyond max_files** - `walk.rs:259,271`
**Confidence**: 85%
- Problem: `file_count.load(Ordering::Relaxed)` and `file_count.fetch_add(1, Ordering::Relaxed)` use Relaxed memory ordering. Multiple threads can simultaneously pass the `>= max_files` check before any of them completes `fetch_add`. The code addresses this with `files.truncate(max_files)` at line 312, but this means work was done (file I/O, SHA-256, classification) unnecessarily. On very large repos, this could mean hundreds of extra file reads above the cap.
- Fix: This is acknowledged in the comment at line 309-311, and the truncation is the mitigation. Consider using `Ordering::Acquire` for the load and `Ordering::Release` for the fetch_add to reduce the TOCTOU window, though full elimination requires `compare_exchange`. The current approach is functionally correct but wasteful under contention. No code change strictly required — the comment documents the trade-off.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **No timeout/watchdog on parallel walk** - `walk.rs:252` (Confidence: 65%) — If a file system is hung (NFS, FUSE), the parallel walker threads could block indefinitely. Consider a wall-clock timeout around the walk phase for production safety.

- **No upper bound on individual line length during manifest parse** - `manifest.rs:152-196` (Confidence: 60%) — While the file size is capped at 256 MiB, `BufReader::lines()` will still allocate individual lines up to that size. A single line of 256 MiB is unlikely but technically possible within the current guards.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The code demonstrates strong reliability awareness overall: bounded iteration (MAX_ANCESTORS, MAX_SKIP_REASONS, MAX_MANIFEST_ENTRIES), file size guards (MAX_FILE_BYTES, MAX_MANIFEST_FILE_BYTES), atomic writes with fsync, TOCTOU race mitigation, compile-time size assertions, and a clean separation of concerns. The main reliability gap is the panic-on-unwrap pattern in the parallel walker — poisoned locks and failed `Arc::try_unwrap` should be handled gracefully (return error) rather than crashing the entire process. This is the difference between a tool that reports "indexing failed for N files" and one that aborts mid-run with a panic backtrace.
