# Security Review Report

**Branch**: refactor-230-232-233-tech-debt-pipeline -> main
**Date**: 2026-05-17T23:15

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **TOCTOU window between walk_metadata and open_and_read** - `walk.rs` (walk_metadata) / `index.rs:336` (Confidence: 65%) -- The streaming design introduces a wider time gap between the metadata-only walk (classify_entry_metadata) and the deferred open_and_read in the producer thread. A file could be replaced with a symlink or grow past MAX_FILE_BYTES during this window. The existing open_and_read already re-checks size on the open file handle, and follow_links is false, which mitigates both vectors. The pre-existing TOCTOU is documented and handled correctly, so this is informational only.

- **Atomic counter ordering on producer_skips** - `index.rs:240` (Confidence: 62%) -- `Ordering::Relaxed` is used for `producer_skips.fetch_add(1, ...)` on the producer side and `producer_skips.load(Ordering::Relaxed)` on the consumer side after `join()`. The `join()` call establishes a happens-before relationship, so the final load will see all increments. This is technically correct, but `Ordering::Acquire` on the load would make the synchronization intent explicit and self-documenting for future maintainers.

- **Panic payload extraction in producer_handle.join** - `index.rs:295-301` (Confidence: 60%) -- The panic-to-error conversion only extracts `String` payloads via `downcast_ref::<String>()`. Panics with `&str` payloads (the most common kind from `panic!("literal")`) are not extracted and fall through to `<non-string panic>`. This does not lose data or create a vulnerability, but the `&str` case could be added for better diagnostics: `.or_else(|| e.downcast_ref::<&str>().map(|s| *s))`.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

## Rationale

This PR is a well-structured internal refactoring of the index build pipeline from a batch model to a bounded-channel streaming model. The security posture is strong:

1. **No new trust boundaries introduced.** All data remains local filesystem I/O within the skim search cache. No network, no user-facing input parsing, no authentication surfaces.

2. **Input validation preserved.** File size checks remain in both the metadata pre-screen (`classify_entry_metadata`) and the `open_and_read` handler. The MAX_FILE_BYTES guard is applied twice (TOCTOU-aware), and the compile-time assertion guarantees the cast to `usize` is sound.

3. **Symlink traversal blocked.** `follow_links(false)` in `configure_builder` prevents symlink-based path traversal. Extracting the builder configuration into a shared function ensures this is applied consistently to both the test-only `walk_and_read` and the production `walk_metadata` path.

4. **Bounded resource usage.** The channel capacity is fixed at 64 (CHANNEL_CAPACITY), max files at 50,000, max skip reasons at 10,000, and max manifest entries at 60,000. All loops have explicit upper bounds. The manifest file size is capped at 256 MiB before parsing begins.

5. **Atomic manifest writes.** The existing `NamedTempFile` + `persist` pattern for manifest writes prevents readers from observing partial state. This was not changed and remains correct.

6. **SHA-256 remains the correctness guarantee.** The new mtime pre-screening is explicitly documented as a performance hint only. SHA-256 is always computed and compared for cache hit decisions (index.rs:359, 366). An attacker who can manipulate file mtimes cannot bypass the content hash check.

7. **No hardcoded secrets, no credential handling, no deserialization of untrusted external data.** The JSONL manifest is written and read by the same process; malformed entries are silently skipped with hard caps on entry count.

8. **Thread safety.** The producer thread moves ownership of `walk_entries`, `manifest`, `tx`, and the skip counter clone into the closure. The consumer receives via the channel. No shared mutable state exists between threads except the `AtomicU32` skip counter, which uses appropriate atomic operations. The `join()` call properly propagates panics.

9. **Overflow protection.** `next_file_id` uses `checked_add` (line 276) to detect u32 overflow rather than wrapping silently. `cache_hits` uses `saturating_add` (line 280) which is safe for a display-only counter. `to_u32_capped` returns `u32::MAX` on overflow.

10. **Backward compatibility.** The `#[serde(default)]` on `ManifestEntry::mtime` ensures old manifests (without the field) deserialize cleanly with `mtime: None`, tested explicitly in `test_mtime_backward_compat_none`.
