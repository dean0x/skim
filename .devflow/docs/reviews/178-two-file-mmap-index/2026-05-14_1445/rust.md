# Rust Review Report

**Branch**: feat/178-two-file-mmap-index -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**`read_array` can panic on out-of-bounds slice indexing** - `format.rs:141`
**Confidence**: 90%
- Problem: `data[start..start + N]` performs an unchecked slice index. If `start + N > data.len()`, this panics rather than returning an `Err`. The doc comment says "Callers must check the minimum data length before calling this," but that contract is not enforced by the type system. Current callers (`decode_header`, `decode_entry`, `decode_posting`, `decode_file_meta`) do validate the minimum slice length before calling, so this is not exploitable via the current call sites. However, `lookup_ngram` at line 311 calls `read_array(entries_data, offset, ...)` where `offset = mid * SKIDX_ENTRY_SIZE` and only the total slice length modulo is validated, not `offset + 2 <= entries_data.len()` explicitly. The binary search logic does guarantee `mid < n` so `offset + SKIDX_ENTRY_SIZE <= entries_data.len()`, making this safe in practice, but the function's contract is fragile.
- Fix: Use `data.get(start..start + N)` instead of `data[start..start + N]` to return a recoverable error on out-of-bounds rather than panicking. This aligns with the crate's `clippy::panic = "deny"` lint and defense-in-depth principle:
```rust
fn read_array<const N: usize>(
    data: &[u8],
    start: usize,
    context: &'static str,
) -> crate::Result<[u8; N]> {
    data.get(start..start + N)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| SearchError::IndexCorrupted(format!("{context}: slice out of bounds")))
}
```

**`expected_idx_size` computation can overflow on 32-bit targets** - `reader.rs:85-87`
**Confidence**: 82%
- Problem: The computation `SKIDX_HEADER_SIZE + (header.ngram_count as usize) * SKIDX_ENTRY_SIZE + (header.file_count as usize) * FILE_META_SIZE` uses wrapping arithmetic on `usize`. On 32-bit targets, a crafted index with `ngram_count = u32::MAX` would overflow `usize`, producing a smaller-than-expected value that could pass the size check. On 64-bit targets this is not exploitable (u32 * 14 fits in u64), but the crate does not restrict to 64-bit. The same pattern appears at `reader.rs:139`, `reader.rs:152`.
- Fix: Use `checked_mul` and `checked_add` for the size computation:
```rust
let entries_bytes = (header.ngram_count as usize)
    .checked_mul(SKIDX_ENTRY_SIZE)
    .ok_or_else(|| SearchError::IndexCorrupted("ngram_count * entry_size overflows".into()))?;
let meta_bytes = (header.file_count as usize)
    .checked_mul(FILE_META_SIZE)
    .ok_or_else(|| SearchError::IndexCorrupted("file_count * meta_size overflows".into()))?;
let expected_idx_size = SKIDX_HEADER_SIZE
    .checked_add(entries_bytes)
    .and_then(|s| s.checked_add(meta_bytes))
    .ok_or_else(|| SearchError::IndexCorrupted("idx size overflows usize".into()))?;
```

### MEDIUM

**`postings_file_size as usize` truncates on 32-bit** - `reader.rs:94`
**Confidence**: 85%
- Problem: `header.postings_file_size` is `u64` but is cast to `usize` for comparison with `post_mmap.len()`. On 32-bit targets, this silently truncates, potentially causing a corrupted index to pass validation. Same pattern at `reader.rs:160-161` where `posting_offset as usize` and `posting_length as usize` are used.
- Fix: Use `usize::try_from()` for the cast:
```rust
let expected_post_size = usize::try_from(header.postings_file_size).map_err(|_| {
    SearchError::IndexCorrupted("postings_file_size exceeds platform address space".into())
})?;
if post_mmap.len() != expected_post_size { ... }
```

**`entries.len() as u32` truncation in builder** - `builder.rs:238`
**Confidence**: 80%
- Problem: `entries.len() as u32` silently truncates if there are more than `u32::MAX` distinct bigrams. While there can be at most 65,536 distinct bigram keys (2^16), making overflow impossible for bigrams, this truncation pattern is fragile if the index evolves to larger n-gram keys. The comment explaining why this is safe is absent.
- Fix: Add an assertion or use checked conversion:
```rust
let ngram_count = u32::try_from(entries.len()).map_err(|_| {
    SearchError::IndexCorrupted("ngram_count exceeds u32::MAX".into())
})?;
```

**`tempfile` is a runtime dependency, not a dev-dependency** - `Cargo.toml:23`
**Confidence**: 90%
- Problem: `tempfile` is listed under `[dependencies]` rather than `[dev-dependencies]`. It is only used in `builder.rs` for atomic writes via `NamedTempFile`. This is a legitimate runtime usage (atomic write pattern), but `tempfile` pulls in `fastrand`, `once_cell`, `cfg-if`, and platform-specific crates. This is a design choice rather than a bug -- the atomic write pattern is valid. However, if this library is used in environments where minimal dependency footprint matters, the same atomic-write pattern can be achieved with `std::fs::rename` + manual temp file creation, avoiding the extra dependency.
- Fix: This is acceptable as-is given the safety `tempfile` provides (handles cleanup on drop, cross-platform atomicity). Consider noting the rationale with a comment in Cargo.toml, similar to the existing comment for `rskim-core`.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`file_meta_at` called repeatedly in hot loop** - `reader.rs:224-226` (Confidence: 70%) -- Inside the search loop, `file_meta_at` is called once per unique (doc_id, ngram) pair. For queries with many bigrams hitting the same documents, this re-decodes the same 5-byte metadata from the mmap repeatedly. Consider caching `FileMetaEntry` in a local `HashMap<u32, FileMetaEntry>` during the search, or pre-loading all metadata once if `file_count` is small.

- **`avg_doc_length` precision loss** - `builder.rs:177` (Confidence: 65%) -- `self.total_doc_length as f32 / self.file_count as f32` loses precision for large corpora because `f32` has only ~7 significant digits. With >16M files or >16M total bytes, the division becomes inaccurate. The value is stored as `f32` in the header, so this is bounded by format, but the intermediate computation could use `f64` and then cast to `f32` at the end: `(self.total_doc_length as f64 / self.file_count as f64) as f32`.

- **`start + N` addition in `read_array` could overflow `usize`** - `format.rs:141` (Confidence: 65%) -- If `start` is near `usize::MAX` and `N > 0`, `start + N` could wrap. This is extremely unlikely in practice since slice data is bounded by addressable memory, but `start.checked_add(N)` would be strictly correct.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The implementation demonstrates strong Rust practices: proper `Result` error handling throughout, `thiserror`-derived errors, `#[must_use]` on important returns, documented `unsafe` blocks with SAFETY comments, no `.unwrap()` outside tests (enforced via clippy lint), good use of const generics and fixed-size arrays for the codec, and a clean separation between pure codec (format.rs), I/O (builder.rs/reader.rs), and type definitions. The `SearchField` discriminant pattern with `#[repr(u8)]` and manual `from_discriminant` is correct and well-documented. The `read_array` panic-on-OOB issue is the most actionable finding -- switching to `.get()` would close the last gap in defense-in-depth for this binary format parser.
