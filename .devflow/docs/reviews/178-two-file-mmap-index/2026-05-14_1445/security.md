# Security Review Report

**Branch**: feat/178-two-file-mmap-index -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**Panic-on-slice in `read_array` with crafted index data** - `crates/rskim-search/src/index/format.rs:141`
**Confidence**: 92%
- Problem: `read_array` performs `data[start..start + N]` which will **panic** (out-of-bounds slice) if `start + N > data.len()`. The function's doc comment says "Callers must check the minimum data length before calling this," and while `decode_header`, `decode_entry`, `decode_posting`, and `decode_file_meta` each validate the minimum total length first, the `read_array` function itself does not defend against out-of-bounds access. The `try_into()` error path only catches the conversion to a fixed-size array, not the slicing. Additionally, `lookup_ngram` (line 311) calls `read_array` on a calculated `offset` that is always within the validated range for the overall buffer size, but the defense is implicit. If a future caller passes a bad `start`, the panic is silent and immediate. When opening a **malicious or malformed index file**, the bounds check depends entirely on upstream callers being correct -- this is defense-in-depth failure for a binary format parser reading untrusted data.
- Fix: Use `data.get(start..start + N)` instead of `data[start..start + N]` to convert a panic into a recoverable error:
```rust
fn read_array<const N: usize>(
    data: &[u8],
    start: usize,
    context: &'static str,
) -> crate::Result<[u8; N]> {
    data.get(start..start + N)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| SearchError::IndexCorrupted(format!("{context}: out of bounds or conversion failed")))
}
```

**Integer overflow in `expected_idx_size` computation with crafted header** - `crates/rskim-search/src/index/reader.rs:85-87`
**Confidence**: 88%
- Problem: The `expected_idx_size` computation multiplies attacker-controlled header fields (`ngram_count` and `file_count`, each up to `u32::MAX`) by constant sizes and adds them together. On a 64-bit platform `usize` is 8 bytes so the multiplication itself won't overflow, but on a **32-bit platform** (`usize` is 4 bytes), `(header.ngram_count as usize) * SKIDX_ENTRY_SIZE` can silently wrap around to a small value. If the wrapped value happens to match `idx_mmap.len()`, the size validation passes and subsequent code will read out-of-bounds from the mmap. While 32-bit targets are not the primary deployment target, this crate is published to crates.io and may be used in WebAssembly (wasm32) or embedded contexts.
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
    .ok_or_else(|| SearchError::IndexCorrupted("expected_idx_size overflows".into()))?;
```

### MEDIUM

**Truncating `as u32` cast for `entries.len()` in builder** - `crates/rskim-search/src/index/builder.rs:238`
**Confidence**: 85%
- Problem: `ngram_count: entries.len() as u32` silently truncates if there are more than `u32::MAX` entries. While reaching 2^32 distinct bigrams is theoretically impossible (only 65,536 possible bigram keys), the `as u32` pattern is inconsistent with the careful `u32::try_from` usage elsewhere in the same file (lines 127, 201). This inconsistency makes the code harder to audit and sets a bad precedent. The same issue applies to `postings_buf.len() as u64` on line 240, though overflow there is even less likely.
- Fix: Use `u32::try_from(entries.len())` for consistency:
```rust
ngram_count: u32::try_from(entries.len()).map_err(|_| {
    SearchError::IndexCorrupted("ngram_count exceeds u32::MAX".into())
})?,
```

**`postings_file_size` truncation on 32-bit when cast back to `usize`** - `crates/rskim-search/src/index/reader.rs:94`
**Confidence**: 82%
- Problem: `header.postings_file_size as usize` silently truncates on 32-bit platforms. A malicious index could set `postings_file_size` to a value like `0x1_0000_0000` which truncates to `0` on 32-bit, potentially passing the size check if the postings file is also empty or crafted to match.
- Fix: Use `usize::try_from`:
```rust
let expected_post_size = usize::try_from(header.postings_file_size)
    .map_err(|_| SearchError::IndexCorrupted("postings_file_size exceeds platform usize".into()))?;
