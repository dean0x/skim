# Complexity Review Report

**Branch**: feat/178-two-file-mmap-index -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**`build()` function is 92 lines with 7 sequential serialisation phases** - `builder.rs:170-261`
**Confidence**: 85%
- Problem: The `build()` method spans lines 170-261 (92 lines including doc comments, ~88 lines of logic). It performs 7 distinct serialisation phases inline: avg_doc_length computation, posting list sorting, key sorting, posting serialisation, metadata serialisation, entry serialisation, CRC computation, header assembly, buffer assembly, and two atomic writes. While each phase is simple, the function exceeds the 50-line warning threshold and requires a reader to hold the full serialisation pipeline in working memory.
- Fix: Extract 2-3 helper methods to reduce `build()` to orchestration-level logic:
```rust
fn serialize_postings(&self, sorted_keys: &[u16]) -> Result<(Vec<u8>, Vec<SkidxEntry>)> { ... }
fn serialize_skidx(entries: &[SkidxEntry], file_meta: &[FileMetaEntry], header: &SkidxHeader) -> Vec<u8> { ... }
```
This keeps the commit-point ordering visible in `build()` while each helper stays under 30 lines.

**`search()` function is 85 lines with 3 nested loops** - `reader.rs:199-284`
**Confidence**: 87%
- Problem: The `search()` method spans lines 199-284 (85 lines of logic). It contains three sequentially-dependent loops: (1) per-ngram posting retrieval with nested TF accumulation and position collection, (2) a sort step, and (3) a result-assembly loop with inline language filtering and offset/limit pagination. The first loop at lines 213-239 has 3 levels of nesting (for-ngram -> for-doc -> if-branch), which is at the warning threshold. The interleaving of scoring, filtering, and pagination in a single method makes the algorithm harder to follow than necessary.
- Fix: Extract the scoring accumulation loop into a helper:
```rust
fn accumulate_scores(
    &self,
    ngrams: &[(Ngram, f32)],
) -> Result<(HashMap<u32, f64>, HashMap<u32, Vec<Range<usize>>>)> { ... }
```
And extract the filter+paginate logic into a second helper. This keeps `search()` as a readable 4-step pipeline: extract ngrams, accumulate scores, filter/sort, paginate.

### MEDIUM

**Repeated `entries_start`/`entries_end` offset computation across reader methods** - `reader.rs:139,151-152`
**Confidence**: 82%
- Problem: The byte-offset computation `SKIDX_HEADER_SIZE + (self.header.ngram_count as usize) * SKIDX_ENTRY_SIZE` appears in both `file_meta_at()` (line 139) and `lookup_postings()` (lines 151-152), using slightly different variable names (`entries_end` vs separate `entries_start`/`entries_end`). This is a minor maintainability concern -- if the layout formula changes, both sites must be updated in lockstep.
- Fix: Add a private helper method:
```rust
fn entries_byte_range(&self) -> std::ops::Range<usize> {
    let start = SKIDX_HEADER_SIZE;
    let end = start + (self.header.ngram_count as usize) * SKIDX_ENTRY_SIZE;
    start..end
}
```

**`add_file()` has 4 early-return validation branches before core logic** - `builder.rs:106-159`
**Confidence**: 80%
- Problem: The `add_file()` method (53 lines) has 4 sequential validation checks (duplicate ID, sequential ID, content length overflow) before reaching the core bigram scanning loop. The function is at the 50-line warning threshold. The validation-then-mutation structure is correct, but the method handles two distinct responsibilities: input validation and bigram extraction.
- Fix: This is borderline. The validation guards use early returns correctly and the overall flow is linear. Consider extracting the bigram scanning loop (lines 144-154) into a private `extract_bigrams()` method only if the function grows further. No immediate action required.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Repeated `super::format::POSTING_ENTRY_SIZE` path in `lookup_postings`** - `reader.rs:170,173,175` (Confidence: 65%) -- The fully-qualified path `super::format::POSTING_ENTRY_SIZE` appears 3 times in a single function. A local alias (`let entry_size = super::format::POSTING_ENTRY_SIZE;`) or a `use` import would reduce visual noise.

- **`lang_to_id`/`lang_from_id` match arms are manually synchronised** - `lang_map.rs:22-41,51-70` (Confidence: 70%) -- The two match blocks must stay in lockstep. If a new Language variant is added to `rskim_core` but only one match block is updated, the compiler will catch the `lang_to_id` side (exhaustive match) but `lang_from_id` will silently return `None` for the new variant. The test `test_lang_mapping_roundtrip` mitigates this, but a macro or const array approach would eliminate the risk.

- **Magic number 20 as default limit** - `reader.rs:248` (Confidence: 62%) -- `query.limit.unwrap_or(20)` uses a literal. A named constant like `DEFAULT_RESULT_LIMIT` would be more self-documenting, though the value is reasonable and the context makes it clear.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The codebase demonstrates strong complexity management overall: files are well-decomposed (format.rs at 368 lines stays under 400, builder.rs at 270 lines, reader.rs at 297 lines), the codec functions are small and focused (most under 20 lines), binary format constants are all named, BM25 parameters use named constants, and the module split (format/builder/reader/lang_map) follows single-responsibility well.

The two HIGH findings are the `build()` and `search()` methods, both of which exceed the 50-line function length threshold. They are linear in structure (no deep nesting beyond 3 levels), but their length makes them harder to review and test in isolation. Extracting helpers would bring them comfortably under the threshold without requiring architectural changes.

Conditions for approval:
1. Consider extracting helpers from `build()` and `search()` to bring each under 50 lines, or document why the current length is justified (e.g., the serialisation pipeline benefits from co-locality for the atomicity contract).
