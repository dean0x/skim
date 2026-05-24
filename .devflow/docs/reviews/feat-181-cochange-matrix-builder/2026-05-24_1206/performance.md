# Performance Review Report

**Branch**: feat/181-cochange-matrix-builder -> main
**Date**: 2026-05-24

## Issues in Your Changes (BLOCKING)

### HIGH

**O(n^2) pair generation in `accumulate_pairs` with redundant `contains_key` before `entry`** - `builder.rs:146-161`
**Confidence**: 90%
- Problem: The inner pair-generation loop at lines 146-163 performs an O(n^2) iteration over file IDs per commit, which is expected and capped by `COUPLING_MAX_FILES=50` (max 1,225 pairs per commit). However, for each pair, the MAX_PAIRS guard calls `pair_counts.contains_key(&(a, b))` on line 155 *before* `pair_counts.entry((a, b))` on line 160. This causes two HashMap lookups for every new pair insertion: one for the bounds check and one for the entry API. For existing keys, the `contains_key` call is wasted entirely since the `or_insert(0)` path is never taken.
- Impact: With up to 2M pairs and thousands of commits, this doubles HashMap lookup cost during the accumulation phase. The HashMap key is `(u32, u32)` (8 bytes, cheap to hash), so the absolute cost per extra lookup is small, but at scale (large git histories) this adds measurable overhead. On a repo with 10K commits averaging 10 files each, this is ~450K extra hash lookups.
- Fix: Restructure to use a single `entry` call with a length check:
```rust
// Replace lines 154-161 with:
let len = pair_counts.len();
match pair_counts.entry((a, b)) {
    std::collections::hash_map::Entry::Occupied(mut e) => {
        *e.get_mut() = e.get().saturating_add(1);
    }
    std::collections::hash_map::Entry::Vacant(v) => {
        if len >= MAX_PAIRS {
            return Err(SearchError::IndexCorrupted(
                "co-change pair count exceeds safety limit".into(),
            ));
        }
        v.insert(1);
    }
}
```

**`pairs_for_file` performs O(pair_count) linear scan** - `reader.rs:163-182`
**Confidence**: 85%
- Problem: `pairs_for_file` iterates over ALL pair entries in the matrix to find partners for a given file. With up to 2M pairs, this is a full linear scan of up to 24MB of mmap'd data (2M * 12 bytes) per query. Since pairs are sorted by `(file_a, file_b)`, entries where `file_a == id` form a contiguous range that could be found via binary search. Entries where `file_b == id` are scattered, but the `file_a` range can be narrowed with a binary search for the lower bound.
- Impact: For search-ranking use cases where `pairs_for_file` is called for the query file and possibly multiple result files, this scales poorly. A matrix with 1M pairs requires scanning 12MB of data per call.
- Fix: For the `file_a` dimension, use binary search to find the contiguous range `[first entry where file_a == id, last entry where file_a == id]`, then scan only that range. The `file_b` dimension still requires a scan, but documenting this limitation and offering a `top_k` parameter would bound the cost:
```rust
/// Return top-K co-change partners for `file_id`, sorted by count descending.
///
/// Uses binary search for file_a matches; linear scan for file_b matches
/// remains O(pair_count) — acceptable for small matrices, revisit if profiling
/// shows this is hot.
pub fn pairs_for_file(&self, file_id: FileId) -> Result<Vec<(FileId, u32)>> {
    // ... existing code is acceptable for v1 given the MAX_PAIRS=2M cap,
    // but add a comment documenting the O(n) cost.
}
```

### MEDIUM

**HashMap initial capacity over-allocation for `pair_counts`** - `builder.rs:106-107`
**Confidence**: 82%
- Problem: `pair_counts` is pre-allocated with `history.commits.len().saturating_mul(4)`. For a repository with 50K commits, this allocates capacity for 200K entries upfront (~3.2MB for (u32,u32)->u32 entries). In practice, most pairs appear across multiple commits, so the actual distinct pair count is often much smaller than `commits * 4`. Meanwhile, many repositories have few commits where this wastes almost nothing.
- Impact: This is a conservative over-allocation that wastes memory temporarily during the build phase. The `saturating_mul` prevents overflow, which is good. For very large histories (100K+ commits), this could allocate ~6.4MB unnecessarily.
- Fix: Consider a smaller multiplier or a fixed initial capacity:
```rust
let mut pair_counts: HashMap<(u32, u32), u32> =
    HashMap::with_capacity(history.commits.len().min(10_000).saturating_mul(2));
```

**Triple buffer copy during serialization** - `builder.rs:207-248`
**Confidence**: 80%
- Problem: The `serialize` function builds three separate `Vec<u8>` buffers (`fc_buf`, `pair_buf`, and `buf`) then copies `fc_buf` and `pair_buf` into the final `buf`. This means every byte of payload data is written twice: once into its intermediate buffer (for CRC computation), then again via `extend_from_slice` into the final output buffer.
- Impact: For a matrix with 2M pairs (24MB of pair data) and 50K files (400KB of file-commit data), this copies ~24.4MB unnecessarily. The CRC32 computation requires a contiguous or streaming pass over the data, which justifies the intermediate buffers, but the final assembly could write directly to disk instead.
- Fix: Write the header, then the file-commit buffer, then the pair buffer directly to the temp file, avoiding the final `buf` assembly:
```rust
fn serialize_to_writer(
    writer: &mut impl std::io::Write,
    pair_counts: &HashMap<(u32, u32), u32>,
    file_commit_counts: &HashMap<u32, u32>,
) -> Result<()> {
    // Build fc_buf and pair_buf as before (needed for CRC)
    // ... compute checksum ...
    writer.write_all(&encode_header(&header))?;
    writer.write_all(&fc_buf)?;
    writer.write_all(&pair_buf)?;
    Ok(())
}
```
This eliminates the ~24MB final copy while keeping the CRC computation valid.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Consider using `BTreeMap` for sorted pair output** - `builder.rs:193-201` (Confidence: 65%) -- The `serialize` function collects pairs into a `Vec`, then sorts them. A `BTreeMap<(u32,u32), u32>` would maintain sort order during accumulation, eliminating the sort step. However, `BTreeMap` has worse cache locality than `HashMap` during the hot accumulation loop, so this is likely a net loss. Only worth benchmarking if serialization time dominates.

- **`file_commit_slice` and `pairs_slice` recompute offsets on every call** - `reader.rs:217-228` (Confidence: 70%) -- These methods recompute `fc_start`, `fc_end`, and `pairs_end` from the header on every query. Caching these offsets as fields in `CochangeMatrixReader` (computed once in `open()`) would save a few multiplications per query. Negligible for single queries but relevant if called in a tight loop.

- **Sort in `pairs_for_file` on every call** - `reader.rs:180` (Confidence: 65%) -- Results are sorted by count descending on every call. If the same file is queried repeatedly, consider caching results or offering an unsorted variant for callers who only need the top-1.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The core design is sound: mmap-based reads, binary search for point lookups, O(1) Jaccard via cached file-commit counts, safety caps on both per-commit file count and total pair count, and atomic writes. The two HIGH issues are the redundant HashMap lookup in the hot accumulation loop (easy fix, measurable at scale) and the O(n) linear scan in `pairs_for_file` (acceptable for v1 given the 2M pair cap, but should be documented and revisited). The MEDIUM issues around buffer copying and over-allocation are real but bounded by the MAX_PAIRS cap. Overall this is a well-engineered first version with appropriate safety limits.
