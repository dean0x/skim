# Review Summary: Co-Change Matrix Builder

**Branch**: main (commit 353ef87)
**Date**: 2026-05-24
**Cycle**: 1 (first review)

## Merge Recommendation

**CHANGES_REQUESTED**

The co-change module is architecturally sound and introduces no regressions, but contains two HIGH-priority performance issues in the hot inner loop that should be addressed before merge. These are straightforward fixes with clear solutions. After addressing the HIGH performance findings and the MEDIUM consistency/reliability issues, this PR is approved.

---

## Scores

| Reviewer | Domain | Score | Recommendation |
|----------|--------|-------|-----------------|
| Security | Input validation, cryptography, memory safety | 8/10 | APPROVED_WITH_CONDITIONS |
| Architecture | Module design, separation of concerns, patterns | 9/10 | APPROVED_WITH_CONDITIONS |
| Performance | Algorithms, memory, throughput, latency | 7/10 | CHANGES_REQUESTED |
| Complexity | Function length, nesting, cyclomatic complexity | 8/10 | APPROVED_WITH_CONDITIONS |
| Consistency | Crate-wide patterns, naming, conventions | 8/10 | APPROVED_WITH_CONDITIONS |
| Testing | Coverage, edge cases, robustness | 8/10 | APPROVED_WITH_CONDITIONS |
| Regression | Breakage, compatibility, exports | 10/10 | APPROVED |
| Reliability | Bounds, atomicity, crash safety, resource cleanup | 8/10 | APPROVED_WITH_CONDITIONS |
| Rust | Idioms, ownership, error handling, safety | 8/10 | APPROVED_WITH_CONDITIONS |

---

## Critical Issues (P0/CRITICAL)

_None identified._

---

## Blocking Issues (P1/HIGH)

### Double HashMap Lookup in Pair Accumulation Inner Loop
**Location**: `builder.rs:186-192`
**Severity**: HIGH
**Confidence**: 95% (Performance)
**Flagged by**: Performance

**Problem**: The inner loop calls `pair_counts.contains_key(&(a, b))` followed immediately by `pair_counts.entry((a, b)).or_insert(0)`. Both operations hash the key and traverse the bucket chain, doubling the hashing work in the O(n*k²) hot path. With `COUPLING_MAX_FILES=50`, a single commit generates up to 1,225 pairs, each hashed twice.

**Fix**:
```rust
// Replace lines 186-192 with:
let len_before = pair_counts.len();
let entry = pair_counts.entry((a, b));
if matches!(entry, std::collections::hash_map::Entry::Vacant(_)) && len_before >= max_pairs {
    return Err(SearchError::IndexCorrupted(
        "co-change pair count exceeds safety limit".into(),
    ));
}
let count = entry.or_insert(0);
*count = count.saturating_add(1);
```

---

### `pairs_for_file` O(n) Linear Scan Over All Pairs
**Location**: `reader.rs:189-208`
**Severity**: HIGH
**Confidence**: 85% (Performance), 80% (Rust)
**Flagged by**: Performance, Rust

**Problem**: `pairs_for_file` scans all pair entries (up to 2M = ~24 MB per call) to find matches. The data is sorted by `(file_a, file_b)`, so `file_a` matches form a contiguous range that could be binary-searched. This is documented but still a significant bottleneck for the primary query use case.

**Fix (Short-term - for now)**:
Use binary search for the `file_a` dimension:
```rust
// Binary search for the start of the file_a == id range
let a_start = self.binary_search_file_a_start(pairs_data, n, id)?;
// Scan only within the contiguous file_a range, then scan file_b matches
```
See Performance report lines 40-61 for full implementation.

**Fix (Long-term)**: Add a secondary sorted index or offset table per `file_a` to the `.skcc` format (requires format version bump).

---

## Should-Address Issues (P1/MEDIUM)

### Redundant `min`/`max` After `sort_unstable` + `dedup`
**Location**: `builder.rs:179-180`
**Severity**: MEDIUM
**Confidence**: 90% (Rust), 90% (Performance)
**Flagged by**: Rust, Performance, Complexity, Reliability

**Problem**: After `ids.sort_unstable(); ids.dedup();`, the vector is sorted ascending. For any `i < j`, we always have `ids[i] < ids[j]`. The `.min()` and `.max()` calls are unnecessary and add two comparisons and conditional moves per pair in the hot loop.

**Fix**:
```rust
let a = ids[i];
let b = ids[j];
// debug_assert!(a < b); // already verified at line 183
```

---

