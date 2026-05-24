# Reliability Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**FileId cast can silently wrap on projects with >4B files** - `index.rs:217`
**Confidence**: 85%
- Problem: `FileId(idx as u32)` uses `as` truncation on the `enumerate()` index. While the default `max_files` cap is 50,000, the `--max-files` flag accepts any `usize` value, meaning a user could pass `--max-files=5000000000`. If `idx` exceeds `u32::MAX` (4,294,967,295), the cast silently wraps to 0, producing duplicate `FileId`s and corrupting the index. Even though this is impractical today, a raw `as` cast on external-controlled size is a reliability defect.
- Fix: Use `u32::try_from(idx)` to fail loudly rather than wrap:
```rust
let file_id = FileId(
    u32::try_from(idx)
        .map_err(|_| anyhow::anyhow!("file count exceeds u32::MAX"))?
);
builder.add_file_classified(file_id, &rf.content, rf.lang, field_map)?;
```

**`--max-files=0` accepted without validation, produces empty index silently** - `index.rs:97-101`
**Confidence**: 82%
- Problem: `val.parse::<usize>()` accepts `0` as a valid value. When `max_files` is 0, `walk_and_read` immediately hits the cap, produces zero files, writes an empty manifest, and reports success. The error message says "--max-files requires a positive integer" but the validation only rejects non-numeric values, not zero.
- Fix: Add a zero-check after parsing:
```rust
} else if let Some(val) = next_value(args, &mut i, "--max-files")? {
    let n = val.parse::<usize>()
        .map_err(|_| anyhow::anyhow!("--max-files requires a positive integer"))?;
    if n == 0 {
        anyhow::bail!("--max-files requires a positive integer");
    }
    max_files = Some(n);
```

### MEDIUM

**`walk_and_read` holds all file contents in memory simultaneously** - `index.rs:171`, `walk.rs:89-198`
**Confidence**: 85%
- Problem: `walk_and_read` reads every file into a `Vec<ReadFile>` with full `String` content. For a project with 50,000 files averaging 10KB each, this is ~500MB of heap. The content is then held through classification and index building. There is no back-pressure or streaming mechanism. The 5MB per-file cap helps, but the aggregate is unbounded relative to file count. With `--max-files` uncapped (user can pass very large values), OOM is possible on large codebases.
- Fix: This is acceptable for v1 given the 50,000 default cap, but consider either (a) documenting the memory profile in the help text, or (b) adding an upper bound assertion on `max_files` (e.g., cap at 200,000 regardless of user input) as a safety valve:
```rust
pub fn effective_max_files(&self) -> usize {
    self.max_files.unwrap_or(Self::DEFAULT_MAX_FILES).min(200_000)
}
```

**`discover_project_root` loop is bounded by filesystem depth but has no explicit limit** - `walk.rs:52-68`
**Confidence**: 80%
- Problem: The `loop` walks up the directory tree via `current.parent()`. On a normal filesystem this terminates at the root directory (`parent()` returns `None`). However, there is no explicit iteration counter. Per the reliability iron law, all loops should have a fixed upper bound. While the OS filesystem depth is an implicit bound, making it explicit is defensive.
- Fix: Add an explicit iteration limit as a safety net:
```rust
let mut depth = 0;
const MAX_DEPTH: usize = 256;
loop {
    if depth >= MAX_DEPTH { break; }
    if current.join(".git").exists() {
        return Ok(current.to_path_buf());
    }
    match current.parent() {
        Some(parent) => current = parent,
        None => break,
    }
    depth += 1;
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Manifest `load` reads unbounded number of JSONL lines into HashMap** - `manifest.rs:152-164`
**Confidence**: 82%
- Problem: The manifest load loop iterates over all lines in the file without any cap. A corrupted or maliciously crafted manifest could contain millions of lines, consuming excessive memory. While the manifest is written by this same code (bounded by `max_files`), a defense-in-depth approach would cap the number of entries parsed.
- Fix: Add a maximum entry count consistent with the file cap:
```rust
const MAX_MANIFEST_ENTRIES: usize = 200_000;
let mut entries = HashMap::new();
for line_result in lines {
    if entries.len() >= MAX_MANIFEST_ENTRIES { break; }
    // ... existing parse logic
}
```

## Pre-existing Issues (Not Blocking)

No pre-existing reliability issues identified in the changed files.

## Suggestions (Lower Confidence)

- **Classify errors silently swallowed** - `index.rs:255` (Confidence: 70%) -- `run_classify` returns `unwrap_or_default()` on classification failure, which silently produces an empty field map. This means corrupted or unusual files get indexed with no field data and no warning. Consider at minimum logging to stderr or incrementing a counter.

- **`NamedTempFile` on cross-device cache dirs** - `manifest.rs:216` (Confidence: 65%) -- `NamedTempFile::new_in(&self.cache_dir)` followed by `persist()` uses an atomic rename. If `cache_dir` and the system temp dir are on different filesystems, `new_in` handles this correctly (it creates the temp file in `cache_dir`). However, if `cache_dir` does not exist at the time of the call, the error message from `NamedTempFile` will be opaque. The `create_dir_all` in `build_index` guards this, but if `save()` is called from other paths in the future, it could fail confusingly.

- **No `BufWriter` wrapping on manifest temp file writes** - `manifest.rs:219-228` (Confidence: 62%) -- Each `writeln!` to the `NamedTempFile` issues a separate write syscall. For manifests with thousands of entries, wrapping in `BufWriter` would reduce syscall overhead.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | - | 0 | 1 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The pipeline demonstrates good reliability fundamentals: atomic manifest writes, SHA-256 integrity checks, graceful handling of corrupted manifests, explicit file-size caps, and proper error propagation. The two HIGH issues (silent `u32` wrapping on FileId and `--max-files=0` acceptance) are straightforward to fix. The MEDIUM issues around memory bounds and the explicit loop limit are worth addressing for defense-in-depth but are not blocking for practical usage given the 50,000 default cap.
