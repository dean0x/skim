# Security Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Mutex poisoning leads to panic in parallel walker** - `walk.rs:261,272,275,287`
**Confidence**: 82%
- Problem: The parallel walk uses `.lock().unwrap()` on shared `Mutex<Vec<...>>` in four locations. If any thread panics (e.g., due to a stack overflow in `classify_entry` on a pathological file name or deeply nested symlink resolution), the mutex becomes poisoned and all subsequent threads will panic on `.unwrap()`. In a hostile environment where an attacker controls file contents or names in the walked directory, this could be used to force a denial-of-service (process abort).
- Fix: Replace `.lock().unwrap()` with `.lock().unwrap_or_else(|e| e.into_inner())` to recover from poisoned mutexes gracefully. The data in the vec is still valid after a panic — only the invariant that the lock was released cleanly is violated.

```rust
// Instead of:
files.lock().unwrap().push(file);
// Use:
files.lock().unwrap_or_else(|e| e.into_inner()).push(file);
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Corrupted manifest field_map ranges not validated** - `manifest.rs:304-312` (Confidence: 65%) — The `decode_field_map` function constructs `Range<usize>` from deserialized `(start, end)` pairs without verifying `start <= end`. While downstream code (`add_file_classified`) handles this safely (out-of-bounds ranges never match), accepting `start > end` ranges could produce subtly incorrect index results on cache-hit paths if the manifest is corrupted or tampered.

- **Arc::try_unwrap panics on thread leak** - `walk.rs:300-307` (Confidence: 62%) — If `build_parallel().run()` leaks a thread (e.g., due to a rare bug in the `ignore` crate), `Arc::try_unwrap(...).expect(...)` will panic. This is extremely unlikely in practice but violates the "no unwinding from untrusted data" principle.

- **No per-line length cap during manifest parsing** - `manifest.rs:185-201` (Confidence: 60%) — While the overall file-size check (256 MiB) limits total memory, individual `BufReader::lines()` calls could still allocate up to 256 MiB for a single line in a specially crafted manifest. A per-line cap (e.g., 1 MiB) would further harden against memory exhaustion.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Notes

The implementation demonstrates strong security awareness:

1. **Symlink safety**: `follow_links(false)` prevents symlink-based path traversal attacks.
2. **File size limits**: Two-phase check (DirEntry metadata pre-screen + open file handle check) with TOCTOU mitigation.
3. **Manifest size guards**: Both entry-count cap (`MAX_MANIFEST_ENTRIES = 60_000`) and file-size cap (`MAX_MANIFEST_FILE_BYTES = 256 MiB`) prevent unbounded memory allocation from corrupted manifests.
4. **No unsafe code**: The previous `String::from_utf8_unchecked` was replaced with the safe `String::from_utf8(...).expect(...)`.
5. **Atomic writes**: Temp file + rename pattern prevents readers from observing partial writes; `sync_data()` ensures durability across power loss.
6. **No path traversal from deserialized data**: Manifest `path` fields are only used as HashMap keys for lookup — never used to open files.
7. **Bounded recursion**: `find_file_with_ext_depth` uses explicit `max_depth` parameter to prevent infinite recursion on symlink loops.
8. **SHA-256 integrity**: Content hashes prevent cache poisoning — a corrupted manifest with wrong SHA will trigger re-classification.
9. **Skip reasons capped**: `MAX_SKIP_REASONS = 10_000` prevents memory exhaustion from large repos with millions of unsupported files.
10. **Debug flag hoisted**: `is_debug_enabled()` called once before parallel work instead of per-file, eliminating side-channel timing from env var access.

The single should-fix item (mutex poisoning) is a defense-in-depth improvement rather than an exploitable vulnerability — it requires a thread to panic first, which would already indicate an exceptional condition.
