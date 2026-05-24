# Architecture Review Report

**Branch**: main (commit 353ef87)
**Date**: 2026-05-24
**Scope**: `crates/rskim-search/src/cochange/` module (~1,881 lines across 10 files)

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Inconsistent sub-module visibility: `pub(crate)` vs `mod` (private)** - `cochange/mod.rs:24-26`
**Confidence**: 85%
- Problem: The `cochange` module declares its sub-modules as `pub(crate) mod builder`, `pub(crate) mod format`, `pub(crate) mod reader`. The established pattern in the `index` module uses plain `mod` (private) for sub-modules, re-exporting only the public types via `pub use`. Since all types within `builder`, `format`, and `reader` are already marked `pub(crate)`, the `pub(crate)` on the module declarations is redundant and deviates from the existing crate convention.
- Fix: Change sub-module declarations to match `index/mod.rs`:
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

**`SearchError::IndexCorrupted` used semantically for a capacity/resource limit, not corruption** - `builder.rs:187-189`
**Confidence**: 82%
- Problem: When the `MAX_PAIRS` safety cap is exceeded, the code returns `SearchError::IndexCorrupted("co-change pair count exceeds safety limit")`. This is a resource-limit violation, not an index-corruption condition. The same variant is used in `serialize()` for overflow conditions (lines 239-252). The `IndexCorrupted` variant semantically means "the index data is in an inconsistent or unreadable state" per `types.rs:530`. Exceeding an in-memory limit during construction is a different failure mode.
- Fix: Consider adding a `SearchError::CapacityExceeded` variant (or using a more descriptive message), or accept this as a pragmatic choice and document the semantic stretch. The existing error variants do not have a perfect fit, so if adding a new variant is undesirable, at minimum the error message should clarify this is a build-time safety cap, not disk corruption:
```rust
SearchError::IndexCorrupted(
    format!("co-change build aborted: pair count {} exceeds safety cap {max_pairs}", pair_counts.len())
)
```

## Issues in Code You Touched (Should Fix)

_None identified._

## Pre-existing Issues (Not Blocking)

_None identified._

## Suggestions (Lower Confidence)

- **`pairs_for_file` O(n) linear scan could be documented as a known limitation with a tracking mechanism** - `reader.rs:189-208` (Confidence: 70%) -- The doc comment already notes the O(pair_count) complexity and mentions a future binary-search optimisation, which is good. However, at the MAX_PAIRS cap of 2M entries this reads ~24MB per call. If this becomes a hot path, consider adding a `TODO` or issue reference so the optimization is tracked.

- **`Jaccard(a, a)` returns `0.0` rather than `1.0`** - `reader.rs:155-157` (Confidence: 65%) -- Mathematically, Jaccard similarity of a set with itself is 1.0 (identical sets). The implementation returns 0.0 for self-pairs as an explicit early-return. This may be an intentional domain decision (self-coupling is meaningless for "find files that co-change"), but it diverges from the standard mathematical definition. If intentional, a brief doc comment explaining the rationale would help.

- **`CochangeMatrixBuilder::new` validates directory existence eagerly but not atomically** - `builder.rs:52-60` (Confidence: 60%) -- The constructor checks `output_dir.exists()` but the directory could be removed between construction and the call to `build()`. This is a minor TOCTOU concern; the atomic_write call would catch it downstream with an IO error. The eager check provides a better UX error message, so this is likely an acceptable trade-off.

## Passed Checks

1. **Separation of concerns (format codec vs builder vs reader)**: Excellent. The three-file split mirrors the established `index/` module pattern exactly. `format.rs` is a pure codec (no I/O, no fs, no io::Write -- as documented in its module doc), `builder.rs` handles write-path I/O, and `reader.rs` handles read-path I/O with mmap. This is a textbook deep-module design.