if post_mmap.len() != expected_post_size {
```

**`start + N` addition in `read_array` can overflow on 32-bit** - `crates/rskim-search/src/index/format.rs:141`
**Confidence**: 80%
- Problem: If `start` is close to `usize::MAX` (possible with crafted data on 32-bit), `start + N` can wrap around to a small value, causing the slice to succeed on the wrong data rather than panicking. This compounds the panic issue described in the HIGH finding above.
- Fix: The `.get()` approach recommended above naturally handles this because `get()` on a range checks bounds without panicking or wrapping.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Unchecked overflow in `file_meta_at` offset computation** - `crates/rskim-search/src/index/reader.rs:139-140`
**Confidence**: 83%
- Problem: `entries_end + (file_index as usize) * FILE_META_SIZE` can overflow on 32-bit platforms when `file_index` is large. While the subsequent bounds check on line 141 (`offset + FILE_META_SIZE > self.idx_mmap.len()`) would catch the *wrapped* value in most cases, a carefully crafted `file_index` could produce a wrapped `offset` that happens to land within the mmap. The `file_meta_at` function is called from `search()` with `doc_id` values read from the posting list, which are attacker-controlled in a malicious index.
- Fix: Use checked arithmetic:
```rust
let entries_end = SKIDX_HEADER_SIZE
    .checked_add((self.header.ngram_count as usize).checked_mul(SKIDX_ENTRY_SIZE)
        .ok_or_else(|| SearchError::IndexCorrupted("overflow".into()))?)
    .ok_or_else(|| SearchError::IndexCorrupted("overflow".into()))?;
let offset = entries_end
    .checked_add((file_index as usize).checked_mul(FILE_META_SIZE)
        .ok_or_else(|| SearchError::IndexCorrupted("overflow".into()))?)
    .ok_or_else(|| SearchError::IndexCorrupted("overflow".into()))?;
```

**`doc_id` not validated against `file_count` in posting decode path** - `crates/rskim-search/src/index/reader.rs:224`
**Confidence**: 80%
- Problem: In `search()` at line 224, `doc_id` values read from the posting list are compared against `self.header.file_count`, and if `doc_id >= file_count` the code falls through with `doc_len = 0`. This gracefully handles the case, but it means BM25 scoring silently proceeds with corrupted/invalid document references rather than flagging them. A malicious index could inject arbitrary `doc_id` values that the reader silently accepts and returns in `SearchResult`. The caller would then use these `FileId` values to look up file paths, potentially accessing the wrong files.
- Fix: Consider returning an error or at minimum filtering out results with invalid `doc_id`:
```rust
if doc_id >= self.header.file_count {
    continue; // skip invalid doc references rather than scoring them
}
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Mmap SAFETY comment could be more precise** - `crates/rskim-search/src/index/reader.rs:78` (Confidence: 65%) -- The SAFETY comment acknowledges that concurrent modification is UB, but does not mention that `Mmap::map` also requires the file to remain valid for the lifetime of the map. Consider using `MmapOptions::populate()` or advisory file locks for production use with concurrent writers.

- **CRC32 is not a cryptographic integrity check** - `crates/rskim-search/src/index/format.rs:327` (Confidence: 62%) -- CRC32 detects accidental corruption but provides no protection against deliberate tampering. If index files could be attacker-modified (e.g., shared network storage), CRC32 offers no security guarantee. This is acceptable for the stated purpose (integrity checking), but worth documenting the threat model boundary.

- **`posting_length` not checked as multiple of `POSTING_ENTRY_SIZE` at open time** - `crates/rskim-search/src/index/reader.rs:170` (Confidence: 70%) -- In `lookup_postings`, `entry.posting_length as usize / POSTING_ENTRY_SIZE` silently drops the remainder if `posting_length` is not aligned. A malicious index could set a non-aligned `posting_length` causing the loop to skip trailing bytes. Validating alignment during decode or lookup would be more robust.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | - | 2 | 2 | - |
| Should Fix | - | - | 2 | - |
| Pre-existing | - | - | - | - |

**Security Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The code demonstrates strong security awareness overall: bounds checks on decode functions, CRC32 integrity verification, atomic writes, checked arithmetic on the builder's hot path, and no `unwrap()`/`panic!()` in non-test code. The two HIGH findings both relate to the same root cause -- the `read_array` helper can panic on crafted input, and the reader's size validation arithmetic can overflow on 32-bit platforms. Both are addressable with straightforward safe-arithmetic changes. The MEDIUM findings about `as` cast consistency and `doc_id` validation are lower risk but worth fixing for defense-in-depth against malicious index files.
