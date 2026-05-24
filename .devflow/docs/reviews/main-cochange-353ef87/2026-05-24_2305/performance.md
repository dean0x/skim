# Performance Review Report

**Branch**: main (353ef87)
**Date**: 2026-05-24
**Focus**: Performance — memory allocation, mmap efficiency, algorithmic complexity, CRC32 cost, sorting, unnecessary copies

## Issues in Your Changes (BLOCKING)

### HIGH

**Double HashMap lookup in pair accumulation inner loop** - `builder.rs:186-192`
**Confidence**: 95%
- Problem: The inner loop calls `pair_counts.contains_key(&(a, b))` on line 186, then immediately calls `pair_counts.entry((a, b)).or_insert(0)` on line 191. Both operations hash the key `(a, b)` and traverse the HashMap bucket chain. In the hot inner loop of an O(n*k^2) algorithm (n commits, k files per commit), this doubles the hashing work for every pair on every commit. With `COUPLING_MAX_FILES=50`, a single commit can generate up to 1,225 pairs, each hashed twice.
- Fix: Use the `Entry` API to combine both operations into a single hash lookup:
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

**`pairs_for_file` O(n) linear scan over all pairs** - `reader.rs:189-208`
**Confidence**: 85%
- Problem: `pairs_for_file` scans every pair entry in the file (up to 2M entries = ~24 MB of mmap reads) to find matches for a single file ID. The data is sorted by `(file_a, file_b)`, so `file_a` matches form a contiguous range that could be found via binary search. However, `file_b` matches are scattered throughout, which is why a full scan is used. This is documented in the doc comment (lines 176-182) but still represents a significant bottleneck for the primary query use case. At 2M pairs, each call touches ~24 MB of data sequentially through mmap.
- Fix: For `file_a` matches, binary-search to the start of the `file_a` range and scan only within it. For `file_b` matches, the linear scan is unavoidable without a secondary index. A practical near-term improvement:
```rust
pub fn pairs_for_file(&self, file_id: FileId) -> Result<Vec<(FileId, u32)>> {
    let id = file_id.0;
    let pairs_data = self.pairs_slice();
    let n = pairs_data.len() / PAIR_ENTRY_SIZE;
    let mut results: Vec<(FileId, u32)> = Vec::new();

    // Binary search for the start of the file_a == id range
    let a_start = self.binary_search_file_a_start(pairs_data, n, id)?;
    // Scan the contiguous file_a range
    for i in a_start..n {
        let offset = i * PAIR_ENTRY_SIZE;
        let entry = super::format::decode_pair(&pairs_data[offset..offset + PAIR_ENTRY_SIZE])?;
        if entry.file_a != id { break; }
        results.push((FileId(entry.file_b), entry.count));
    }
    // file_b matches still require a scan (or a secondary index in a future version)
    for i in 0..n {
        let offset = i * PAIR_ENTRY_SIZE;
        let entry = super::format::decode_pair(&pairs_data[offset..offset + PAIR_ENTRY_SIZE])?;
        if entry.file_b == id {
            results.push((FileId(entry.file_a), entry.count));
        }
    }

    results.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(results)
}
```
  This still does a full scan for `file_b` matches, but eliminates linear search for the `file_a` dimension. A more complete fix would add a secondary sorted index or an offset table per file_a to the `.skcc` format (format version bump).

### MEDIUM

**HashMap capacity overestimate for `pair_counts`** - `builder.rs:137-138`
**Confidence**: 82%
- Problem: `pair_counts` is initialized with capacity `commits.len() * 4`. This heuristic assumes 4 new distinct pairs per commit, but for repositories with high file overlap across commits, most pairs will be duplicates — the HashMap will have far fewer entries than the pre-allocated capacity. For a history of 10,000 commits, this pre-allocates space for 40,000 entries (each entry is 24 bytes for the key `(u32, u32)` + 4 bytes for value + HashMap overhead = ~50-80 bytes), so ~2-3 MB. The actual distinct pair count may be much lower (e.g., 5,000 for a focused module).
- Fix: Use a more conservative initial capacity or let the HashMap grow naturally. Since `with_capacity` only avoids rehashing, the wasted memory is the concern rather than CPU cost. A simpler heuristic:
```rust
let mut pair_counts: HashMap<(u32, u32), u32> =
    HashMap::with_capacity(history.commits.len().min(max_pairs / 4));
```

