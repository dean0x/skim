# Reliability Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**Manifest parsing has no upper bound on entry count** - `manifest.rs:158-169`
**Confidence**: 85%
- Problem: `FileManifest::load` reads every line from `index.skfiles` into a `HashMap` without any upper bound. While the file is locally generated (not untrusted external input), a corrupted or hand-edited manifest with millions of lines would cause unbounded memory growth during loading. The `HashMap::with_capacity(1024)` is a pre-allocation hint, not a cap. Per the Iron Law: "every loop must have a fixed upper bound."
- Fix: Add a manifest entry cap consistent with the index file cap (e.g., `DEFAULT_MAX_FILES + margin`). Once the cap is reached, stop parsing and use what was loaded:
```rust
const MAX_MANIFEST_ENTRIES: usize = 60_000; // slightly above DEFAULT_MAX_FILES
let mut entries = HashMap::with_capacity(1024);
for line_result in lines {
    if entries.len() >= MAX_MANIFEST_ENTRIES {
        break;
    }
    // ... existing parsing logic ...
}
```

**`BufReader::lines()` has no per-line length limit** - `manifest.rs:126-127`
**Confidence**: 82%
- Problem: `BufReader::lines()` will read a single line of arbitrary length into memory. A corrupted or adversarially crafted manifest file with a single multi-gigabyte line (no newlines) would cause an out-of-memory condition. This is a defense-in-depth concern -- the file is self-generated, but the Iron Law requires explicit bounds on all I/O operations. The `read_line` underlying `lines()` allocates without bound.
- Fix: Use `BufReader::with_capacity()` with a bounded read loop, or validate file size before parsing:
```rust
// Option A: Size-gate the manifest file
let meta = file.metadata()?;
if meta.len() > 256 * 1024 * 1024 {
    // 256 MiB sanity cap for a manifest (50K entries at ~5KB each = ~250MB max)
    return Ok(Self::new(project_root, cache_dir));
}
```

### MEDIUM

**No fsync before atomic rename on manifest save** - `manifest.rs:237-242`
**Confidence**: 80%
- Problem: The manifest `save()` method calls `flush()` on the `BufWriter` and then `persist()` (rename) but does not call `fsync`/`sync_data` on the file before the rename. On power loss between the rename and the OS flushing dirty pages, the manifest could contain zeros or partial data. The PR description highlights atomic write ordering as a reviewer focus, and the manifest is the coherence marker -- if it is corrupt after a crash, the next build may misinterpret the index state.
- Fix: Call `sync_data()` on the inner file before persisting. The `tempfile` crate supports this via `as_file().sync_data()`:
```rust
buf.flush()?;
let tmp = buf.into_inner().context("failed to flush manifest buffer")?;
tmp.as_file().sync_data()?;  // ensure bytes hit disk before rename
let manifest_path = self.cache_dir.join(Self::MANIFEST_FILENAME);
tmp.persist(&manifest_path)
    .map_err(|e| anyhow::anyhow!("failed to persist manifest: {}", e.error))?;
```
Note: The same pattern should be applied to `NgramIndexBuilder::atomic_write` in the search crate, but that file is pre-existing and not modified in this PR.

**`walk_and_read` skipped vec can grow without bound** - `walk.rs:131`
**Confidence**: 80%
- Problem: The `skipped` vector is pre-allocated at 256 but has no upper bound. In a repository with millions of non-source files (e.g., a large monorepo with many `.png`, `.bin`, `.dat` files), the `UnsupportedLanguage` entries would allocate unbounded path allocations. Each `SkipReason` variant carries a `PathBuf` heap allocation. The accepted `files` vector is bounded by `max_files`, but `skipped` is not.
- Fix: Cap the `skipped` collection. Since skip reasons are purely diagnostic (only the count is reported to stderr), stop collecting individual reasons after a threshold:
```rust
const MAX_SKIP_REASONS: usize = 10_000;
// In the loop:
if skipped.len() < MAX_SKIP_REASONS {
    skipped.push(SkipReason::UnsupportedLanguage(abs_path.to_path_buf()));
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`unsafe` block in `sha256_hex` relies on implicit invariant** - `walk.rs:332`
**Confidence**: 85%
- Problem: `String::from_utf8_unchecked` is used with a comment "SAFETY: NIBBLES contains only ASCII hex characters." While the invariant is correct today, the `unsafe` block's safety depends on `NIBBLES` never being modified to contain non-ASCII bytes. This is a single-function distance (the const is 7 lines above), but reliability best practice is to avoid `unsafe` when a safe alternative exists with negligible cost.
- Fix: Use the safe `String::from_utf8(hex).expect("hex nibbles are ASCII")` or even `String::from_utf8(hex).unwrap()` (since the invariant is compile-time verifiable). For a hot-path function called once per file, the single UTF-8 validation check is negligible:
```rust
// Safe alternative -- the unwrap is infallible since NIBBLES is all-ASCII
String::from_utf8(hex).expect("hex digest contains only ASCII nibbles")
```

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`NgramIndexBuilder::build()` lacks fsync on atomic writes** - `builder.rs:86-93` (pre-existing, not modified in this PR)
**Confidence**: 82%
- Problem: The `atomic_write` function in the search index builder performs `write_all` followed by `persist` (rename) without an intermediate `sync_data()` call. Same crash-consistency concern as the manifest write. Since `.skidx` is the commit point and the manifest is written after, a crash during the window between the `.skidx` rename and the manifest write could leave the system in an inconsistent state (index files exist but no manifest).
- This is pre-existing code not modified in this PR -- informational only.

## Suggestions (Lower Confidence)

- **Manifest line deserialization does not validate field_map ranges** - `manifest.rs:165` (Confidence: 70%) -- When deserializing `ManifestEntry`, the `field_map` triples `(start, end, discriminant)` are accepted without checking that `start < end` or that ranges don't overlap. A corrupted manifest could inject invalid ranges into the index builder, though `decode_field_map` already filters unknown discriminants.

- **`project_root_hash` truncates SHA-256 to 8 bytes (64 bits)** - `index.rs:300-312` (Confidence: 65%) -- Using only the first 8 bytes of a SHA-256 for the cache directory name gives 64 bits of collision resistance. While sufficient for typical workloads (birthday bound at ~4 billion project roots), this is worth documenting as an explicit design choice. A collision would cause two projects to share a cache directory, leading to incorrect incremental builds.

- **`find_file_with_ext` in tests is recursively unbounded** - `index_tests.rs:383-398` (Confidence: 62%) -- The test helper `find_file_with_ext` recurses into directories without a depth limit. On symlink loops (not expected in tempdir, but worth noting) this would stack overflow. Low risk since it's test-only code on controlled temp directories.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The pipeline demonstrates strong reliability fundamentals: bounded ancestor traversal (`MAX_ANCESTORS`), file size caps with TOCTOU protection, compile-time assertions on constant validity, saturating u32 casts, fail-soft error handling for classification failures, and atomic write patterns via temp file + rename. The main gaps are around unbounded manifest parsing (entry count and line length) and missing fsync for crash consistency on the coherence-critical manifest file. These are addressable without architectural changes.
