# Performance Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16T12:48

## Issues in Your Changes (BLOCKING)

### HIGH

**Per-byte allocation in `classify_source` scales linearly with file size** - `crates/rskim-search/src/lexical/classifier.rs:116`
**Confidence**: 85%
- Problem: `classify_source` allocates a `Vec<SearchField>` with one element per byte of source (`vec![SearchField::Other; len]`). For the project's stated large-file scenarios (up to 100MB before the rejection limit), this is a 100MB heap allocation of 1-byte enums just to classify fields. Even for typical 3000-line files (~100KB), this is a 100KB allocation plus a full pre-order tree walk that overwrites byte-by-byte using a `for byte in &mut field_at[start..end]` inner loop (line 138). The byte-by-byte overwrite loop is O(total_node_bytes), not O(nodes), meaning overlapping parent/child spans cause redundant writes.
- Fix: Consider a range-based approach directly. Instead of a per-byte array with innermost-wins overwrite, accumulate ranges from leaf nodes upward (post-order walk) and only record non-Other fields. This eliminates the per-byte allocation entirely and reduces the overwrite cost from O(sum_of_all_span_bytes) to O(nodes). Alternatively, if the current approach is acceptable for v1, add a size guard that rejects files above a reasonable threshold (e.g., 1MB) before allocating:
```rust
const MAX_CLASSIFY_SIZE: usize = 1_048_576; // 1MB
if len > MAX_CLASSIFY_SIZE {
    return Ok(vec![(0..len, SearchField::Other)]);
}
```

---

**`resolve_field` binary search called per byte-window in builder hot path** - `crates/rskim-search/src/index/builder.rs:161`
**Confidence**: 90%
- Problem: In `add_file_classified`, `resolve_field` is called once per 2-byte window (line 161). For a 100KB file, that is ~100,000 binary searches through the field_map. While each binary search is O(log n) where n = number of field ranges, the total cost is O(file_size * log(range_count)). This is strictly worse than pre-computing a flat per-byte lookup table (O(file_size) one-time cost, then O(1) per position).
- Fix: Since `classify_source` already produces a contiguous, sorted field_map, build a position-to-field iterator that advances linearly through the ranges as `pos` increases (since `pos` is iterated sequentially from 0 to len-2). This converts O(file_size * log(range_count)) into O(file_size):
```rust
let mut range_idx = 0;
for (pos, window) in bytes.windows(2).enumerate() {
    // Advance to the range containing `pos`.
    while range_idx < field_map.len() && field_map[range_idx].0.end <= pos {
        range_idx += 1;
    }
    let field_id = if range_idx < field_map.len() && field_map[range_idx].0.contains(&pos) {
        field_map[range_idx].1.discriminant()
    } else {
        SearchField::Other.discriminant()
    };
    // ... rest of loop
}
```

---

### MEDIUM

**Multiple HashMap allocations per search without capacity hints** - `crates/rskim-search/src/index/reader.rs:273-279`
**Confidence**: 82%
- Problem: The `search()` function creates 4 HashMaps (doc_scores, doc_field_tfs, doc_positions, doc_meta_cache) plus an additional `tf_per_doc` HashMap per ngram iteration (line 286) — all with default capacity. For queries matching many documents, these maps will reallocate multiple times as they grow. The inner `tf_per_doc` is particularly wasteful since it is allocated fresh per ngram iteration.
- Fix: Pre-allocate with `HashMap::with_capacity` based on `self.header.file_count` (capped at a reasonable maximum like 1024) for the outer maps. For `tf_per_doc`, consider reusing a single map across iterations by clearing it:
```rust
let cap = (self.header.file_count as usize).min(1024);
let mut doc_scores: HashMap<u32, f64> = HashMap::with_capacity(cap);
let mut doc_field_tfs: HashMap<u32, [f32; FIELD_COUNT]> = HashMap::with_capacity(cap);
// ...

// Reuse across iterations:
let mut tf_per_doc: HashMap<u32, [f32; FIELD_COUNT]> = HashMap::with_capacity(cap);
for (ngram, _weight) in &ngrams {
    tf_per_doc.clear();
    // ... populate tf_per_doc ...
}
```

---

**`sort_by` instead of `sort_unstable_by` for final result ranking** - `crates/rskim-search/src/index/reader.rs:336`
**Confidence**: 83%
- Problem: The scored results are sorted using `sort_by` which is stable sort (requires extra allocation for merge sort). The previous code used `sort_unstable_by`. Since the tie-breaking already uses FileId for determinism, stability is unnecessary and `sort_unstable_by` would be faster (no allocation, better cache behavior for the common case).
- Fix:
```rust
scored.sort_unstable_by(|a, b| {
    b.1.partial_cmp(&a.1)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.0.cmp(&b.0))
});
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Posting list per-position storage inflates index size** - `crates/rskim-search/src/index/builder.rs:160-168`
**Confidence**: 80%
- Problem: Every 2-byte window generates a 9-byte posting entry (doc_id:4 + field_id:1 + position:4). For a 100KB file, this produces ~100,000 posting entries = 900KB of posting data per file. With 1000 files, the posting file approaches 900MB. The field_id byte was always 7 (Other) in the previous format; now it stores meaningful per-position field IDs, making each posting 12.5% larger than if field were stored at the document level. This is a design trade-off (position-level field precision vs. document-level), but the index size growth should be documented.
- Fix: This is acceptable for correctness (field varies by position), but consider whether the index could use a more compact encoding in a future version (e.g., delta-encoding positions, or run-length encoding field_id runs within a doc). No blocking fix needed for v1.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **f64 conversions in hot scoring loop** - `crates/rskim-search/src/lexical/scoring.rs:46-78` (Confidence: 65%) -- The `bm25f_score` function converts f32 to f64 eight times per field in the inner loop. While `f64::from(f32)` is a single widening instruction, the function could precompute `k1` as f64 outside the loop (already done) and potentially vectorize the field loop with SIMD in future if profiling shows this as a bottleneck.

- **`doc_positions` collects all match positions unconditionally** - `crates/rskim-search/src/index/reader.rs:293-296` (Confidence: 70%) -- Match positions are collected for ALL documents (even those below the top-20 cutoff). For queries with high fan-out (common bigrams), this allocates position vectors for thousands of documents when only 20 will be returned. A two-pass approach (score first, then collect positions only for top-k) would reduce memory pressure.

- **Inner `for i in 0..FIELD_COUNT` loop in search accumulation** - `crates/rskim-search/src/index/reader.rs:319-321` (Confidence: 62%) -- The per-field TF accumulation loop runs for all 8 fields even when most are zero. For sparse field distributions (most positions map to 1-2 fields), this does ~6 unnecessary additions per document per ngram. Profile before optimizing.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The two HIGH-severity findings represent meaningful performance regressions in the indexing path. The `classify_source` per-byte allocation and the `resolve_field` binary-search-per-window pattern both scale poorly with file size. Since this is a search index builder that processes entire codebases, these hot paths will be exercised on thousands of files and the accumulated cost is significant. The linear-scan fix for `resolve_field` is straightforward and impactful; the classifier allocation is a design choice that could be addressed with a size guard for v1 and a range-based rewrite for v2. The `sort_by` vs `sort_unstable_by` regression is a quick fix.