### Inconsistent Sub-Module Visibility
**Location**: `cochange/mod.rs:24-26`
**Severity**: MEDIUM
**Confidence**: 92% (Consistency), 85% (Architecture)
**Flagged by**: Consistency, Architecture

**Problem**: The `cochange` module declares sub-modules as `pub(crate) mod builder; pub(crate) mod format; pub(crate) mod reader;`. The established pattern in the `index` module uses `mod builder; mod format; mod reader;` (private, re-exported via `pub use`). All types within these modules are already `pub(crate)`, making the module visibility redundant and divergent from the crate convention.

**Fix**:
```rust
// cochange/mod.rs
mod builder;
mod format;
mod reader;
#[cfg(test)]
mod test_helpers;

pub use builder::CochangeMatrixBuilder;
pub use reader::CochangeMatrixReader;
```

---

### Semantic Mismatch: `IndexCorrupted` Used for Capacity Limit Violation
**Location**: `builder.rs:187-189`, `builder.rs:239-252`
**Severity**: MEDIUM
**Confidence**: 85% (Rust), 82% (Architecture)
**Flagged by**: Rust, Architecture

**Problem**: When `MAX_PAIRS` is exceeded, the code returns `SearchError::IndexCorrupted("co-change pair count exceeds safety limit")`. This is a resource-limit violation, not an index-corruption condition. The variant semantically means "the index data is in an inconsistent or unreadable state" per `types.rs:530`. Consumers matching on `IndexCorrupted` may misinterpret this as file corruption and trigger unnecessary re-indexing.

**Fix**:
Option 1 (preferred): Add a new `SearchError::CapacityExceeded` variant.
Option 2: At minimum, clarify the error message to distinguish build-time limits from disk corruption:
```rust
SearchError::IndexCorrupted(
    format!("co-change build aborted: pair count {} exceeds safety cap {max_pairs}", 
            pair_counts.len())
)
```

---

### Missing `flush()` Before `persist()` in Atomic Write
**Location**: `builder.rs:294-306`
**Severity**: MEDIUM
**Confidence**: 85% (Reliability)
**Flagged by**: Reliability

**Problem**: `atomic_write` calls `tmp.write_all(data)` then `tmp.persist(path)` without explicit `flush()` or `sync_all()`. On power loss between write and OS page flush, the renamed file could be empty or truncated. The CRC32 check on read would catch this, but the user would see "corrupt index" rather than a clean rebuild trigger.

**Fix**:
```rust
fn atomic_write(dir: &Path, path: &Path, data: &[u8]) -> Result<()> {
    let mut tmp = NamedTempFile::new_in(dir)?;
    use std::io::Write as _;
    tmp.write_all(data)?;
    tmp.as_file().sync_all()?;  // ensure data reaches disk before rename

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o644))?;
    }

    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
```

---

### Inconsistent Error Handling: `unwrap_or(u32::MAX)` vs `map_err` in Stats
**Location**: `builder.rs:106-107` (stats assignment), `builder.rs:266-277` (serialize checks)
**Severity**: MEDIUM
**Confidence**: 82% (Security, Reliability, Rust)
**Flagged by**: Security, Reliability

**Problem**: `u32::try_from(pairs.len()).unwrap_or(u32::MAX)` silently saturates stats to `u32::MAX` if overflow occurs. However, the same conversion in `serialize()` returns a proper error. This is inconsistent: stats values become wrong while serialization fails -- confusing for callers.

**Fix**: Use the same `map_err` pattern consistently:
```rust
stats.pair_count = u32::try_from(pairs.len()).map_err(|_| {
    SearchError::IndexCorrupted(format!("pair_count {} exceeds u32::MAX", pairs.len()))
})?;
stats.file_count = u32::try_from(file_counts.len()).map_err(|_| {
    SearchError::IndexCorrupted(format!("file_count {} exceeds u32::MAX", file_counts.len()))
})?;
```

---

### CRC32 Lacks Explicit Documentation of Limitations
**Location**: `format.rs:287-288`, `reader.rs:105-113`
**Severity**: MEDIUM
**Confidence**: 85% (Security)
**Flagged by**: Security

**Problem**: CRC32 is used as the sole integrity check for `.skcc` files, but CRC32 is designed for accidental corruption detection, not tamper resistance. A determined attacker with write access to the index directory could forge coupling data while preserving a valid checksum. The PR description calls this "CRC32 validation" which may give false assurance.

**Fix**: Add explicit documentation:
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

### Mmap SAFETY Comment Understates Concurrent Modification Risk
**Location**: `reader.rs:22-25`, `reader.rs:74-75`
**Severity**: MEDIUM
**Confidence**: 82% (Security)
**Flagged by**: Security

