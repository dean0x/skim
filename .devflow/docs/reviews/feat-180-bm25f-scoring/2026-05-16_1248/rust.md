# Rust Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### HIGH

**BM25FConfig::validate() is never called on user-supplied config** - `reader.rs:265`, `reader.rs:153`
**Confidence**: 92%
- Problem: `open_with_config()` accepts a `BM25FConfig` and `SearchQuery::bm25f_config` allows per-query overrides, but neither path calls `validate()`. A caller can supply negative `k1`, negative boosts, or `b > 1.0` without triggering an error. Negative `k1` would cause a division by a negative denominator in the BM25F formula (`tf_weighted / (tf_weighted + k1)`), potentially producing negative or non-finite scores that corrupt ranking.
- Fix: Call `config.validate()?` in `open_with_config()` and at the start of `search()` when `query.bm25f_config` is `Some`:
```rust
// In open_with_config:
pub fn open_with_config(dir: &std::path::Path, config: BM25FConfig) -> Result<Self> {
    config.validate()?;
    let mut reader = Self::open(dir)?;
    reader.bm25f_config = config;
    Ok(reader)
}

// In search, after resolving scoring_config:
if let Some(ref cfg) = query.bm25f_config {
    cfg.validate()?;
}
```

**Per-byte allocation in classifier scales O(n) memory for large files** - `classifier.rs:116`
**Confidence**: 82%
- Problem: `classify_source` allocates a `Vec<SearchField>` of size `source.len()` bytes. For a 100 MB file (`u32::MAX` is the stated limit), this is a 100 MB allocation of `SearchField` enum values (1 byte each due to `#[repr(u8)]`). While the CLAUDE.md notes large files should be rejected, the function itself has no size guard. Combined with tree-sitter parsing on the same input, memory pressure could be significant for legitimate large source files (e.g., generated code at 5-10 MB).
- Fix: Consider adding an upper-bound check (e.g., 10 MB) with a clear error, or use a sparse approach for very large files. Alternatively, document this as an intentional trade-off for simplicity given the existing `u32::MAX` cap in the builder:
```rust
const MAX_CLASSIFY_SIZE: usize = 10 * 1024 * 1024; // 10 MB
if source.len() > MAX_CLASSIFY_SIZE {
    return Ok(vec![(0..len, SearchField::Other)]);
}
```

### MEDIUM

**Classifier innermost-wins logic can produce surprising results** - `classifier.rs:134-141`
**Confidence**: 80%
- Problem: The comment says "Only overwrite if this field is more specific than Other" but the actual logic stamps ANY non-Other field from a parent over a more-specific child's assignment from a previous iteration. In a pre-order walk, parents are visited BEFORE children, so children correctly overwrite parents. However, consider: a `function_item` (priority 4 -> FunctionSignature) contains an `identifier` node that maps to SymbolName. The identifier overwrites the parent's FunctionSignature marking for those bytes. This may be intentional (innermost wins) but means FunctionSignature only covers bytes NOT claimed by any child node (basically nothing in practice since function bodies contain identifiers, blocks, etc.). The documented intent ("function signatures rank higher") may not match the actual behavior where most function bytes end up classified as SymbolName or Other from interior nodes.
- Fix: This is a design consideration more than a bug. If the intent is that the entire function signature span contributes to FunctionSignature scoring, the algorithm would need to only descend into specific child categories. If innermost-wins is intentional, the doc comments on `map_priority_to_field` should clarify that field assignment granularity is at the leaf/innermost node level, not at the structural node level.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`doc_positions` accumulates positions for filtered-out documents** - `reader.rs:292-296`
**Confidence**: 85%
- Problem: The position collection at line 292-296 happens before the `doc_id >= self.header.file_count` check and the language filter at line 313. This means positions are accumulated in memory for documents that will be skipped. For large posting lists with many out-of-range doc_ids or wrong-language docs, this wastes memory.
- Fix: Move position accumulation inside the per-doc scoring block (after language filter passes), or lazily collect positions only for scored documents:
```rust
for p in &postings {
    let field_idx = p.field_id as usize;
    if field_idx < FIELD_COUNT {
        tf_per_doc.entry(p.doc_id).or_insert([0.0; FIELD_COUNT])[field_idx] += 1.0;
    }
    // Defer position collection to after filtering
}
// After scoring loop, collect positions for documents that passed:
for p in &postings {
    if doc_scores.contains_key(&p.doc_id) {
        let pos = p.position as usize;
        doc_positions.entry(p.doc_id).or_default().push(pos..pos + 2);
    }
}
```

**`sort_by` instead of `sort_unstable_by` for scored results** - `reader.rs:336`
**Confidence**: 82%
- Problem: The previous code used `sort_unstable_by` which is faster (no allocation) and appropriate since the tie-breaking by FileId already provides deterministic ordering. The new code uses `sort_by` which allocates a temporary buffer. For large result sets this is a measurable performance regression.
- Fix: Switch back to `sort_unstable_by`:
```rust
scored.sort_unstable_by(|a, b| {
    b.1.partial_cmp(&a.1)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.0.cmp(&b.0))
});
```

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`unsafe` mmap with adequate SAFETY comment but no defensive validation** - `reader.rs:85-86`
**Confidence**: 80%
- Problem: The SAFETY comment correctly describes the assumption ("files are not modified after mapping") but there is no platform-specific defense. On Linux, a concurrent `truncate` of the mapped file could cause a SIGBUS. This is pre-existing and acceptable for a pre-1.0 project but worth noting for hardening later.

## Suggestions (Lower Confidence)

- **`f32` equality comparisons in scoring** - `scoring.rs:47,53` (Confidence: 65%) — Using `== 0.0` for f32 is technically fragile, though in this context the values come from array initialization or config fields that are literally set to 0.0, so it is unlikely to cause false negatives.

- **`FIELD_COUNT` duplication** - `config.rs:26`, `types.rs:98-99` (Confidence: 70%) — `FIELD_COUNT` is defined as `8` in `config.rs` and `SearchField::count()` returns `8` in `types.rs`. These could drift if a new field is added. Consider having one derive from the other (e.g., `pub const FIELD_COUNT: usize = SearchField::count()`).

- **`pub(crate) file_count` field visibility widened for tests** - `builder.rs:47` (Confidence: 62%) — The field was widened from private to `pub(crate)` solely for a test assertion. A `file_count()` accessor method would maintain better encapsulation.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Rust Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The implementation demonstrates strong Rust patterns: proper error handling with `Result` types throughout, `#[must_use]` annotations on pure functions, safe numeric conversions with checked arithmetic, well-documented invariants, and comprehensive test coverage. The BM25F formula implementation is correct with appropriate zero-guards. The primary concern is the missing validation call path for user-supplied config (HIGH), which could allow invalid parameters to produce incorrect rankings. The classifier memory scaling and sort regression are lower-priority but worth addressing.
