# Reliability Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### HIGH

**BM25FConfig::validate() is never called at trust boundaries** - `reader.rs:153`, `reader.rs:266`
**Confidence**: 90%
- Problem: `BM25FConfig` has a `validate()` method that enforces invariants (k1 >= 0, boosts >= 0, b in [0,1]), but neither `open_with_config()` nor the search path (`query.bm25f_config.as_ref()`) calls it before use. An externally-supplied config with negative `k1` or `field_b > 1.0` can produce non-finite scores without any error, violating the stated invariants. Since `BM25FConfig` is public API and `Deserialize`-derived, arbitrary values can flow in from JSON.
- Fix: Validate at the two trust boundaries:
```rust
// In open_with_config:
pub fn open_with_config(dir: &std::path::Path, config: BM25FConfig) -> Result<Self> {
    config.validate()?;
    let mut reader = Self::open(dir)?;
    reader.bm25f_config = config;
    Ok(reader)
}

// In search(), when per-query config is provided:
let scoring_config: &BM25FConfig = match query.bm25f_config.as_ref() {
    Some(cfg) => { cfg.validate()?; cfg }
    None => &self.bm25f_config,
};
```

### MEDIUM

**Per-byte Vec allocation in classify_source() lacks a size guard** - `classifier.rs:116`
**Confidence**: 82%
- Problem: `classify_source()` allocates `vec![SearchField::Other; len]` where `len = source.len()`. While the builder constrains content to `u32::MAX` bytes (~4 GB), `classify_source()` is a public API that can be called directly with arbitrarily large input. A 4 GB allocation for the per-byte array plus the source itself would consume ~8 GB. The function has no size guard — it relies solely on the caller bounding the input.
- Fix: Add an explicit upper bound or at minimum document the constraint:
```rust
pub fn classify_source(
    source: &str,
    lang: Language,
) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    if source.is_empty() {
        return Ok(Vec::new());
    }
    // Guard: reject sources that would cause excessive allocation.
    // The on-disk format limits files to u32::MAX bytes; enforce here too.
    if source.len() > u32::MAX as usize {
        return Err(crate::SearchError::InvalidQuery(
            "source too large for classification (exceeds u32::MAX bytes)".into(),
        ));
    }
    // ... rest of function
}
```

**Unbounded doc_positions growth in search loop** - `reader.rs:293-296`
**Confidence**: 80%
- Problem: During the search loop, `doc_positions` accumulates every posting entry's position without any cap. For a common bigram (e.g., "th") in a large index, the posting list may contain millions of entries. Each generates a `Range<usize>` pushed into a `Vec`. The final result only needs positions for the top `limit` documents (default 20), but positions are collected for ALL matching documents before sorting. This is not a correctness bug but represents unbounded memory growth proportional to index size.
- Fix: Consider deferring position collection to a second pass over only the top-scoring documents, or cap the per-document position vector:
```rust
// Option A: Collect positions only for top-scoring docs in a second pass
// Option B: Cap positions per document
let positions = doc_positions.entry(p.doc_id).or_default();
if positions.len() < MAX_POSITIONS_PER_DOC {
    positions.push(pos..pos + 2);
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Scoring function does not validate NaN propagation from deserialized header values** - `reader.rs:323-329`
**Confidence**: 82%
- Problem: The `avg_field_lengths` array is decoded from disk as raw `f32` little-endian bytes. A corrupted or maliciously crafted index file could contain NaN or infinity values in these fields. While the CRC32 checksum guards against accidental corruption, it does not guard against intentional manipulation (an attacker can compute a valid CRC for crafted data). The scoring function has a guard for `avg_field_lengths[i] > 0.0` (which NaN fails), but NaN fails the comparison by returning false, causing the fallback to `1.0` -- which is actually safe behavior. However, `field_lengths` from `FileMetaEntry` are `u32` (cannot be NaN), so this is well-defended.
- Fix: No action required -- the existing zero-guard (`if avg_field_lengths[i] > 0.0`) handles NaN correctly by falling through to the `1.0` fallback. The concern is informational.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Tree-sitter cursor walk terminates only by structural exhaustion** - `classifier.rs:121-159` (Confidence: 65%) -- The outer `loop` in the tree-sitter walk has no explicit iteration counter. Termination depends entirely on tree-sitter's cursor API always eventually returning `false` from `goto_parent()`. While tree-sitter is well-tested and trees are finite DAGs, a corrupted tree-sitter grammar could theoretically produce a cycle. Adding a max-iterations guard (e.g., `source.len() * 4` as an upper bound on node count) would provide defense-in-depth.

- **HashMap allocations inside per-ngram loop** - `reader.rs:286` (Confidence: 62%) -- `tf_per_doc` is allocated fresh each iteration of the ngram loop. For queries with many bigrams, this creates repeated allocation/deallocation churn. Pre-allocating and clearing between iterations would reduce allocator pressure.

- **f32 TF accumulator precision loss** - `reader.rs:290` (Confidence: 60%) -- Per-field term frequencies are accumulated as `f32`. For documents with very high term counts (millions of postings in a single field), `f32` loses precision above 2^24 (16,777,216). In practice this is unlikely for code search, but switching to `f64` accumulators would eliminate the theoretical precision cliff.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The BM25F scoring engine demonstrates good defensive coding: zero-guards prevent division-by-zero, checked arithmetic prevents overflow in the builder, CRC32 detects corruption, and the scoring formula uses f64 for precision. The primary reliability gap is the missing validation of user-supplied `BM25FConfig` at trust boundaries -- the `validate()` method exists but is never invoked in production paths. The per-byte allocation in `classify_source()` and unbounded position collection in search are secondary concerns that matter at scale.