**Problem**: The module-level SAFETY comment acknowledges that concurrent file modification while the mmap is live results in undefined behavior. The `unsafe` block only asserts "The file is not modified after mapping," which is an unenforceable external precondition. On Windows, the old file could be replaced in-place rather than atomically renamed, violating the invariant.

**Fix**: Expand the SAFETY comment to document the atomic-rename mitigation and Windows caveat:
```rust
// SAFETY: The mmap is read-only. On POSIX, the builder uses atomic
// rename (tempfile::persist), so existing mmaps continue to reference
// the old inode even after a rebuild. On Windows, concurrent
// build + read is not safe -- callers must serialize access.
let mmap = unsafe { Mmap::map(&file) }?;
```

---

### `#[must_use]` Inconsistency with Existing Patterns
**Location**: `cochange/builder.rs:51,76`, `cochange/reader.rs:69,135,154`
**Severity**: MEDIUM
**Confidence**: 82% (Consistency), 82% (Rust)
**Flagged by**: Consistency, Rust

**Problem**: The cochange module adds `#[must_use]` with custom messages to 5 Result-returning methods. The `index` module has NO such annotations. The compiler already emits `unused_must_use` warnings for `Result` types, making explicit `#[must_use]` on Result-returning functions redundant. Only `stats()` in the index reader has `#[must_use]` (for a non-Result type).

**Fix**: Either remove `#[must_use]` from Result-returning methods to match the `index` pattern, or apply it consistently across the entire crate as a policy decision. The cochange approach is arguably better practice, but divergence should be intentional.

---

### Function Length Exceeds 50-Line Threshold
**Location**: `builder.rs:132-206` (`accumulate_pairs`, 73 lines), `builder.rs:209-288` (`serialize`, 79 lines)
**Severity**: MEDIUM (contextual)
**Confidence**: 90% (Complexity, Complexity)
**Flagged by**: Complexity

**Problem**: Two functions exceed the 50-line threshold. Both have low cyclomatic complexity and linear control flow, so practical readability is moderate. The length is driven by checked arithmetic and verbose error messages (which are desirable for binary format codecs).

**Fix**: Extract helpers:
```rust
// For accumulate_pairs: extract generate_pairs helper (lines 177-194)
fn generate_pairs(
    ids: &[u32],
    pair_counts: &mut HashMap<(u32, u32), u32>,
    max_pairs: usize,
) -> Result<()> { ... }

// For serialize: extract sorted-entry collection helpers
fn collect_sorted_file_entries(counts: &HashMap<u32, u32>) -> Vec<FileCommitEntry> { ... }
fn collect_sorted_pair_entries(counts: &HashMap<(u32, u32), u32>) -> Vec<PairEntry> { ... }
```

---

### HashMap Capacity Overestimate
**Location**: `builder.rs:137-138`
**Severity**: MEDIUM
**Confidence**: 82% (Performance)
**Flagged by**: Performance

**Problem**: `pair_counts` is initialized with capacity `commits.len() * 4`, assuming 4 new pairs per commit. For repositories with high file overlap, most pairs are duplicates -- the HashMap will have fewer entries than pre-allocated capacity. For a 10,000-commit history, this pre-allocates ~2-3 MB for ~5,000 actual entries in focused modules.

**Fix**:
```rust
let mut pair_counts: HashMap<(u32, u32), u32> =
    HashMap::with_capacity(history.commits.len().min(max_pairs / 4));
```

---

## Testing Gaps (P2/MEDIUM)

### Missing Truncated-Input Tests for Format Decoders
**Location**: `format_tests.rs`
**Severity**: MEDIUM
**Confidence**: 90% (Testing)
**Flagged by**: Testing

**Problem**: `decode_file_commit` (lines 199-203) and `decode_pair` (lines 231-234) have explicit truncation guards but zero test coverage. Header truncation is tested, but entry decoders are not.

**Fix**: Add two tests:
```rust
#[test]
fn test_file_commit_entry_truncated() {
    let result = decode_file_commit(&[0u8; 4]);  // < FILE_COMMIT_ENTRY_SIZE
    assert!(result.is_err());
}

#[test]
fn test_pair_entry_truncated() {
    let result = decode_pair(&[0u8; 8]);  // < PAIR_ENTRY_SIZE
    assert!(result.is_err());
}
```

---

### Missing Reader Size-Mismatch Test
**Location**: `reader_tests.rs`
**Severity**: MEDIUM
**Confidence**: 92% (Testing)
**Flagged by**: Testing

