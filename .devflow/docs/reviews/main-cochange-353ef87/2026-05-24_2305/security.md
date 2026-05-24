# Security Review Report

**Branch**: main (commit 353ef87)
**Date**: 2026-05-24
**Scope**: `crates/rskim-search/src/cochange/` -- co-change matrix builder with Jaccard similarity and binary persistence (.skcc format)

## Issues in Your Changes (BLOCKING)

### MEDIUM

**CRC32 is not a cryptographic integrity check -- insufficient for tamper detection** - `format.rs:287-288`, `reader.rs:105-113`
**Confidence**: 85%
- Problem: CRC32 (`crc32fast::hash`) is used as the sole integrity check for `.skcc` files. CRC32 is designed for accidental corruption detection, not tamper resistance. It is trivial to craft a file with a specific CRC32 value -- a determined attacker who can write to the index directory could modify pair counts or coupling data while preserving a valid checksum. The PR description calls this "CRC32 validation" which may give a false sense of security.
- Impact: If an attacker gains write access to the `.skcc` index directory, they could forge coupling data to mislead downstream analysis (e.g., hiding co-change relationships or inflating Jaccard scores) without triggering a checksum error. Severity is MEDIUM because: (1) the threat model here is local file integrity, not remote exploitation; (2) if an attacker has write access to the index dir, they likely have broader access; (3) CRC32 is standard practice for detecting accidental corruption in binary index formats (e.g., `.git/index` uses CRC32 for pack files).
- Fix: Document explicitly that CRC32 provides accidental-corruption detection only, not tamper resistance. If tamper resistance is needed in the future, add HMAC-SHA256 or BLAKE3 keyed hashing. For now, adding a doc comment is sufficient:

```rust
/// Compute the CRC32 checksum of `data`.
///
/// NOTE: CRC32 detects accidental corruption (bit flips, truncation) but is
/// NOT a cryptographic integrity check. It does not protect against intentional
/// tampering. This is acceptable because the `.skcc` file is a derived cache
/// that can be rebuilt from git history.
pub(crate) fn compute_checksum(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}
```

---

**Mmap SAFETY comment understates the risk -- concurrent modification is UB** - `reader.rs:22-25`, `reader.rs:74-75`
**Confidence**: 82%
- Problem: The module-level SAFETY comment acknowledges that concurrent file modification while the mmap is live results in undefined behaviour. However, the `unsafe` block at line 75 only says "The file is not modified after mapping." This is an assertion about the external environment that the code cannot enforce. If another process (or another thread calling `CochangeMatrixBuilder::build`) truncates or overwrites `cochange.skcc` while a reader holds an mmap, Rust's memory safety guarantees are violated -- the process may read garbage, segfault, or exhibit arbitrary behaviour.
- Impact: In practice, the `atomic_write` in `builder.rs` uses rename-based persistence, which means the old inode remains valid for existing mmaps on Unix. This mitigates the risk significantly on POSIX systems. However, on Windows, `NamedTempFile::persist` may fail or behave differently, and the old file could be replaced in-place depending on filesystem semantics.
- Fix: The atomic write pattern already mitigates this on Unix. Add an explicit note about the safety contract:

```rust
// SAFETY: The mmap is read-only. On POSIX, the builder uses atomic
// rename (tempfile::persist), so existing mmaps continue to reference
// the old inode even after a rebuild. On Windows, concurrent
// build + read is not safe -- callers must serialize access.
let mmap = unsafe { Mmap::map(&file) }?;
```

## Issues in Code You Touched (Should Fix)

### LOW

**`unwrap_or(u32::MAX)` silently saturates stats instead of reporting overflow** - `builder.rs:106-107`
**Confidence**: 80%
- Problem: `u32::try_from(pairs.len()).unwrap_or(u32::MAX)` silently saturates if pair/file counts exceed `u32::MAX`. While `MAX_PAIRS` (2M) is well within `u32::MAX` (4.29B) on 64-bit targets, on a 32-bit target the `pairs.len()` is already bounded by `MAX_PAIRS` and `usize` would be 32-bit anyway. The issue is that these stat values feed into `CochangeStats` which is `Serialize`/`Deserialize` -- a saturated `u32::MAX` value would be misleading if ever displayed or logged.
- Fix: Since `MAX_PAIRS` is 2M (well within u32), this is safe in practice. Consider adding a `debug_assert!` to catch any future changes that raise the cap above `u32::MAX`:

```rust
debug_assert!(pairs.len() <= u32::MAX as usize, "pair count exceeds u32 reporting capacity");
stats.pair_count = u32::try_from(pairs.len()).unwrap_or(u32::MAX);
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Potential DoS via `pairs_for_file` linear scan** - `reader.rs:189-208` (Confidence: 65%) -- `pairs_for_file` performs O(pair_count) linear scan over up to 2M entries (~24 MB). If exposed to untrusted callers, repeated calls could consume significant CPU. The code documents this explicitly and suggests a future binary-search optimization. Not blocking since MAX_PAIRS caps the upper bound and callers are internal.

- **No file locking on `cochange.skcc`** - `builder.rs:294-306`, `reader.rs:70-121` (Confidence: 60%) -- Neither the builder nor reader acquires advisory file locks. Two concurrent builders could race on `atomic_write`. On Unix, the last rename wins (safe for data integrity due to atomic rename), but the loser's work is silently discarded. Not blocking since this is typical for cache files, and the atomic write prevents corruption.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 1 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Conditions

1. Add explicit documentation that CRC32 is for accidental corruption detection only, not tamper resistance (MEDIUM -- 1 line doc comment).
2. Expand the `unsafe` mmap SAFETY comment to document the atomic-rename mitigation and Windows caveat (MEDIUM -- 3 lines).

### What Passed Security Review

- **Input validation on binary format parsing**: Thorough. Magic bytes validated, version checked, all size fields use `checked_mul`/`checked_add` to prevent integer overflow, file size validated against computed expected size, truncated data caught at every decode boundary.
- **Integer overflow protection**: Comprehensive use of `checked_mul`, `checked_add`, `saturating_add` throughout both builder and reader. No unchecked arithmetic on untrusted values.
- **Bounds checking on mmap access**: All slice accesses into the mmap use pre-validated offsets computed with checked arithmetic at `open()` time. The `read_array` helper validates bounds before every extraction.
- **Safety caps**: `COUPLING_MAX_FILES=50` prevents quadratic blowup from bulk commits. `MAX_PAIRS=2M` caps memory growth. Both are well-chosen.
- **Atomic file writes**: `tempfile::NamedTempFile` + `persist` ensures readers never observe partial writes. Explicit `0o644` permissions on Unix prevent world-writable files from permissive umask.
- **No path traversal**: Output path is constructed by joining a fixed filename (`cochange.skcc`) to a caller-provided directory. No user-controlled path components.
- **No `unwrap()`/`expect()`/`panic!()` in non-test code**: Confirmed. All error paths return `Result` types. The `unwrap_or` usage is safe saturation, not panicking.
- **Self-pair exclusion**: Duplicate paths within commits are deduplicated before pair generation, preventing invariant violation (`file_a < file_b`).
- **No hardcoded secrets or credentials**: Clean.
- **No network access or external service calls**: Pure local file I/O.
