# Performance Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**All file contents held in memory simultaneously** - `index.rs:171`, `walk.rs:89-198`
**Confidence**: 90%
- Problem: `walk_and_read` reads every file into memory (as `ReadFile.content: String`) and returns them all in a `Vec`. With 50,000 files (the default cap) and an average file size of even 10 KB, this is ~500 MB of heap-resident strings. The entire `read_files` vector then lives alongside the `classified` vector until the end of `build_index`. At scale this causes significant memory pressure and poor cache locality.
- Fix: Consider a streaming/batched approach where files are processed in chunks (e.g., 1,000 at a time), or separate the walk phase (collect paths + SHA-256 hashes) from the read phase (read content just-in-time during classification). As a minimal fix, drop `content` from `ReadFile` after SHA-256 computation and re-read during classification, or process files in batches:

```rust
// Minimal mitigation: pre-allocate the Vec
let mut files: Vec<ReadFile> = Vec::with_capacity(max_files.min(4096));
```

**Redundant `fs::metadata` syscall per file** - `walk.rs:147`
**Confidence**: 92%
- Problem: `walk_and_read` calls `fs::metadata(abs_path)` for every file to check its size. However, the `ignore::DirEntry` returned by the walker already provides a `.metadata()` method that may reuse cached metadata from the directory traversal (avoiding a redundant `stat(2)` syscall per file). On a 50,000-file project, this is 50,000 unnecessary syscalls.
- Fix: Use the metadata from the `DirEntry` directly:

```rust
// Before (redundant syscall):
let metadata = match fs::metadata(abs_path) { ... };

// After (reuse walker metadata):
let metadata = match entry.metadata() {
    Ok(Ok(m)) => m,
    _ => {
        skipped.push(SkipReason::ReadError {
            path: abs_path.to_path_buf(),
            error: "metadata unavailable".to_string(),
        });
        continue;
    }
};
```

Note: `ignore::DirEntry::metadata()` returns `Result<Metadata, Error>` so the error type differs slightly from `std::fs::metadata`.

### MEDIUM

**Duplicate path-key string allocation in rayon hot path** - `index.rs:198`, `index.rs:226`
**Confidence**: 85%
- Problem: `rf.rel_path.to_string_lossy().replace('\\', "/")` is computed twice for every file: once during parallel classification (line 198) and once during manifest construction (line 226). Each call allocates a new `String`. On Unix, the `replace('\\', "/")` call always allocates a copy even when there are no backslashes to replace (since `to_string_lossy()` returns a `Cow` but `replace` always returns an owned `String`).
- Fix: Pre-compute path keys once before the parallel classify step, or store the path key in `ReadFile`. On Unix, skip the replace entirely:

```rust
// Compute once before par_iter:
let path_keys: Vec<String> = read_files
    .iter()
    .map(|rf| rf.rel_path.to_string_lossy().replace('\\', "/"))
    .collect();

// Then use path_keys[idx] in both the classify and manifest loops.
```

**Debug-format-based language serialization** - `index.rs:230`
**Confidence**: 82%
- Problem: `format!("{:?}", rf.lang).to_lowercase()` uses the `Debug` trait to produce a language name string. This performs two allocations (one for `format!`, one for `to_lowercase()`) per file. More importantly, relying on `Debug` output for serialization is fragile -- if the Debug representation changes, stored manifests become incompatible.
- Fix: Add a `fn name(&self) -> &'static str` method to `Language` (or use `Display`), and use it directly. This eliminates both allocations:

```rust
// Instead of: format!("{:?}", rf.lang).to_lowercase()
// Use: rf.lang.name()  // returns &'static str like "rust", "typescript"
```

**No pre-allocation for HashMap in manifest load** - `manifest.rs:153`
**Confidence**: 80%
- Problem: `HashMap::new()` starts with zero capacity. When loading a manifest with thousands of entries, the HashMap must rehash and reallocate multiple times (approximately log2(n) resizes). For a 50,000-entry manifest this means ~16 reallocations during load.
- Fix: Count lines in the file first (cheap) or use a reasonable default capacity:

```rust
// Reasonable default since we know the file count cap:
let mut entries = HashMap::with_capacity(1024);
```

**Manifest save writes unbuffered** - `manifest.rs:216-228`
**Confidence**: 80%
- Problem: `save()` writes directly to a `NamedTempFile` without wrapping it in a `BufWriter`. Each `writeln!` call is a separate `write(2)` syscall. For 50,000 entries, that's 50,001 syscalls (1 header + 50,000 entries). The load path correctly uses `BufReader`, but the save path does not use `BufWriter`.
- Fix: Wrap the temp file in a `BufWriter`:

```rust
let tmp = NamedTempFile::new_in(&self.cache_dir)?;
let mut writer = std::io::BufWriter::new(tmp);
// ... write header and entries to `writer` ...
writer.flush()?;
let tmp = writer.into_inner().map_err(|e| anyhow::anyhow!("flush error: {}", e))?;
tmp.persist(&manifest_path)...
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Sequential single-threaded file walk and read** - `walk.rs:108`
**Confidence**: 82%
- Problem: The `ignore` crate supports parallel walking via `WalkBuilder::build_parallel()`, but the current implementation uses the sequential `build()` iterator. For large projects, the walk + read phase is I/O-bound and could benefit from parallel directory traversal and file reading, especially on SSDs where concurrent reads are efficient.
- Fix: This is a future optimization opportunity. The current sequential approach is correct and simpler. Consider switching to `build_parallel()` if profiling shows the walk phase is a bottleneck on large repositories. The sorted ordering requirement would need to be handled post-collection.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`sort_by_file_path` forces sequential walk** - `walk.rs:106` (Confidence: 65%) -- The `sort_by_file_path` callback enforces lexicographic ordering during traversal, which prevents the walker from using its parallel backend. If deterministic ordering is only needed for the output, consider sorting the `files` vector after collection instead.

- **`to_string_lossy()` repeated in `path_key` computation on rayon threads** - `index.rs:198` (Confidence: 70%) -- Inside `par_iter().map()`, `to_string_lossy()` may allocate on non-UTF-8 paths. Since the vast majority of paths are valid UTF-8, consider pre-computing path keys before entering the parallel section to avoid per-thread allocation overhead.

- **All SkipReasons accumulated but never used** - `walk.rs:94`, `index.rs:172` (Confidence: 75%) -- The `skipped_reasons` vector accumulates `SkipReason` values (each containing a `PathBuf`) but only its `.len()` is used. The actual reasons are discarded. Consider counting skip categories with a simple counter struct instead of allocating `PathBuf` values that are immediately dropped.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 4 | 0 |
| Should Fix | - | 0 | 1 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Performance Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The pipeline's overall architecture is sound -- rayon for parallel classification and sequential index building is the right approach given `NgramIndexBuilder` is not Sync. The two HIGH findings (full-content memory residency and redundant metadata syscalls) represent measurable overhead at the 50,000-file scale. The MEDIUM findings around allocation patterns and unbuffered writes are standard Rust performance hygiene items that compound at scale. None of these are correctness issues, but they will noticeably impact wall-clock time and memory consumption on large codebases.
