# Security Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Manifest cache poisoning via crafted `path` field in JSONL sidecar** - `crates/rskim/src/cmd/search/manifest.rs:165`
**Confidence**: 82%
- Problem: `FileManifest::load` deserializes `ManifestEntry.path` from the JSONL file and inserts it directly into the `entries` HashMap without validating the path string. A crafted `index.skfiles` on disk could contain entries with `../` path segments or absolute paths. When the pipeline later looks up entries by path key (computed from `rel_path.to_string_lossy().replace('\\', "/")` in `index.rs:188`), a poisoned manifest entry would not match and would be harmlessly ignored. However, the poisoned entry is re-serialized into the new manifest via `manifest.save()` without sanitization, persisting the invalid data indefinitely. While not directly exploitable in the current code (lookup is by key computed from walked files, and the cache dir is user-owned `~/.cache`), this violates defense-in-depth: if future code iterates `entries` values or uses the `path` field to construct file paths, it becomes a path traversal vector.
- Fix: Add a validation check in `FileManifest::load` when deserializing entries. Skip entries whose `path` contains `..` components or starts with `/`:
  ```rust
  if let Ok(entry) = serde_json::from_str::<ManifestEntry>(&line) {
      // Defense-in-depth: reject paths that could escape the project root
      if entry.path.contains("../") || entry.path.starts_with('/') {
          continue;
      }
      entries.insert(entry.path.clone(), entry);
  }
  ```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`unsafe` block in `sha256_hex` could use safe alternative** - `crates/rskim/src/cmd/search/walk.rs:332` (Confidence: 65%) -- The `unsafe { String::from_utf8_unchecked(hex) }` is sound because the nibble table contains only ASCII hex chars. However, the performance gain over `String::from_utf8(hex).unwrap()` is negligible for a 64-byte string, and eliminating the `unsafe` block reduces audit burden. The SAFETY comment is present and correct, so this is not a defect -- just a preference for safe code in non-hot-path contexts.

- **Hidden `--index-dir` flag accepts arbitrary write path** - `crates/rskim/src/cmd/search/index.rs:112` (Confidence: 62%) -- The hidden `--index-dir` flag allows writing index files and the manifest to any directory the user has write access to. This is intentionally hidden and used for tests, so it is not user-facing. But if this CLI is ever invoked by other tools or agents passing untrusted arguments, it could write to unexpected locations. The flag is marked `hide = true`, which is the right mitigation for now.

- **8-byte (64-bit) SHA-256 truncation for cache directory names** - `crates/rskim/src/cmd/search/index.rs:307-311` (Confidence: 60%) -- The `project_root_hash` function truncates SHA-256 to 8 bytes (16 hex chars). With 64 bits of entropy, birthday collision probability is ~1 in 2^32 for a given pair of paths, which is acceptable for a local cache directory name. Not a security issue at current scale, but worth noting if the cache is ever shared across untrusted users.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Positive Security Observations

1. **TOCTOU race handled correctly** (`walk.rs:203-228`): The `open_and_read` function opens the file first, then checks metadata on the open handle, preventing the classic stat-then-read TOCTOU race. Explicitly documented in comments.

2. **Symlinks not followed** (`walk.rs:142`): `.follow_links(false)` prevents symlink-based directory escape during recursive walk.

3. **Path traversal protection in `Language::from_path`** (`types.rs:85-94`): Rejects `..` components, preventing path traversal through language detection.

4. **Atomic manifest writes** (`manifest.rs:221-242`): Uses `NamedTempFile` + `persist()` for atomic rename, so readers never see partial writes.

5. **Bounded file size** (`walk.rs:34`): 5 MiB cap with compile-time assertion that the constant fits in `usize`.

6. **Bounded ancestor traversal** (`walk.rs:51`): `MAX_ANCESTORS = 256` prevents unbounded upward directory walk.

7. **Bounded file count** (`types.rs:29`): Default cap of 50,000 files prevents resource exhaustion.

8. **No command injection**: The search pipeline does not shell out or construct commands from user input. DNS parsers only parse output, not construct commands.

9. **Root mismatch detection** (`manifest.rs:149-155`): Manifest header validates project root, preventing cross-project cache confusion.

10. **Debug output to stderr only**: Classify errors go to stderr behind `SKIM_DEBUG` flag, no information leakage through stdout.
