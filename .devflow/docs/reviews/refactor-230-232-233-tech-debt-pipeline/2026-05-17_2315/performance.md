# Performance Review Report

**Branch**: refactor-230-232-233-tech-debt-pipeline -> main
**Date**: 2026-05-17T23:15

## Issues in Your Changes (BLOCKING)

### HIGH

**Mtime pre-screening not actually used for SHA skip** - `index.rs:358-367`
**Confidence**: 92%
- Problem: The `ManifestEntry` now stores `mtime`, and `WalkEntry` captures it during the walk, but the `read_and_classify` function never consults `mtime` to skip SHA computation. The comment at `index.rs:10` describes a "4-tier mtime/SHA cache" and the `manifest.rs:62-65` documents mtime as a "fast pre-screening hint to skip SHA computation when the file has not changed", yet the actual code at lines 358-367 always computes SHA-256 (`sha256_hex(content.as_bytes())`) and only then checks the manifest for a SHA match. The mtime field is stored in `ManifestEntry` and `ProcessedFile` but never read during the cache decision. SHA-256 on a 5 MiB file costs ~10 ms; with 50K files this is nontrivial overhead that the mtime hint was designed to avoid.
- Fix: Add the mtime pre-screening tier before SHA computation in `read_and_classify`:
```rust
// Tier 1: mtime hint — if mtime matches, skip SHA entirely.
if !force {
    let path_key = entry.rel_path.to_string_lossy().replace('\\', "/");
    if let Some(cached) = manifest.lookup(&path_key) {
        if entry.mtime.is_some() && entry.mtime == cached.mtime {
            // mtime match → trust cached SHA, reuse field_map.
            return Ok(ProcessedFile {
                rel_path: entry.rel_path.clone(),
                lang: entry.lang,
                content,
                sha256: cached.sha256.clone(),
                mtime: entry.mtime,
                field_map: decode_field_map(&cached.field_map),
                cache_hit: true,
            });
        }
    }
}

// Tier 2: SHA authority — mtime mismatch or no cached entry.
let sha = sha256_hex(content.as_bytes());
// ... existing SHA-based cache check ...
```

### MEDIUM

**Single-threaded producer serializes all I/O and classification** - `index.rs:226-245`
**Confidence**: 82%
- Problem: The previous implementation used `rayon::par_iter` for parallel classification across all CPU cores. The new streaming design uses a single producer thread that sequentially iterates `walk_entries`, calling `open_and_read` (disk I/O) and `run_classify` (CPU-bound tree-sitter parsing) for each file one at a time. On large repos (50K files), classification dominated the pipeline time, and this regression from N-core parallelism to 1-core serialization will be measurable. The bounded channel provides memory backpressure (a win), but the producer is the bottleneck since the consumer only does `add_file_classified` which is cheaper than `classify_source`.
- Fix: Consider spawning a thread pool (or using rayon's `par_bridge`) inside the producer, or using multiple producer threads feeding the same bounded channel. The channel is already `Send`-safe via crossbeam:
```rust
// Conceptual: use rayon inside producer for classification parallelism
let producer_handle = std::thread::spawn(move || {
    walk_entries.par_iter().for_each(|entry| {
        match read_and_classify(entry, &manifest, force, debug_enabled) {
            Ok(pf) => { let _ = tx.send(pf); }
            Err(_) => { producer_skips_clone.fetch_add(1, Ordering::Relaxed); }
        }
    });
});
```
Note: this would require the consumer to handle out-of-order FileId assignment or a re-sorting step, since `NgramIndexBuilder` may require sequential IDs. Evaluate whether the classification cost on your target repos justifies this complexity.

**Redundant `entry.metadata()` call in `classify_entry_metadata`** - `walk.rs:331-339`
**Confidence**: 85%
- Problem: `mtime_secs(entry)` at line 341 calls `entry.metadata()` a second time after the size pre-screen at line 331 already called it. While `ignore::DirEntry::metadata()` caches on some platforms, on others it may issue a second `stat(2)` syscall. On a 50K-file walk, this is 50K potential extra syscalls.
- Fix: Capture the metadata result once and reuse it:
```rust
fn classify_entry_metadata(entry: &ignore::DirEntry, root: &Path) -> MetaOutcome {
    // ... file_type checks ...
    let abs_path = entry.path();
    let lang = match Language::from_path(abs_path) { /* ... */ };

    let meta = entry.metadata().ok();

    // Fast size pre-screen.
    if let Some(ref m) = meta {
        if m.len() > MAX_FILE_BYTES {
            return MetaOutcome::Skip(SkipReason::TooLarge { path: abs_path.to_path_buf(), size: m.len() });
        }
    }

    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok().map(|d| d.as_secs()));

    // ... rest ...
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`walk_skips` vector kept alive across entire pipeline** - `index.rs:186-188`
**Confidence**: 80%
- Problem: `walk_skips` (up to 10K `SkipReason` entries, each containing a `PathBuf`) is collected in Stage 1 but only its `.len()` is used at line 188. The vector itself is not dropped until the end of `run()`. On large repos with many unsupported files this holds potentially hundreds of KB of path allocations throughout the entire streaming pipeline.
- Fix: Drop the vector immediately after extracting the count:
```rust
let walk_skip_count = walk_skips.len();
drop(walk_skips);
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Channel capacity tuning undocumented for varying workloads** - `index.rs:162` (Confidence: 65%) -- The constant `CHANNEL_CAPACITY = 64` is well-commented for worst-case (320 MiB), but there is no runtime tuning based on available system memory or file count. For very small projects (< 64 files), the channel is never full and the overhead of bounded-channel synchronization is pure cost vs. an unbounded channel or direct iteration. Consider a heuristic: `min(64, walk_entries.len())`.

- **`ProcessedFile` carries cloned `PathBuf` fields** - `types.rs:108-123` (Confidence: 68%) -- Each `ProcessedFile` clones `rel_path` from `WalkEntry`. Since the producer owns `walk_entries` and iterates by reference, the clone is necessary for the `Send` across the channel. However, if the walk entries were consumed (`into_iter`), the paths could be moved instead of cloned, saving one allocation per file.

- **`path_key` string allocation on every consumer iteration** - `index.rs:283` (Confidence: 72%) -- `pf.rel_path.to_string_lossy().replace('\\', "/")` allocates a new String on every file. On Unix (the primary target), `to_string_lossy` returns a `Cow::Borrowed` and `replace` still allocates if backslashes are absent. Consider computing `path_key` in the producer (where it is already needed for manifest lookup at line 362) and storing it in `ProcessedFile` to avoid the duplicate work.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The streaming bounded-channel design is a significant memory-management improvement over the batch `Vec<ReadFile>` approach. Peak memory is now bounded by channel capacity rather than total project size, which is the right architecture for 50K-file repos. However, two performance concerns prevent full approval:

1. The mtime pre-screening field is plumbed through the entire pipeline (walk, manifest, types) but never actually used to skip SHA computation -- the stated purpose of the feature. This is a functional gap, not just a missed optimization.

2. The move from `rayon::par_iter` classification to a single producer thread is an intentional simplification that trades CPU parallelism for memory bounds, but it should be documented as a known regression with a plan for re-introducing parallelism (e.g., rayon inside the producer).
