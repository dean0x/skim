# Reliability Review Report

**Branch**: feat/178-two-file-mmap-index -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**Unchecked `as u32` truncation of `entries.len()` in builder** - `builder.rs:238`
**Confidence**: 90%
- Problem: `entries.len() as u32` silently truncates if the number of distinct bigrams exceeds `u32::MAX` (4,294,967,295). While the theoretical maximum for 2-byte bigrams is only 65,536 distinct keys, this cast pattern is inconsistent with the careful overflow checking applied to other values in the same function (`posting_length` uses `u32::try_from`, `doc_length` uses `u32::try_from`). More critically, it sets a bad precedent for future n-gram sizes (3-grams, 4-grams) where the key space could exceed `u32::MAX`.
- Fix: Use `u32::try_from(entries.len())` for consistency with the adjacent overflow checks:
```rust
let ngram_count = u32::try_from(entries.len()).map_err(|_| {
    SearchError::IndexCorrupted(format!(
        "ngram count {} exceeds u32::MAX",
        entries.len()
    ))
})?;
// ...
ngram_count: ngram_count,
```

**Unchecked arithmetic overflow in `expected_idx_size` computation** - `reader.rs:85-87`
**Confidence**: 85%
- Problem: The expression `SKIDX_HEADER_SIZE + (header.ngram_count as usize) * SKIDX_ENTRY_SIZE + (header.file_count as usize) * FILE_META_SIZE` can overflow `usize` on 32-bit platforms (where `usize` is 32 bits) if a crafted/corrupt header contains large `ngram_count` or `file_count` values. This would wrap to a small value, pass the size-match check against the (potentially small) mmap length, and allow subsequent reads to go out of bounds. On 64-bit platforms the risk is negligible since `u32::MAX * 14` fits in `u64`, but `usize` is platform-dependent.
- Fix: Use checked arithmetic:
```rust
let entries_bytes = (header.ngram_count as usize)
    .checked_mul(SKIDX_ENTRY_SIZE)
    .ok_or_else(|| SearchError::IndexCorrupted("ngram_count * entry_size overflows".into()))?;
let meta_bytes = (header.file_count as usize)
    .checked_mul(FILE_META_SIZE)
    .ok_or_else(|| SearchError::IndexCorrupted("file_count * meta_size overflows".into()))?;
let expected_idx_size = SKIDX_HEADER_SIZE
    .checked_add(entries_bytes)
    .and_then(|v| v.checked_add(meta_bytes))
    .ok_or_else(|| SearchError::IndexCorrupted("expected_idx_size overflows".into()))?;
```

**Unchecked `start + posting_length` addition can overflow on 32-bit** - `reader.rs:160-161`
**Confidence**: 85%
- Problem: `let start = entry.posting_offset as usize; let end = start + entry.posting_length as usize;` can overflow `usize` on 32-bit platforms if a corrupted index contains large `posting_offset` and `posting_length` values. The subsequent bounds check `end > self.post_mmap.len()` would pass incorrectly if `end` wrapped around to a small value. This is the same class of issue as the `expected_idx_size` overflow.
- Fix: Use `start.checked_add(entry.posting_length as usize)`:
```rust
let start = entry.posting_offset as usize;
let end = start.checked_add(entry.posting_length as usize).ok_or_else(|| {
    SearchError::IndexCorrupted(format!(
        "posting slice offset+length overflows for key {ngram_key:#06x}"
    ))
})?;
```

### MEDIUM

