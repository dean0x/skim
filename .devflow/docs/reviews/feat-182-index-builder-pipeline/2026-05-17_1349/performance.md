# Performance Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**All file contents held in memory simultaneously** - `walk.rs:123` / `index.rs:162`
**Confidence**: 90%
- Problem: `walk_and_read` reads every accepted file into a `Vec<ReadFile>` where each `ReadFile` owns its full `content: String`. With a default cap of 50,000 files at up to 5 MB each, the theoretical peak RSS is ~250 GB. In practice on a mid-size repo (10,000 files averaging 20 KB), this is ~200 MB of string content held simultaneously. The content is needed downstream in the classify phase (parallel) and the build phase (sequential), so it cannot be released until the build loop completes.
- Fix: This is a design-level concern that may be acceptable for v1 given the 5 MB per-file and 50,000-file caps, but should be documented as a known memory ceiling. For a future iteration, consider a streaming two-pass approach: pass 1 walks + hashes (no content), pass 2 reads content on-demand during classify+build. Alternatively, memory-map files via `memmap2` to let the OS page manager handle pressure.

**Sequential walker with sorted output forces single-threaded I/O** - `walk.rs:143`
**Confidence**: 82%
- Problem: `WalkBuilder::sort_by_file_path(|a, b| a.cmp(b))` disables the `ignore` crate's parallel directory traversal. The `ignore::WalkBuilder` supports `.threads(n)` for parallel walking, but sorting forces sequential iteration. On repos with 50,000 files, the walk+read phase (file open, metadata check, read, SHA-256 hash) is entirely single-threaded and I/O-bound.
- Fix: Consider removing the sort at the walker level. If deterministic ordering is needed for the manifest, sort the `ReadFile` vec after collection instead. This allows the walker to use `WalkBuilder::build_parallel()` for concurrent directory traversal and file reading, which is the `ignore` crate's primary performance feature. The downstream classify phase is already parallelized with rayon, but the I/O-heavy walk phase is not.

```rust
// Instead of sorting in the walker:
// builder.sort_by_file_path(|a, b| a.cmp(b));

// Collect unsorted, then sort after:
let mut files = walk_and_read_parallel(&config.root, max_files)?;
files.0.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
```

### MEDIUM

**SHA-256 computed for every file on every build** - `walk.rs:237`
**Confidence**: 85%
- Problem: Even in the incremental path, every file's full content is read and SHA-256 hashed (line 237). The SHA-256 is used to detect content changes vs. the manifest cache. For large repos where most files are unchanged, this is redundant CPU work. A cheaper pre-screen using file mtime (already available from `entry.metadata()`) could skip the hash for files whose mtime matches the manifest, at the cost of one more field in `ManifestEntry`.
- Fix: Add an `mtime` field to `ManifestEntry`. During the walk phase, compare mtime first; only read + hash the file if mtime changed. This converts the incremental no-op case from O(n * file_size) read + O(n * file_size) SHA-256 to O(n * stat), which is dramatically faster for the common "nothing changed" incremental build.

```rust
// In ManifestEntry, add:
pub mtime_secs: i64,
pub mtime_nanos: u32,

// In walk, skip read+hash when mtime matches:
if let Some(entry) = manifest.lookup(path_key)
    && entry.mtime_secs == meta_mtime_secs
    && entry.mtime_nanos == meta_mtime_nanos
{
    // Reuse cached content hash and field_map — skip file read entirely
}
```

**Manifest entry path cloned on insert** - `manifest.rs:185`
**Confidence**: 80%
- Problem: `ManifestEntry::path` is cloned for the HashMap key on every `insert()`. In `index.rs:224-229`, `path_keys[idx]` is already `take`-d via `std::mem::take`, but then `entry.path.clone()` happens again inside `HashMap::insert`. With 50,000 files, this is 50,000 unnecessary string clones (one per file path).
- Fix: The `insert` method could take ownership of the key separately, or the HashMap could use `entry.path` as the key directly since it is consumed. The current pattern clones `entry.path` for the key while also storing it inside the value.

```rust
// Current (clones the path):
pub(super) fn insert(&mut self, entry: ManifestEntry) {
    self.entries.insert(entry.path.clone(), entry);
}

// Better: extract the key before inserting
pub(super) fn insert(&mut self, entry: ManifestEntry) {
    let key = entry.path.clone(); // still one clone, but consider...
    self.entries.insert(key, entry);
}
// Or restructure to avoid storing path in both key and value.
```

**`decode_field_map` allocates a new Vec on every cache hit** - `index.rs:205`
**Confidence**: 80%
- Problem: On the incremental path, when a manifest cache hit occurs, `decode_field_map(&entry.field_map)` allocates a fresh `Vec<(Range<usize>, SearchField)>` for each cache-hit file. These are then used read-only in the sequential build phase. For a fully-cached 50,000-file repo, this is 50,000 Vec allocations inside the rayon parallel iterator that could be avoided by storing the decoded form or using a cow pattern.
- Fix: Acceptable for v1 since the allocation is proportional to the number of field ranges (typically < 100 per file), but consider caching the decoded form in the manifest struct for repeated lookups.

## Issues in Code You Touched (Should Fix)

_(none identified)_

## Pre-existing Issues (Not Blocking)

_(none identified at CRITICAL severity in unchanged code)_

## Suggestions (Lower Confidence)

- **Sorted manifest save may be unnecessary** - `manifest.rs:229` (Confidence: 65%) -- `paths.sort_unstable()` during `save()` creates a sorted Vec of all path keys for deterministic output. For 50,000 entries this is a modest cost, but if determinism is not required for correctness (the manifest is keyed by path, not position), this sort could be skipped.

- **`to_string_lossy().replace('\\', "/")` allocates twice** - `index.rs:188` (Confidence: 70%) -- Each path key goes through `to_string_lossy()` (potential allocation) then `.replace('\\', "/")` (always allocates a new String). On Unix, backslashes are rare, so this always allocates an unnecessary copy. Consider using `to_string_lossy().into_owned()` on Unix, or a conditional replace.

- **`entry.metadata()` fallthrough on error** - `walk.rs:192` (Confidence: 60%) -- When `entry.metadata()` fails, the code falls through to `open_and_read` which will do its own metadata check. This is the correct fail-soft behavior, but on repos where metadata access fails frequently (e.g., network mounts with permission issues), every failed file hits two syscalls instead of one. Low likelihood in practice.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The pipeline demonstrates solid performance awareness: hoisted env-var checks, pre-allocated buffers, TOCTOU-safe file reading, parallel classification via rayon, and an efficient SHA-256 hex encoding. The two HIGH findings (all-files-in-memory and sequential walker) represent meaningful scalability limits at the 50,000-file cap but are acceptable for a v1 with appropriate documentation. The mtime-based pre-screening (MEDIUM) would significantly improve incremental rebuild performance on large repos and should be considered before the feature stabilizes.
