# Security Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Symlink-based file content substitution in TOCTOU window** - `walk.rs:162-171`
**Confidence**: 68%

Moved to Suggestions (below threshold).

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Residual TOCTOU window between pre-screen and open_and_read** - `walk.rs:162-177` (Confidence: 68%) — The `entry.metadata()` pre-screen at line 162 uses the DirEntry cached metadata (from readdir), then `open_and_read` opens the file. Between these two operations, the file could be replaced by a larger file (or a symlink to a large file). The `open_and_read` function re-checks size on the open handle (line 247), so actual exploitation is mitigated. The residual window only produces a redundant `TooLarge` skip rather than a memory exhaustion. This is defense-in-depth working as designed. No action required.

- **Error kind matching via string comparison** - `walk.rs:183-184` (Confidence: 72%) — The check `e.kind() == io::ErrorKind::Other && e.to_string().contains("too large")` relies on the literal error message produced by `open_and_read`. If the message string changes in a future refactor, the error would be misclassified as a generic `ReadError` rather than `TooLarge`. This is not a security vulnerability (the file is still skipped), but coupling to a string literal is fragile. Consider using a custom error type or a dedicated `ErrorKind` to avoid string-based dispatch.

- **Cache directory hash uses truncated SHA-256 (8 bytes / 16 hex chars)** - `index.rs:295-305` (Confidence: 62%) — `project_root_hash` takes only the first 8 bytes of SHA-256 for the directory name. With ~65,000 indexed projects, birthday collision probability approaches 50%. A collision would cause two different projects to share an index directory, leading to incorrect search results but not a security vulnerability (both projects must be owned by the same user). Acceptable for the current use case.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

## Analysis Notes

This PR demonstrates strong security practices:

1. **TOCTOU mitigation** (`open_and_read`): The new `open_and_read` function at `walk.rs:243-255` correctly uses the file handle's metadata (not a separate stat call) for the authoritative size check, preventing the classic TOCTOU race where a small file is swapped for a large one between stat and read.

2. **Symlink protection**: `WalkBuilder` is configured with `.follow_links(false)` at `walk.rs:112`, preventing symlink traversal attacks that could escape the project root.

3. **Path traversal defense**: `Language::from_path` (pre-existing, unchanged) rejects paths containing `..` components, preventing path traversal in the classification layer.

4. **Bounded iteration**: `discover_project_root` now has a `MAX_ANCESTORS = 256` upper bound (`walk.rs:44,64`), preventing unbounded filesystem traversal.

5. **Atomic writes**: Manifest persistence uses `NamedTempFile` + rename (`manifest.rs:221-242`), ensuring readers never see partial writes.

6. **Input validation**: `--max-files` rejects zero via `parse_positive_usize` (`index.rs:134-143`), preventing a silently empty index. Unknown flags are rejected by clap.

7. **Fail-soft classification**: `run_classify` logs errors only when `SKIM_DEBUG` is set (`index.rs:265`), not leaking internal details to stdout in production.

8. **Size-limited reads**: 5 MiB cap prevents memory exhaustion from large files.

No blocking security issues were found. The changes actively improve the security posture (TOCTOU fix, bounded loops, better error handling).
