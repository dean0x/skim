# Rust Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**NaN/Infinity values accepted without validation in decoded `avg_field_lengths` and `avg_doc_length`** - `crates/rskim-search/src/index/format.rs:228-233`
**Confidence**: 85%
- Problem: `decode_header()` reads `avg_field_lengths` (and pre-existing `avg_doc_length`) as raw `f32::from_le_bytes()` values from an mmap'd file. A corrupted or maliciously crafted index file could contain NaN or Infinity in these fields. While the CRC32 checksum protects against accidental corruption, a deliberately crafted file with matching CRC could inject NaN values that propagate through `bm25f_score()`, producing NaN scores and corrupting all search results silently. The `BM25FConfig::validate()` method correctly validates user-supplied parameters, but the corpus-derived values from the index file are not validated.
- Fix: Add a post-decode validation pass in `decode_header()`:
```rust
// After decoding avg_field_lengths
for (i, &v) in avg_field_lengths.iter().enumerate() {
    if !v.is_finite() || v < 0.0 {
        return Err(SearchError::IndexCorrupted(format!(
            "header: avg_field_lengths[{i}] is not a valid non-negative finite value: {v}"
        )));
    }
}
let avg_doc_length = f32::from_le_bytes(read_array(data, 22, "header: avg_doc_length")?);
if !avg_doc_length.is_finite() || avg_doc_length < 0.0 {
    return Err(SearchError::IndexCorrupted(
        format!("header: avg_doc_length is not a valid non-negative finite value: {avg_doc_length}")
    ));
}
```

### MEDIUM

**`compute_field_lengths` uses `unwrap_or(u32::MAX)` on saturation instead of returning `Result`** - `crates/rskim-search/src/index/builder.rs:207-208`
**Confidence**: 82%
- Problem: When `source_len` exceeds `u32::MAX` in the empty `field_map` branch, `unwrap_or(u32::MAX)` silently saturates the value. The caller (`add_file_classified`) already validates `content.len()` fits `u32` via `u32::try_from` at line 133, so this branch is technically unreachable for well-formed callers. However, `compute_field_lengths` is a standalone function that does not enforce this precondition itself. Similarly, line 212 saturates range lengths with `unwrap_or(u32::MAX)` and `saturating_add`. While these defensive guards prevent panics, they hide invariant violations: if a range exceeds `u32::MAX`, something is fundamentally wrong and the index will contain inaccurate field lengths.
- Fix: Since the caller already validates `content.len() <= u32::MAX`, add a `debug_assert!` at the top of `compute_field_lengths` to document the precondition:
```rust
fn compute_field_lengths(
    source_len: usize,
    field_map: &[(Range<usize>, SearchField)],
) -> [u32; FIELD_COUNT] {
    debug_assert!(source_len <= u32::MAX as usize, "source_len must fit u32");
    // ... rest unchanged
}
```

