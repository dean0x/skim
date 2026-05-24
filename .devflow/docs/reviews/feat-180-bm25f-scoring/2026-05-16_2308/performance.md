# Performance Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### HIGH

**Per-byte Vec allocation in classify_source is O(n) memory** - `classifier.rs:131`
**Confidence**: 85%
- Problem: `classify_source` allocates a `Vec<SearchField>` of size `source.len()` -- one element per byte. Although `SearchField` is `#[repr(u8)]` (1 byte), this means a 10 MB source file creates a 10 MB heap allocation, a 50 MB file creates 50 MB, and the ceiling is 100 MiB (`MAX_SOURCE_BYTES`). This allocation occurs per file during indexing. For large codebases with many sizeable files being indexed concurrently or sequentially, this creates significant transient memory pressure and GC/allocator load.
- Impact: During indexing of large codebases, peak RSS will spike proportionally to the largest file being classified. The 100 MiB guard prevents catastrophic OOM but still allows substantial allocation per file.
- Fix: Consider a two-pass approach: (1) walk the AST and collect `(byte_range, SearchField)` tuples directly from the tree-sitter nodes (most ASTs have far fewer nodes than bytes), then (2) merge/sort into the contiguous range list without materializing a per-byte array. This would reduce memory from O(source_bytes) to O(AST_nodes), which is typically 10-100x smaller.

```rust
// Alternative: collect per-node ranges directly, then merge overlapping ones
// (pseudocode sketch)
let mut ranges: Vec<(Range<usize>, SearchField)> = Vec::new();
// ... walk AST, push (node.byte_range(), field) for non-Other fields ...
// Sort by start, then merge overlapping ranges (innermost-wins via reverse iteration)
// Fill gaps with SearchField::Other
```

### MEDIUM

**Six HashMap allocations per search query in the hot path** - `reader.rs:281-296`
**Confidence**: 83%
- Problem: Each `search()` call allocates six `HashMap` instances (`doc_scores`, `doc_field_tfs`, `doc_positions`, `doc_meta_cache`, plus two per-ngram: `tf_per_doc`, `pos_per_doc`). The two per-ngram maps are allocated and dropped once per query bigram. For short queries this is 1-2 iterations; for longer queries (e.g., multi-word identifier search with 10+ bigrams) this creates 20+ short-lived HashMap allocations.
- Impact: HashMap allocation/deallocation cost and rehashing. For search-heavy workloads (interactive "search as you type"), this overhead adds up. The per-ngram maps (`tf_per_doc`, `pos_per_doc`) are the main concern since they're re-created each loop iteration.
- Fix: Move `tf_per_doc` and `pos_per_doc` outside the ngram loop and `.clear()` them each iteration to reuse the allocation.

```rust
// Reuse allocations across ngram iterations
let mut tf_per_doc: HashMap<u32, [f32; FIELD_COUNT]> = HashMap::new();
let mut pos_per_doc: HashMap<u32, Vec<std::ops::Range<usize>>> = HashMap::new();

for (ngram, _weight) in &ngrams {
    tf_per_doc.clear();
    pos_per_doc.clear();
    // ... rest of loop body unchanged ...
}
```

**Index file size growth: FileMetaEntry 5 -> 37 bytes (7.4x)** - `format.rs:55`
**Confidence**: 80%
- Problem: The `FileMetaEntry` on-disk size grew from 5 to 37 bytes due to the new `field_lengths: [u32; 8]` array. For an index of 100K files, the metadata section grows from ~500 KB to ~3.7 MB. For 1M files, it goes from ~5 MB to ~37 MB. The index file is mmap'd so this directly affects page-in costs and disk I/O during index loading.
- Impact: Larger `.skidx` files mean more disk I/O on first read, more memory mapped pages, and slower CRC32 checksumming during `open()`. For typical project sizes (< 10K files) the impact is negligible. For monorepo-scale indexes (100K+ files) this could meaningfully slow cold-start index opening.
- Fix: This is an inherent cost of BM25F and is well-justified by the feature. The format v2 clean break is the right approach. No code change needed, but document the size growth in the format spec for capacity planning. Consider adding a `--stats` output that shows index file sizes to help users understand storage costs.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**postings_buf Vec grows without pre-sizing** - `builder.rs:275`
**Confidence**: 82%
- Problem: `postings_buf` is initialized as `Vec::new()` with no capacity hint, then repeatedly extended via `extend_from_slice` during serialization. This causes multiple re-allocations as the buffer grows. The posting entries total is known: `sum of all posting list lengths * POSTING_ENTRY_SIZE`. While this was pre-existing, the overall builder performance is now more important given the additional per-file overhead from field classification.
- Impact: For indexes with millions of postings, this causes O(log n) reallocations during the build phase. Each reallocation copies the entire buffer.
- Fix: Pre-compute the total postings byte size and use `Vec::with_capacity`.

```rust
let total_postings_bytes: usize = self.postings.values().map(|v| v.len() * POSTING_ENTRY_SIZE).sum();
let mut postings_buf: Vec<u8> = Vec::with_capacity(total_postings_bytes);
```

## Pre-existing Issues (Not Blocking)

### MEDIUM

**lookup_postings allocates a Vec per ngram key** - `reader.rs:194`
**Confidence**: 82%
- Problem: `lookup_postings` allocates a new `Vec<PostingEntry>` for every ngram lookup and decodes each posting entry individually via `decode_posting`. This is called once per query bigram. For a query with 10 bigrams, each returning 1000 postings, that is 10 Vec allocations and 10,000 individual decode calls.
- Impact: This is the dominant allocation in the search hot path. A zero-copy approach reading directly from the mmap'd data (interpreting bytes in-place) would eliminate these allocations entirely.
- Fix: Future optimization (not blocking this PR): return an iterator over mmap'd posting entries rather than collecting into a Vec.

## Suggestions (Lower Confidence)

- **map_priority_to_field performs two match arms per node** - `classifier.rs:43-78` (Confidence: 65%) -- The function first matches `kind` against ~20 string patterns for comments/strings/identifiers, then falls through to match `priority`. For typical ASTs with thousands of nodes, the double-match cost is measurable but likely dominated by the per-byte stamping loop. Could be unified into a single match, but tree-sitter string comparison is already branch-predicted well.

- **node_kind_priority calls node_kind_info which returns an unused (&str, u8) tuple** - `rskim-core/src/lib.rs:61` (Confidence: 70%) -- `node_kind_priority` calls `transform::utils::node_kind_info(kind).1`, discarding the static string. The compiler likely optimizes this away, but an explicit `score_node_kind` wrapper already exists in utils.rs and could be exposed instead to make the intent clearer.

- **doc_field_tfs HashMap persists across all ngrams but is only used for dominant_field at the end** - `reader.rs:283` (Confidence: 62%) -- This accumulator HashMap grows across all ngrams but is only consumed once at the end for `dominant_field()`. If dominant_field is not needed (or could be deferred), this allocation and per-ngram accumulation could be skipped entirely, but the feature design requires it.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Performance Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The BM25F scoring implementation is well-designed with good algorithmic choices (O(n+m) linear scan instead of O(n log m) binary search in the builder, metadata caching in the reader, early-exit guards in bm25f_score). The scoring formula itself is clean and uses f64 arithmetic appropriately. The main performance concerns are: (1) the per-byte allocation in classify_source which could be improved to per-node, and (2) per-query HashMap churn that could be reduced by reusing allocations across ngram iterations. Neither blocks merge, but the HashMap reuse fix is a straightforward improvement that should be addressed soon.
