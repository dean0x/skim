# Performance Review Report

**Branch**: feat/178-two-file-mmap-index -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**Redundant HashMap allocation in search hot path: per-bigram tf_per_doc is rebuilt from scratch** - `crates/rskim-search/src/index/reader.rs:216-222`
**Confidence**: 90%
- Problem: Inside the `for (ngram, _weight) in &ngrams` loop (line 213), a fresh `HashMap<u32, u32>` (`tf_per_doc`) is created on every bigram iteration just to count term frequencies per document. The postings are iterated once to fill `tf_per_doc`, then iterated again (line 232) to fill `doc_positions`. This means every posting entry is visited twice per bigram, and a temporary HashMap is allocated and dropped on every loop iteration. For a query with N bigrams and M total postings, this creates N HashMap allocations and 2M posting reads.
- Fix: Merge the two passes into a single pass. Count tf and collect positions in one loop over `postings`:
```rust
for (ngram, _weight) in &ngrams {
    let postings = self.lookup_postings(ngram.key())?;
    let idf = idf_for_key(ngram.key());

    // Single-pass: count tf per doc AND collect positions
    let mut tf_counts: HashMap<u32, u32> = HashMap::new();
    for p in &postings {
        *tf_counts.entry(p.doc_id).or_default() += 1;
        let pos = p.position as usize;
        doc_positions.entry(p.doc_id).or_default().push(pos..pos + 2);
    }

    for (doc_id, tf) in tf_counts {
        let doc_len = if doc_id < self.header.file_count {
            self.file_meta_at(doc_id)?.doc_length
        } else {
            0
        };
        let contribution = bm25_score(tf as f32, idf, doc_len, self.header.avg_doc_length);
        *doc_tf.entry(doc_id).or_default() += contribution;
    }
}
```

**Repeated `file_meta_at` mmap decode per document per bigram** - `crates/rskim-search/src/index/reader.rs:224-228`
**Confidence**: 92%
- Problem: `file_meta_at(doc_id)` is called inside the inner loop over `tf_per_doc` entries, which itself is inside the outer loop over bigrams. If a document matches 10 bigrams, `file_meta_at` will decode the same 5-byte `FileMetaEntry` from the mmap 10 separate times. Each call computes an offset and invokes `decode_file_meta` (which includes a bounds check and byte array conversion). While individually cheap, this is O(bigrams x unique_docs) redundant work that compounds on large corpora.
- Fix: Cache doc_length lookups. Since `doc_len` is immutable for a given `doc_id`, look it up once and store it:
```rust
// At the top of search(), alongside doc_tf:
let mut doc_len_cache: HashMap<u32, u32> = HashMap::new();

// Inside the tf scoring loop:
let doc_len = match doc_len_cache.entry(doc_id) {
    std::collections::hash_map::Entry::Occupied(e) => *e.get(),
    std::collections::hash_map::Entry::Vacant(e) => {
        let len = if doc_id < self.header.file_count {
            self.file_meta_at(doc_id)?.doc_length
        } else {
            0
        };
        *e.insert(len)
    }
};
```

**Language filter applied after scoring all documents** - `crates/rskim-search/src/index/reader.rs:252-261`
**Confidence**: 88%
- Problem: When `query.lang` is set, all documents are scored with BM25 first (lines 213-238), sorted (line 245), and only then filtered by language (lines 254-261). For a corpus of 100k files where only 10k match the language filter, 90% of scoring work is wasted. BM25 scoring involves a `file_meta_at` lookup, floating-point arithmetic with `f64::from` conversions, and the `bm25_score` function with log/division operations. This is O(total_matching_docs) work that could be O(matching_lang_docs).
- Fix: Apply the language filter during the scoring loop (where tf_per_doc is iterated), before computing BM25:
```rust
let lang_filter: Option<u8> = query.lang.map(super::format::lang_to_id);

// Inside the tf scoring loop:
for (doc_id, tf) in tf_counts {
    // Early language filter - skip scoring entirely
    if let Some(required_lang) = lang_filter {
        if doc_id < self.header.file_count {
            let meta = self.file_meta_at(doc_id)?;
            if meta.lang_id != required_lang {
                continue;
            }
        }
    }
    // ... proceed with BM25 scoring
}
```
Note: This moves the `lang_filter` binding before the bigram loop and filters before scoring, avoiding both BM25 computation and HashMap insertions for non-matching documents.

### MEDIUM

**No pre-allocation hint for `postings_buf` in builder** - `crates/rskim-search/src/index/builder.rs:190`
**Confidence**: 85%
- Problem: `postings_buf: Vec<u8>` is created with `Vec::new()` (zero capacity). As posting entries are appended via `extend_from_slice` for potentially millions of entries (9 bytes each), the Vec will grow through multiple reallocations (each doubling). For a corpus of 100k files producing, say, 50M postings, this could trigger ~25 reallocations with large memcpy operations on the final doublings (hundreds of MB).
- Fix: Estimate the total posting bytes upfront:
```rust
let total_postings: usize = self.postings.values().map(|v| v.len()).sum();
let mut postings_buf: Vec<u8> = Vec::with_capacity(total_postings * POSTING_ENTRY_SIZE);
```

