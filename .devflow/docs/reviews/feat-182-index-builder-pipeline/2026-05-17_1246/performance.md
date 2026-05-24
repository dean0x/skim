# Performance Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**`std::env::var_os("SKIM_DEBUG")` called per-file inside parallel classify loop** - `crates/rskim/src/cmd/search/index.rs:265`
**Confidence**: 85%
- Problem: `run_classify` is invoked via `par_iter()` for every file that has a cache miss. On error, it calls `std::env::var_os("SKIM_DEBUG")` which performs a syscall (libc `getenv` with potential lock contention). While this only fires on errors (the happy path skips it), if a project has many parse failures (e.g., syntax errors in generated code), this becomes a syscall per failed file under parallel load. More importantly, this is a pattern concern: the env check should be hoisted outside the hot loop.
- Fix: Cache the debug flag once before the `par_iter()` call and pass it into `run_classify`:
```rust
let debug_enabled = std::env::var_os("SKIM_DEBUG").is_some();

// In the parallel classify step:
(run_classify(&rf.content, rf.lang, debug_enabled), false)

fn run_classify(content: &str, lang: Language, debug: bool) -> FieldMap {
    match classify_source(content, lang) {
        Ok(fields) => fields,
        Err(e) => {
            if debug {
                eprintln!("...");
            }
            Vec::new()
        }
    }
}
```

### MEDIUM

**`path_keys[idx].clone()` allocates a new String per file in the sequential manifest-insert loop** - `crates/rskim/src/cmd/search/index.rs:221`
**Confidence**: 82%
- Problem: In the sequential loop (line 214-226), `path_keys[idx].clone()` allocates a new heap `String` for every file. Since `ManifestEntry.path` takes ownership of this String, and `path_keys` is not used after the loop except at line 221, the vector could be consumed (via `into_iter()` / indexing with `std::mem::take`) instead of cloning. On a 50K-file repo this is 50K unnecessary string allocations.
- Fix: Consume `path_keys` by converting the indexed loop to drain the vector:
```rust
// Convert path_keys into an owned iterator aligned with read_files
let mut path_keys_iter = path_keys.into_iter();
for (idx, rf) in read_files.iter().enumerate() {
    let path_key = path_keys_iter.next().unwrap(); // same order, consumed
    // ...
    new_manifest.insert(ManifestEntry {
        path: path_key, // moved, not cloned
        // ...
    });
}
```
Alternatively, use `std::mem::take(&mut path_keys[idx])` if indexed access is preferred.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`walk_and_read` Vec allocations not pre-sized** - `crates/rskim/src/cmd/search/walk.rs:100-101`
**Confidence**: 80%
- Problem: `Vec::new()` for both `files` and `skipped` starts with zero capacity. For a typical project (hundreds to thousands of files), this triggers multiple reallocations as the vector grows. The `max_files` parameter is already available and provides a reasonable upper bound for pre-allocation.
- Fix:
```rust
let mut files: Vec<ReadFile> = Vec::with_capacity(max_files.min(4096));
let mut skipped: Vec<SkipReason> = Vec::with_capacity(256);
```

**SHA-256 hex encoding uses manual `write!` loop** - `crates/rskim/src/cmd/search/walk.rs:282-291`
**Confidence**: 80%
- Problem: The `sha256_hex` function formats each byte individually with `write!(hex, "{byte:02x}")` in a loop. While the `String::with_capacity(64)` pre-allocation is correct, the per-byte formatting through `std::fmt` machinery adds overhead compared to a direct lookup table approach. On 50K files this function is called 50K times (once per file in the walk phase).
- Fix: Use the `hex` crate (already common in Rust crypto ecosystems) or a lookup-table encoder:
```rust
fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    // hex::encode uses a lookup table - faster than fmt
    hex::encode(digest)
}
```
If adding a dependency is undesirable, a `const` lookup table for nibbles is also faster than `write!`.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**Sequential file walk with sorted output forces single-threaded I/O** - `crates/rskim/src/cmd/search/walk.rs:113`
**Confidence**: 80%
- Problem: `sort_by_file_path(|a, b| a.cmp(b))` forces the `ignore` walker into single-threaded mode (per `ignore` crate docs, sorting disables parallel walk). For large repos (50K files), the walk + read phase is purely sequential I/O. The classification phase uses rayon, but the walk phase does not benefit from parallelism.
- Note: This is a design tradeoff (deterministic output order) that pre-dates this PR. Changing it would require collecting unsorted results and sorting after the walk.

## Suggestions (Lower Confidence)

- **Walk reads all file content upfront before classification** - `crates/rskim/src/cmd/search/index.rs:162` (Confidence: 70%) -- The pipeline reads ALL files into memory (step 2) before classifying any of them (step 4b). For a 50K-file repo with an average 10KB file, this is ~500MB resident. A streaming approach (walk + classify in batches) would reduce peak memory. However, this is a fundamental architecture choice (two-phase pipeline) and may be intentional for simplicity.

- **`manifest.save()` sorts entry keys before writing** - `crates/rskim/src/cmd/search/manifest.rs:229-230` (Confidence: 65%) -- Sorting 50K string keys for deterministic output adds O(n log n) cost. The sort is needed for reproducible builds but could be skipped if determinism is not required for correctness.

- **`project_root_hash` uses `write!` loop similar to sha256_hex** - `crates/rskim/src/cmd/search/index.rs:296-304` (Confidence: 62%) -- Same per-byte `write!` pattern as `sha256_hex`, though this is only called once per build so impact is negligible.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Performance Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The pipeline demonstrates good performance awareness: rayon parallelism for classification, pre-computed path keys to avoid duplicate allocations, BufWriter for manifest I/O, pre-sized buffer in `open_and_read`, cached DirEntry metadata for size pre-screening, and merged loops to eliminate redundant iteration. The blocking issues are relatively minor (env var in hot path, avoidable String clones) and straightforward to fix without architectural changes.