**Doc comment for `classify_source` merges two items without a blank separator** - `crates/rskim-search/src/lexical/classifier.rs:80-99`
**Confidence**: 85%
- Problem: The doc comment for `classify_source()` at line 80 runs directly into the doc for `MAX_SOURCE_BYTES` at line 93 without a blank line or `///` separator. The `pub const MAX_SOURCE_BYTES` declaration at line 99 appears to have its own doc comment starting at line 93, but the `///` block that begins at line 80 also continues through it. Rustdoc will attach lines 93-98 to `classify_source`'s documentation, not to `MAX_SOURCE_BYTES`. The `MAX_SOURCE_BYTES` constant ends up with no documentation in the generated rustdoc.
- Fix: Add a blank `///` line before `/// Maximum source size` to terminate the `classify_source` doc, or move the `MAX_SOURCE_BYTES` declaration and its doc comment above `classify_source`:
```rust
/// ...for supported languages).
///
/// # Size limit
///
/// Sources exceeding [`MAX_SOURCE_BYTES`] are rejected with
/// [`SearchError::FileTooLarge`].
pub fn classify_source(
```
And separately:
```rust
/// Maximum source size (in bytes) accepted by [`classify_source`].
///
/// The classifier allocates a per-byte `Vec<SearchField>`, so accepting
/// unbounded input would allow a caller to trigger proportional memory
/// allocation. 100 MiB is generous for any real source file while keeping
/// peak RSS bounded.
pub const MAX_SOURCE_BYTES: usize = 100 * 1024 * 1024; // 100 MiB
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Classifier pre-order walk stamps parent ranges then children overwrite, but `Other` skip causes non-innermost-wins for `Other` children inside non-`Other` parents** - `crates/rskim-search/src/lexical/classifier.rs:149-156`
**Confidence**: 80%
- Problem: The classifier walks tree-sitter nodes in pre-order and uses the rule "only overwrite if field != Other" (line 152). This means if a parent node maps to `FunctionSignature` and a child node (e.g., a block/body node) maps to `Other`, the child's bytes remain stamped as `FunctionSignature`. This is intentional per the comment, but it creates a semantic issue: the body of a function (which is `Other` / `FunctionBody`) will be classified the same as its signature when the body-level node kind maps to `Other` through `map_priority_to_field`. Priority 2 (class/module containers) maps to `FunctionBody` but priority 1 (everything else, including block/body nodes like `block` in Rust) maps to `Other` -- so actual function bodies get classified as `FunctionSignature` because the parent function_item has priority 4. This inflates `FunctionSignature` field lengths, reducing the precision of BM25F scoring for large function bodies.
- Fix: Consider adding body-specific node kinds (e.g., `block`, `compound_statement`, `statement_block`) to `map_priority_to_field` so they map to `SearchField::FunctionBody`:
```rust
"block" | "compound_statement" | "statement_block" | "function_body" => {
    return SearchField::FunctionBody;
}
```
This would ensure function bodies are classified separately from signatures, improving scoring precision.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`file_count` field visibility widened to `pub(crate)`** - `crates/rskim-search/src/index/builder.rs:47` (Confidence: 65%) -- The field was previously private and is now `pub(crate)`. The diff does not show any external reads of `builder.file_count` outside the builder itself. If this was for test access, consider adding a `pub(crate) fn file_count(&self) -> u32` accessor instead of exposing the field directly.

- **Positions accumulated in per-ngram HashMap then transferred, doubling allocation** - `crates/rskim-search/src/index/reader.rs:296,339-341` (Confidence: 65%) -- Each ngram iteration creates a fresh `pos_per_doc` HashMap that is then `.remove()`d and `.extend()`d into `doc_positions`. For queries with many ngrams, this creates and discards many small Vec allocations. Consider accumulating positions directly into `doc_positions` after the language filter check, reducing temporary allocations.

- **`dominant_field` initialises `best_tf` to `0.0` meaning all-zero TFs fall through to `Other`** - `crates/rskim-search/src/lexical/scoring.rs:98` (Confidence: 70%) -- The doc says "lowest discriminant wins" on ties including zero, but with `best_tf = 0.0` and the `tf > best_tf` strict comparison, the all-zero case returns `Other` (discriminant 7, the initialised fallback) rather than `TypeDefinition` (discriminant 0). This is tested and documented behaviour, but the doc comment at line 91-92 ("including ties at zero") is slightly misleading since zero TFs do not actually tie-break by discriminant -- they all lose the `> 0.0` comparison. The comment could be clarified.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The BM25F implementation follows Rust idioms well: proper `Result` propagation, `#[must_use]` annotations on scoring functions, `f64` intermediates to avoid precision loss, compile-time assertions to keep `FIELD_COUNT` in sync, and thorough edge-case testing (NaN guards in scoring, division-by-zero guards, determinism tests). The format v2 codec is clean with symmetric encode/decode and CRC32 integrity checking.

The one condition for approval is addressing the HIGH issue: **validate `avg_field_lengths` values decoded from the index file** against NaN/Infinity/negative values. This is a trust boundary that accepts data from disk (potentially corrupted or crafted) and feeds it directly into floating-point arithmetic in the scoring hot path. The existing CRC32 check provides accidental-corruption protection but not malicious-input protection.

The MEDIUM issues (doc comment separation, debug_assert for `compute_field_lengths` precondition, and function body classification) are worth addressing but should not block merge.