**Decode overhead in `lookup_postings`: decoding every posting entry individually** - `crates/rskim-search/src/index/reader.rs:170-178`
**Confidence**: 82%
- Problem: Each posting entry (9 bytes) is individually decoded through `decode_posting`, which performs a length check, `read_array` calls (each with a `try_into` and error path), and a `SearchField::from_discriminant` validation. For a posting list with 10,000 entries, that is 10,000 bounds checks + 30,000 `read_array` calls + 10,000 discriminant validations. The bounds check on line 162 already guarantees the entire slice is valid, so per-entry length checks are redundant.
- Fix: Consider an unsafe zero-copy approach for the hot path (with the overall bounds check as the safety gate), or at minimum, use a simplified decode that skips the per-entry length check since the slice bounds were already validated:
```rust
// After validating the overall slice bounds (line 162),
// we know every entry fits. A tighter inner loop:
fn decode_posting_unchecked(data: &[u8]) -> PostingEntry {
    debug_assert!(data.len() >= POSTING_ENTRY_SIZE);
    PostingEntry {
        doc_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        field_id: data[4],
        position: u32::from_le_bytes([data[5], data[6], data[7], data[8]]),
    }
}
```
This eliminates `read_array`'s `try_into` + error wrapping overhead per entry. The discriminant validation can be deferred to a single pass or skipped entirely when the index was just built (trusted source).

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`as` casts for u64/f32 conversions in builder::build without overflow protection** - `crates/rskim-search/src/index/builder.rs:177,195,238,240`
**Confidence**: 80%
- Problem: Lines 177 (`self.total_doc_length as f32 / self.file_count as f32`) and 238 (`entries.len() as u32`) use `as` casts. While the `as` cast for `entries.len() as u32` could silently truncate if there are more than 4 billion distinct bigrams (unlikely but theoretically possible with adversarial input), the `total_doc_length as f32` cast on line 177 loses precision for large total lengths. With u64 values above 2^24, the f32 mantissa cannot represent the value exactly, which could produce inaccurate `avg_doc_length` values used in every BM25 score computation.
- Fix: Use f64 for the avg_doc_length computation, then convert to f32 at the end:
```rust
let avg_doc_length = if self.file_count == 0 {
    0.0f32
} else {
    (self.total_doc_length as f64 / self.file_count as f64) as f32
};
```
And add a checked conversion for `entries.len()`:
```rust
let ngram_count = u32::try_from(entries.len()).map_err(|_| {
    SearchError::IndexCorrupted(format!("ngram count {} overflows u32", entries.len()))
})?;
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Consider `SmallVec` for posting lists in builder** - `crates/rskim-search/src/index/builder.rs:149` (Confidence: 65%) -- Many bigrams may have very short posting lists (1-3 entries). Using `SmallVec<[PostingEntry; 4]>` instead of `Vec<PostingEntry>` for `self.postings` values could eliminate heap allocations for the majority of posting lists, reducing allocator pressure during indexing.

- **Binary search in `lookup_ngram` could use unsafe unchecked indexing** - `crates/rskim-search/src/index/format.rs:310-311` (Confidence: 70%) -- The binary search loop already guarantees `mid * SKIDX_ENTRY_SIZE < entries_data.len()` by construction (`mid < n` and `n * SKIDX_ENTRY_SIZE == entries_data.len()`), but `read_array` still performs a bounds-checked slice conversion. For a lookup table with 16k entries, the binary search touches ~14 entries. The overhead is minimal per query but could matter at very high QPS.

- **`sorted_keys` collection in builder could be avoided** - `crates/rskim-search/src/index/builder.rs:186-187` (Confidence: 62%) -- `self.postings.keys().copied().collect()` followed by `sort_unstable()` creates a temporary Vec. Using a `BTreeMap` instead of `HashMap` for `self.postings` would maintain sorted order during insertion at the cost of O(log n) inserts vs O(1) amortized. For the build path (which runs once), this is unlikely to matter, but it would eliminate the separate sort step.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 3 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The overall architecture is strong: mmap-based I/O avoids syscall overhead on reads, binary search over sorted entries gives O(log n) lookup, and the format is compact with fixed-size entries. The main performance concerns are in the search hot path: redundant iteration over postings (two passes where one suffices), repeated file_meta decoding for the same document across bigrams, and late application of the language filter causing wasted BM25 scoring work. The builder path has a minor pre-allocation miss but is not latency-critical since it runs once during index construction. Addressing the three HIGH items in the search path would meaningfully improve query latency, especially on larger corpora where the wasted work scales linearly.
