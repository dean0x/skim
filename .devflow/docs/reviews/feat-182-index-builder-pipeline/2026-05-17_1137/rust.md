# Rust Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**Unsafe `as u32` cast on enumeration index may truncate silently** - `index.rs:217`
**Confidence**: 90%
- Problem: `FileId(idx as u32)` uses `as` cast which silently truncates if `idx >= u32::MAX`. While the default `max_files` of 50,000 is safe, the `--max-files` flag accepts arbitrary `usize` values, meaning a user could pass `--max-files=5000000000` on a 64-bit system. The cast would silently wrap, producing duplicate `FileId` values and corrupting the index.
- Fix: Use `u32::try_from(idx)` with a proper error, consistent with how `file_count` and `skipped_count` are already handled on lines 172, 211, 236:
```rust
let file_id = FileId(
    u32::try_from(idx)
        .map_err(|_| anyhow::anyhow!("file index {idx} exceeds u32::MAX"))?
);
builder.add_file_classified(file_id, &rf.content, rf.lang, field_map)?;
```

**Fragile language serialization via `Debug` format** - `index.rs:230`
**Confidence**: 85%
- Problem: `format!("{:?}", rf.lang).to_lowercase()` relies on the `Debug` derive output of `Language` enum variants for the manifest `lang` field. The `Debug` trait output is not considered a stable API surface -- if a variant is renamed (e.g., `TypeScript` to `Typescript`) or a custom `Debug` impl is added, the manifest will silently produce different strings, breaking incremental cache hits for all existing manifests. The `lang` field is stored in the manifest and compared across sessions.
- Fix: Implement a `Display` or `as_str()` method on `Language` that returns a stable, documented string, or use the file extension directly since it's already deterministic:
```rust
// Option A: add a stable method to Language (preferred)
lang: rf.lang.as_str().to_owned(),

// Option B: use extension from the path (works but less clean)
lang: rf.rel_path.extension()
    .and_then(|e| e.to_str())
    .unwrap_or("unknown")
    .to_lowercase(),
```

### MEDIUM

**`read_to_string` errors all classified as NonUtf8** - `walk.rs:166-171`
**Confidence**: 85%
- Problem: Any error from `fs::read_to_string()` (including permission denied, device errors, broken pipes on special files) is classified as `SkipReason::NonUtf8`. This misclassifies I/O errors and makes diagnostic output misleading when debugging indexing issues.
- Fix: Distinguish encoding errors from I/O errors:
```rust
let content = match fs::read_to_string(abs_path) {
    Ok(c) => c,
    Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
        skipped.push(SkipReason::NonUtf8(abs_path.to_path_buf()));
        continue;
    }
    Err(e) => {
        skipped.push(SkipReason::ReadError {
            path: abs_path.to_path_buf(),
            error: e.to_string(),
        });
        continue;
    }
};
```

**`classify_source` errors silently swallowed** - `index.rs:250-256`
**Confidence**: 82%
- Problem: `run_classify` calls `classify_source(content, lang).unwrap_or_default()`, silently discarding any error. A file that fails classification is indexed with zero field annotations, meaning BM25F scoring will treat all its content as unclassified. There is no count of classification failures in `IndexResult`, so the user has no way to know this happened.
- Fix: At minimum, increment a counter for classification failures and include it in the summary output. Consider logging to stderr under the `SKIM_DEBUG` flag:
```rust
fn run_classify(
    content: &str,
    lang: rskim_core::Language,
) -> (FieldMap, bool) {
    match classify_source(content, lang) {
        Ok(fm) => (fm, true),
        Err(_e) => {
            // Could log under SKIM_DEBUG
            (Vec::new(), false) // false = classification failed
        }
    }
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`walk_and_read` error path creates misleading `ReadError` from walker error string** - `walk.rs:117-123`
**Confidence**: 82%
- Problem: When the walker yields an error entry, the code constructs a `PathBuf` from the error's `to_string()` representation: `PathBuf::from(err.to_string())`. This produces nonsensical paths like `/path/to/file: permission denied` that cannot be used for any path operations. The `ignore::Error` type has methods to extract the actual path.
- Fix: Extract the path from the error if available:
```rust
Err(err) => {
    let path = err.path()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("<unknown>"));
    skipped.push(SkipReason::ReadError {
        path,
        error: err.to_string(),
    });
    continue;
}
```

## Pre-existing Issues (Not Blocking)

No critical pre-existing issues found in the reviewed files.

## Suggestions (Lower Confidence)

- **TOCTOU between metadata check and file read** - `walk.rs:147-172` (Confidence: 65%) -- There is a time-of-check/time-of-use gap between `fs::metadata()` (line 147) and `fs::read_to_string()` (line 166). A file could grow beyond 5 MB between the check and the read. In practice this is unlikely for source files, and the content is bounded by available memory anyway.

- **Pre-size `files` Vec when `max_files` is known** - `walk.rs:93` (Confidence: 65%) -- `Vec::new()` starts with zero capacity. When indexing large projects, this causes repeated reallocations. Consider `Vec::with_capacity(max_files.min(1024))` as a reasonable pre-allocation.

- **Manifest `save()` does not `flush()` before `persist()`** - `manifest.rs:216-232` (Confidence: 70%) -- `NamedTempFile` uses buffered I/O internally. While `persist()` closes the file descriptor (which flushes), an explicit `tmp.flush()?` before `persist()` would make the intent clearer and guard against future refactors.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The overall architecture is clean: well-separated modules (types, walk, manifest, index), good use of `anyhow` for application-level error handling, atomic manifest writes via `NamedTempFile`, and correct use of rayon for parallel classification. The type design is solid with `SkipReason` enum encoding all skip states. The `as u32` truncation and `Debug`-based serialization are the two issues that should be addressed before merge -- the former is a correctness risk for edge cases, the latter is a maintenance fragility that will cause silent cache invalidation when `Language` variants change.