2. **Dependency direction**: Correct. `builder.rs` and `reader.rs` both depend on `format.rs` (codec), never the reverse. The module depends on `crate::types` for `FileId`, `HistoryResult`, `CochangeStats`, `Result`, and `SearchError` -- all appropriate abstractions from the core types module. No infrastructure types leak outward.

3. **Layer boundary alignment**: The module correctly does NOT implement `LayerBuilder` (as documented in `builder.rs:39-40`), because it takes `HistoryResult` rather than raw file content. This is an honest acknowledgment that co-change analysis has a fundamentally different input type than content-based indexing layers.

4. **SOLID adherence**:
   - **SRP**: Each file has one reason to change -- codec format changes affect `format.rs`, build logic changes affect `builder.rs`, query logic changes affect `reader.rs`.
   - **OCP**: The format version field (`FORMAT_VERSION`) allows future format evolution without breaking existing readers (version check in `decode_header`).
   - **ISP**: The public API surface is minimal -- only `CochangeMatrixBuilder` and `CochangeMatrixReader` are re-exported. Internal types (`SkccHeader`, `PairEntry`, `FileCommitEntry`) are correctly `pub(crate)`.
   - **DIP**: The builder accepts `&HashMap<PathBuf, FileId>` as a caller-managed dependency rather than owning path resolution. The reader depends on the abstract `format` codec, not on builder internals.

5. **Error type design**: Consistent use of `crate::Result<T>` (= `Result<T, SearchError>`) throughout. No panics outside `#[cfg(test)]` -- enforced by `clippy::unwrap_used = "deny"` in Cargo.toml. All error paths return structured `SearchError` variants with descriptive messages including byte offsets and expected vs actual values.

6. **Safety and reliability patterns**:
   - Checked arithmetic throughout (`.checked_mul()`, `.checked_add()`, `u32::try_from()`) prevents overflow on 32-bit targets.
   - `MAX_PAIRS` cap (2M) prevents unbounded memory growth -- documented and tested.
   - `COUPLING_MAX_FILES` cap (50) filters noisy bulk-refactor commits.
   - Atomic writes via `tempfile::NamedTempFile::persist` prevent partial-write corruption.
   - CRC32 checksum validation on read catches bit-rot and truncation.
   - Explicit `0o644` permissions on Unix prevent world-writable files from permissive umasks.
   - `#[must_use]` annotations on all public methods that return Result.

7. **Binary format design**: Well-structured fixed-size format with magic bytes, version field, and CRC32 integrity check. Sorted arrays enable O(log n) binary search for lookups. Little-endian encoding is explicit. The 18-byte header is compact. File layout documentation is clear and consistent between module docs and inline comments.

8. **Test architecture**: Tests are separated into `_tests.rs` files (matching existing crate pattern), with shared helpers extracted into `test_helpers.rs`. Test coverage includes roundtrip (build-then-read), boundary conditions (empty history, self-pairs, exact-at-limit), error paths (corruption detection, CRC mismatch, capacity exceeded), and compile-time trait checks (Send + Sync).

9. **Type safety**: `FileId` newtype is used consistently rather than raw `u32`. Canonical pair ordering `(min, max)` is enforced at construction and transparently handled at query time.

10. **Consistency with existing `index/` module**: Module structure (mod.rs + builder + format + reader + tests), public API pattern (re-export only Builder + Reader), atomic write strategy, mmap-based reader, format versioning, and CRC32 integrity checks all follow the established patterns.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The `cochange/` module is architecturally sound. It follows established crate patterns (three-file codec/builder/reader split, mmap-based reader, atomic writes, CRC32 integrity), maintains correct dependency direction, and has a well-scoped public API surface. The two MEDIUM findings are minor consistency and semantic-precision issues, neither of which represents a structural risk. The sub-module visibility inconsistency (`pub(crate) mod` vs private `mod`) should be aligned with the `index/` module pattern before merge. The semantic stretch of `IndexCorrupted` for capacity limits is worth a brief discussion but not blocking.