**Missing validation that `posting_length` is a multiple of `POSTING_ENTRY_SIZE`** - `reader.rs:170`
**Confidence**: 88%
- Problem: `let n = entry.posting_length as usize / super::format::POSTING_ENTRY_SIZE;` uses integer division. If a corrupt index has a `posting_length` that is not a multiple of `POSTING_ENTRY_SIZE` (9), the trailing bytes are silently ignored. This is a data integrity gap -- the `lookup_ngram` function validates entry alignment in the index table but the posting list reader does not perform the equivalent check.
- Fix: Add a divisibility precondition:
```rust
let posting_len = entry.posting_length as usize;
if posting_len % super::format::POSTING_ENTRY_SIZE != 0 {
    return Err(SearchError::IndexCorrupted(format!(
        "posting_length {} for key {ngram_key:#06x} is not a multiple of POSTING_ENTRY_SIZE {}",
        posting_len,
        super::format::POSTING_ENTRY_SIZE
    )));
}
let n = posting_len / super::format::POSTING_ENTRY_SIZE;
```

**Missing `postings_file_size` truncation check on 32-bit platforms** - `reader.rs:94`
**Confidence**: 82%
- Problem: `header.postings_file_size as usize` truncates the `u64` value on 32-bit platforms. If a `.skpost` file was written on a 64-bit system with a size exceeding `u32::MAX`, opening it on a 32-bit system would silently truncate the expected size, and the comparison against `post_mmap.len()` could spuriously pass or give a misleading error.
- Fix: Validate that `postings_file_size` fits in `usize` before casting:
```rust
let expected_post_size = usize::try_from(header.postings_file_size).map_err(|_| {
    SearchError::IndexCorrupted(format!(
        "postings_file_size {} exceeds platform usize",
        header.postings_file_size
    ))
})?;
if post_mmap.len() != expected_post_size { ... }
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`file_count` can silently wrap if `add_file` is called more than `u32::MAX` times** - `builder.rs:156`
**Confidence**: 80%
- Problem: `self.file_count += 1` will panic in debug mode or wrap in release mode when `file_count` reaches `u32::MAX`. This is extremely unlikely in practice (4 billion files), but violates the reliability principle that every counter must have a known bound. The `seen_file_ids` `HashSet<u32>` would also reach its maximum capacity at that point.
- Fix: Use `checked_add` or add a precondition at the top of `add_file`:
```rust
if self.file_count == u32::MAX {
    return Err(SearchError::IndexCorrupted(
        "file count exceeds u32::MAX".into(),
    ));
}
```

## Pre-existing Issues (Not Blocking)

No pre-existing reliability issues found in the modified files.

## Suggestions (Lower Confidence)

- **Temp file leak on power loss between the two atomic writes** - `builder.rs:256-257` (Confidence: 65%) -- If the process is killed after `.skpost` is written but before `.skidx` is committed, a valid `.skpost` file exists without its companion. The atomicity contract (documented in the header comment) handles this correctly -- readers will fail with "file not found" for `.skidx`. However, the orphaned `.skpost` file is never cleaned up. Consider documenting that callers should check for and remove stale `.skpost` files.

- **`read_array` panics on out-of-bounds slice** - `format.rs:141` (Confidence: 60%) -- The `data[start..start + N]` expression will panic if `start + N > data.len()`. All call sites pre-validate the minimum data length, but the function itself lacks an internal bounds check. A debug_assert or explicit bounds check inside `read_array` would provide defense-in-depth.

- **HashMap allocations in search hot path** - `reader.rs:210-211` (Confidence: 70%) -- `doc_tf` and `doc_positions` HashMaps are allocated fresh on every `search()` call with no pre-sizing hint. For indices with many documents, this leads to multiple reallocations. Consider pre-sizing with `HashMap::with_capacity` based on `header.file_count`.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 3 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The code demonstrates strong reliability discipline overall -- checked arithmetic for posting lengths, validated content sizes with `u32::try_from`, CRC32 integrity checking, atomic writes, sequential FileId enforcement, and comprehensive error paths returning `Result` instead of panicking. The main gaps are a few unchecked `as` casts that could cause silent truncation or arithmetic overflow on 32-bit platforms, and a missing alignment check on posting list data from disk. These are all straightforward fixes that bring the remaining casts in line with the careful overflow checking already present elsewhere in the code.