**Redundant min/max in pair generation after sorted dedup** - `builder.rs:179-180`
**Confidence**: 90%
- Problem: After `ids.sort_unstable(); ids.dedup();` on line 167-168, the `ids` vector is already sorted ascending. Therefore in the nested loop `for i in 0..ids.len() { for j in (i+1)..ids.len() }`, we always have `ids[i] < ids[j]`. The `.min()` and `.max()` calls on lines 179-180 are unnecessary — `a` is always `ids[i]` and `b` is always `ids[j]`. These are cheap operations individually, but in the hot inner loop they add two comparisons and two conditional moves per pair.
- Fix: Replace with direct assignment:
```rust
let a = ids[i];
let b = ids[j];
```
  The `debug_assert!(a < b)` on line 183 already verifies this invariant.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **CRC32 computation on open is O(file_size), but acceptable** - `reader.rs:106-107` (Confidence: 65%) -- `crc32fast` uses hardware-accelerated CRC32C (SSE4.2/ARM CRC). At the MAX_PAIRS cap (2M pairs * 12 bytes = ~24 MB), this takes ~8ms on modern hardware. This is a one-time cost at `open()` and is the correct trade-off for data integrity. No action needed unless profiling shows it as a bottleneck on very large matrices.

- **Serialization allocates intermediate Vec structs before final buffer** - `builder.rs:214-232` (Confidence: 60%) -- `serialize()` collects `HashMap` entries into `Vec<FileCommitEntry>` and `Vec<PairEntry>`, sorts them, then encodes into the final `Vec<u8>`. The intermediate Vecs duplicate the data briefly (HashMap + Vec + final buffer). At 2M pairs this is ~72 MB peak (HashMap ~48 MB + Vec ~24 MB + output ~24 MB). This is a one-time build cost and the code is clear. A streaming approach writing directly to a sorted iterator would halve peak memory but add complexity.

- **`decode_pair` called per entry in `pairs_for_file` linear scan** - `reader.rs:197` (Confidence: 70%) -- Each iteration decodes 12 bytes through `decode_pair`, which calls `read_array` with bounds checks. For the linear scan path, raw pointer arithmetic with `from_le_bytes` directly on the slice (after a single upfront length validation) would avoid per-entry bounds checking. This is micro-optimization territory; profile before pursuing.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

### Assessment

The co-change module demonstrates solid performance engineering fundamentals: mmap-based reading avoids copying file data into heap, binary search is used for point lookups (`pair_count`, `file_commits`, `lookup_pair`), CRC32 uses hardware acceleration via `crc32fast`, buffer sizes use checked arithmetic to prevent overflow, and the `COUPLING_MAX_FILES=50` cap bounds the O(k^2) pair generation per commit to 1,225 pairs maximum.

The two HIGH findings are the most impactful. The double HashMap lookup in the hot inner loop (`contains_key` + `entry`) is a concrete 2x hashing overhead that the Rust `Entry` API was designed to eliminate. The `pairs_for_file` O(n) scan is documented and acceptable for v1 but should be improved before the pair count approaches the 2M cap, as scanning 24 MB of mmap data per query call will become a latency concern.

The MEDIUM findings (capacity overestimate and redundant min/max) are lower-impact but easy wins that improve clarity and reduce unnecessary work in the hot path.

Overall, the implementation is performance-conscious with appropriate safety caps, mmap usage, and algorithmic choices for the sorted data. The blocking issues are about eliminating known inefficiencies, not fundamental design problems.
