# Performance Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**Mutex contention on every accepted file in parallel walker** - `walk.rs:270-272`
**Confidence**: 85%
- Problem: Inside the parallel walker closure, every accepted file acquires `files.lock().unwrap()` (line 272) to push a single `ReadFile`. Each `ReadFile` contains the entire file content (`String`), SHA-256 hash, and path. With rayon's thread pool processing entries concurrently, every thread contends on this single mutex for every file acceptance. On repos with thousands of source files, this serializes the hot path and negates much of the parallelism benefit. The `skipped` mutex (line 275) has a similar pattern but is less impactful since skipped entries are smaller and less frequent.
- Fix: Use thread-local collection with a final merge. Each worker thread accumulates into a local `Vec<ReadFile>`, then after the parallel walk completes, merge all local vectors into the final result. Alternatively, use a lock-free concurrent collection like `crossbeam::deque` or simply accept the lock (the I/O cost of reading files likely dominates the lock contention in practice). If staying with the current design, consider batching: accumulate locally in the closure and push in bulk less frequently.

```rust
// Alternative: use a channel instead of mutex for accepted files
let (tx, rx) = std::sync::mpsc::channel::<ReadFile>();
// In closure: tx.send(file).unwrap();
// After walk: let files: Vec<_> = rx.into_iter().collect();
```

**sync_data() called on every manifest save — unnecessary I/O latency** - `manifest.rs:277`
**Confidence**: 82%
- Problem: `tmp.as_file().sync_data()` forces a physical disk flush (fsync) before the rename. While the comment explains this guards against power loss between page-cache flush and physical write, this is a search index cache — not a database or financial ledger. The manifest can always be regenerated from source files. The fsync adds significant latency (potentially 5-50ms on SSD, 50-200ms on HDD) to every index build. For a cache file that is trivially regenerable, this durability guarantee is unnecessarily expensive.
- Fix: Remove the `sync_data()` call. The atomic rename via `NamedTempFile::persist()` already provides crash consistency against partial writes — the old manifest remains intact if the process dies before rename completes. Power-loss corruption of a regenerable cache is an acceptable risk for a developer tool.

```rust
// Remove this block:
// tmp.as_file().sync_data()?;
```

### MEDIUM

**SHA-256 computed for every file on every build (no early-exit on mtime)** - `walk.rs:202` / `index.rs:162`
**Confidence**: 80%
- Problem: The pipeline always reads the full content of every source file and computes SHA-256 (`sha256_hex` in `walk.rs:202`) during the walk phase, even for incremental builds where most files are unchanged. The SHA is then compared against the manifest. For a 50K-file repo where 99% of files are unchanged between builds, this means reading ~50K files and computing ~50K SHA-256 hashes just to discover that almost nothing changed. An mtime+size check in the manifest could skip the full read+hash for unchanged files.
- Fix: Add `mtime` and `size` fields to `ManifestEntry`. During the walk phase, compare the file's mtime and size against the manifest entry before reading. If both match, skip the full read and SHA computation — reuse the cached entry directly. This would reduce incremental build I/O from O(all files) to O(changed files). Note: this is an enhancement suggestion — the current approach is correct and safe, just slower than necessary for large incremental builds.

**Manifest entries sorted on every save** - `manifest.rs:263-268`
**Confidence**: 80%
- Problem: `save()` collects all keys into a `Vec<&str>`, sorts them with `sort_unstable()`, then iterates to serialize. For 60K entries (the max cap), this is a sort of 60K strings on every build. While `sort_unstable` is efficient, this sort is purely for deterministic output and happens on every successful build, adding O(n log n) string comparisons to the write path.
- Fix: This is acceptable for correctness and debuggability. However, if profiling shows this is a bottleneck for very large repos, consider maintaining entries in a `BTreeMap` instead of `HashMap` (sorted on insertion, no sort-on-save needed). The HashMap lookup performance for the classify phase likely outweighs the save-time sort cost at current scales.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`String::from_utf8` with expect in sha256_hex hot path** - `walk.rs:388`
**Confidence**: 82%
- Problem: The change from `unsafe { String::from_utf8_unchecked(hex) }` to `String::from_utf8(hex).expect(...)` adds a UTF-8 validation pass over the 64-byte hex string on every file. While 64 bytes is trivial, this function is called once per indexed file (up to 50K times). The validation is a memchr scan that is strictly unnecessary — the nibble table provably produces only ASCII hex bytes. The old `unsafe` approach was actually sound and avoided the redundant check.
- Fix: This is a safety-vs-performance tradeoff. The overhead is negligible (64 bytes validated 50K times = ~3.2MB of redundant scanning, which completes in microseconds on modern CPUs). The safer version is the correct choice here — the performance cost is effectively zero. No action needed, but document that this was a deliberate safety choice.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **PathBuf allocations in SkipReason variants** - `walk.rs:154,177,182,188,198` (Confidence: 65%) — Every skipped file allocates a `PathBuf` via `abs_path.to_path_buf()`. In repos with many unsupported files, this could mean tens of thousands of path allocations for diagnostics that are only capped at 10K. The `MAX_SKIP_REASONS` cap mitigates this, but the allocations happen before the cap check inside `classify_entry`.

- **`root_buf.clone()` per rayon thread** - `walk.rs:257` (Confidence: 60%) — Each parallel worker closure clones the `root_buf: PathBuf` on thread spawn. With rayon's default thread pool, this is only ~8-16 clones, so negligible. An `Arc<Path>` would avoid even that, but the cost is trivial.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The parallel walker conversion from sequential `build()` to `build_parallel()` is a significant performance improvement for the walk phase. The incremental SHA-based caching in the classify phase avoids redundant tree-sitter work effectively. The main concerns are: (1) mutex contention pattern that may limit parallel scaling on large repos, and (2) unnecessary fsync on a regenerable cache file. Both are addressable without architectural changes. The overall design is sound for the target workload (50K file cap, developer machines with SSDs).