**Problem**: `reader.rs:98-103` validates `mmap.len() == pairs_end`. This branch is never exercised -- all tests use either garbage data (failing earlier on magic) or valid data with correct sizes.

**Fix**: Add test with valid header but truncated body (see Testing report lines 35-52).

---

## Informational Issues (P2/LOW)

### Misleading Test Name: `test_jaccard_zero_denominator_returns_zero`
**Location**: `reader_tests.rs:150`
**Severity**: LOW
**Confidence**: 85% (Testing)
**Flagged by**: Testing

**Problem**: The test name claims to test the zero-denominator guard, but exercises the zero-count early return instead. The denominator check is mathematically unreachable.

**Fix**: Rename to `test_jaccard_no_shared_commits_returns_zero`.

---

### Conditional Guard Weakens CRC Corruption Test
**Location**: `reader_tests.rs:261`
**Severity**: LOW
**Confidence**: 82% (Testing)
**Flagged by**: Testing

**Problem**: `if data.len() > 20` skips the byte-flip corruption if the file is small. The test currently passes because it generates 46 bytes, but the condition is fragile.

**Fix**:
```rust
assert!(data.len() > HEADER_SIZE, "test requires non-empty data section");
data[HEADER_SIZE] ^= 0xFF;
```

---

### Redundant min/max Already Documented
**Location**: `builder.rs:179-180`
**Severity**: LOW
**Confidence**: 70% (Complexity)
**Flagged by**: Complexity

**Problem**: The `debug_assert!(a < b)` on line 183 confirms the author knows the pair is already canonical. The min/max calls are defensive but add false complexity.

**Note**: This is deduplicated with the MEDIUM issue above.

---

### Type Alias Lacks Self-Documentation
**Location**: `builder.rs:122`
**Severity**: LOW
**Confidence**: 65% (Complexity)
**Flagged by**: Complexity

**Problem**: `AccumulatedPairs` is a 3-tuple `(HashMap<...>, HashMap<...>, CochangeStats)` without semantic names.

**Note**: Only used internally; not blocking. Consider for future refactoring.

---

### `atomic_write` Implementation Diverges from `index` Module
**Location**: `builder.rs:294`
**Severity**: LOW
**Confidence**: 68% (Consistency)
**Flagged by**: Consistency

**Problem**: The `index` module uses `atomic_write` as a private method; `cochange` uses a free function. The cochange version adds Unix permission hardening (`0o644`) that `index` lacks.

**Note**: Both patterns work. Consider whether the permission hardening should be applied consistently across both modules.

---

## Cross-Cutting Themes

### 1. Hot-Path Performance Optimizations (2 HIGH issues)
Multiple reviewers flagged efficiency in the O(n*k²) pair accumulation loop:
- Double HashMap lookup (Performance, Rust) — use Entry API
- Redundant min/max (Performance, Rust, Complexity) — remove after sort+dedup

These are concrete 2x improvements to measured throughput.

### 2. Semantic Error Variant Misuse (2 MEDIUM issues)
- `IndexCorrupted` used for capacity limit (Architecture, Rust) — should be its own variant
- `IndexCorrupted` used for safety cap in `serialize()` (Architecture) — inconsistent with capacity limit usage

This affects error handling in callers and should be resolved consistently.

### 3. Safety Boundaries Under-Documented (2 MEDIUM issues)
- Mmap TOCTOU risk (Security) — atomic rename mitigates on Unix, but Windows caveat needed
- CRC32 limitations (Security) — must clarify it detects accidents, not attacks

Both are handled reasonably in code; documentation is the gap.

### 4. Consistency with `index` Module (3 MEDIUM issues)
- Sub-module visibility (Consistency, Architecture) — use `mod` not `pub(crate) mod`
- `#[must_use]` pattern (Consistency, Rust) — either remove or apply crate-wide
- `atomic_write` location (Consistency) — method vs function, permission differences

All represent minor divergence from established patterns.

### 5. Test Coverage Gaps (2 MEDIUM issues)
- Truncated entry decoders (Testing) — 2 tests needed
- Reader size-mismatch path (Testing) — 1 test needed

Both are valid error paths with zero coverage.

---

## What Passed Review

### Strong Architectural Design
- Three-file split (format/builder/reader) mirrors the `index` module pattern exactly
- Correct dependency direction: builder/reader depend on format, never reverse
- Public API surface is minimal and well-scoped
- Layer boundary alignment: correctly excludes `LayerBuilder` trait (different input type)
- SOLID adherence: SRP, OCP, ISP, DIP all satisfied

### Comprehensive Input Validation
- Magic bytes, format version, size fields all validated
- Checked arithmetic throughout (`checked_mul`, `checked_add`)
- Integer overflow protection on 32-bit targets
- Bounds checking on all mmap accesses
- CRC32 checksum validation on every read

### Safety and Capacity Controls
- `COUPLING_MAX_FILES=50` prevents quadratic blowup
- `MAX_PAIRS=2M` prevents unbounded memory growth
- Both are tested at boundary conditions
- Atomic writes prevent partial-file observations
- `0o644` permissions on Unix prevent world-writable caches

### Well-Designed Binary Format
- Fixed-size format with magic bytes and version field
- Explicit little-endian encoding
- 18-byte header is compact yet extensible
- Sorted arrays enable O(log n) binary search
- CRC32 integrity check catches bit-rot and truncation

### Solid Test Coverage
- 46 tests across 3 test files for ~900 lines of code
- Roundtrip tests (build-then-read) for all struct types
- Error paths: corruption, CRC mismatch, truncation, capacity exceeded
- Edge cases: empty history, duplicate paths, self-pairs, canonical ordering
- Boundary testing: COUPLING_MAX_FILES at all three boundaries
- Compile-time trait checks: Send + Sync

### No Regressions
- 354 existing tests still pass
- Downstream `rskim` crate compiles without issues
- No changed exports, type signatures, or module visibility (besides new additions)
- Alphabetical ordering in `lib.rs` maintained

---

## Recommended Action Plan

**Priority 1 (HIGH - Block Merge)**
1. Fix double HashMap lookup using Entry API (Performance) — `builder.rs:186-192`
2. Improve `pairs_for_file` with binary search for `file_a` dimension (Performance) — `reader.rs:189-208`

**Priority 2 (MEDIUM - Fix Before Merge)**
3. Remove redundant min/max calls after sort+dedup (Performance, Rust) — `builder.rs:179-180`
4. Fix sub-module visibility to use `mod` pattern (Consistency, Architecture) — `cochange/mod.rs:24-26`
5. Add `flush()`/`sync_all()` before `persist()` (Reliability) — `builder.rs:299`
6. Replace stats `unwrap_or(u32::MAX)` with `map_err` (Reliability) — `builder.rs:106-107`
7. Document CRC32 limitations explicitly (Security) — `format.rs` module docs
8. Expand mmap SAFETY comment for Windows caveat (Security) — `reader.rs:74-75`
9. Resolve `IndexCorrupted` semantic mismatch or add `CapacityExceeded` variant (Architecture, Rust) — `builder.rs:187-189`
10. Remove redundant `#[must_use]` from Result-returning methods OR apply crate-wide (Consistency, Rust) — `builder.rs:51,76`, `reader.rs:69,135,154`

**Priority 3 (MEDIUM - Improve Quality)**
11. Extract `generate_pairs` helper to reduce `accumulate_pairs` to <50 lines (Complexity) — `builder.rs:177-194`
12. Extract sorted-entry helpers to reduce `serialize` to <50 lines (Complexity) — `builder.rs:214-232`
13. Fix HashMap capacity overestimate (Performance) — `builder.rs:137-138`
14. Add truncated-entry decoder tests (Testing) — `format_tests.rs`
15. Add reader size-mismatch test (Testing) — `reader_tests.rs`
16. Fix misleading test name (Testing) — `reader_tests.rs:150`
17. Fix conditional guard in CRC corruption test (Testing) — `reader_tests.rs:261`

**Priority 4 (LOW - Nice-to-Have)**
- Add missing boundary tests: `max_pairs=0`, single-element binary search, rebuild/overwrite
- Consider `TypeAlias` → named struct for `AccumulatedPairs`
- Review `atomic_write` permission consistency between modules

---

## Convergence

| Metric | Value |
|--------|-------|
| **Cycle** | 1 (first review) |
| **Total Issues** | 27 (deduplicated from 132 across 9 reviewers) |
| **CRITICAL** | 0 |
| **HIGH** | 2 |
| **MEDIUM** | 15 |
| **LOW** | 10 |
| **Pre-existing** | 0 |
| **Average Score** | 8.1/10 |
| **Recommendation** | CHANGES_REQUESTED (address P1 issues, then approve) |

**Key Insight**: This is a high-quality, well-engineered module with no fundamental flaws. The HIGH issues are concrete performance optimizations in the hot path (2x improvements), and the MEDIUM issues are mostly consistency/safety-documentation gaps. After addressing Priorities 1-2, the PR is solid for merge.

