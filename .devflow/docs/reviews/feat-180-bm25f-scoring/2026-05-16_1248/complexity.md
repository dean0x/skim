# Complexity Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### HIGH

**`search()` function exceeds 50-line threshold (113 lines)** - `reader.rs:254-367`
**Confidence**: 88%
- Problem: The `search()` method spans 113 lines (lines 254-367) with 4 HashMap accumulators, a nested loop with branching for language filtering and metadata caching, followed by sorting and result assembly. The function handles accumulation, scoring, filtering, sorting, pagination, and result construction — multiple responsibilities in a single body.
- Fix: Extract the inner ngram processing loop (lines 281-331) into a private method like `accumulate_ngram_scores()`, and the result assembly (lines 345-364) into `build_results()`. This would bring each function under 50 lines and separate concerns:

```rust
fn accumulate_ngram_scores(
    &self,
    ngrams: &[(Ngram, f32)],
    scoring_config: &BM25FConfig,
    lang_filter: Option<u8>,
) -> Result<(HashMap<u32, f64>, HashMap<u32, [f32; FIELD_COUNT]>, HashMap<u32, Vec<Range<usize>>>)> {
    // ...moved logic...
}
```

---

**`build()` function exceeds 50-line threshold (100 lines)** - `builder.rs:252-355`
**Confidence**: 85%
- Problem: The `build()` method is 100 lines covering: average computation, posting list sorting, serialisation of posting lists, file metadata, entry arrays, CRC computation, header construction, file assembly, and atomic writes. While each step is sequential and linear, the function's length exceeds the 50-line threshold and handles at least 5 distinct serialisation stages.
- Fix: Extract serialisation into a helper. The posting list serialisation (lines 270-304), metadata serialisation (lines 306-316), and file writing (lines 339-354) could each be separate private methods. The most impactful split: extract `serialise_postings()` and `write_index_files()`.

### MEDIUM

**`classify_source()` mixes tree traversal with run-length encoding concern** - `classifier.rs:93-159`
**Confidence**: 80%
- Problem: At 66 lines, `classify_source()` is within the warning zone (50-line threshold for functions, 30-50 is the "MEDIUM" band). The function combines tree-sitter parsing, per-byte array allocation and stamping via tree traversal, and delegates to a separate `run_length_encode()`. The nesting depth reaches 3 levels at the innermost traversal logic (lines 145-158). The traversal cursor loop is idiomatic tree-sitter but the combined early-return + cursor + two nested loops pattern requires careful reading.
- Fix: The structure is already well-factored with `run_length_encode()` extracted. No action required unless the function grows further. Consider extracting the cursor walk into a named helper if additional field-stamping logic is added in the future.

## Issues in Code You Touched (Should Fix)

_No issues identified in this category._

## Pre-existing Issues (Not Blocking)

_No pre-existing complexity issues identified in the reviewed files._

## Suggestions (Lower Confidence)

- **Multiple HashMap accumulators in `search()` indicate potential for a struct** - `reader.rs:273-279` (Confidence: 70%) — Four parallel HashMaps (`doc_scores`, `doc_field_tfs`, `doc_positions`, `doc_meta_cache`) with the same key type could be consolidated into a single `HashMap<u32, DocAccumulator>` struct, reducing cognitive load and ensuring related data stays together.

- **Magic number `8` used across module boundaries** - `builder.rs:51`, `format.rs:93`, `config.rs:26` (Confidence: 65%) — The literal `8` appears in array declarations across `format.rs`, `builder.rs`, and `config.rs`. While `FIELD_COUNT` is defined in `config.rs`, the format module uses raw `[f32; 8]` / `[u32; 8]`. A single `FIELD_COUNT` import would couple the two modules — so the current approach may be intentional to keep the codec independent — but the implicit coupling via matching array sizes is a latent maintenance risk.

- **`map_priority_to_field` combines two classification strategies in one function** - `classifier.rs:43-78` (Confidence: 62%) — The function first matches on node kind (for comment/string/identifier) then falls through to priority-based classification. This dual-dispatch is not immediately obvious from the function name. A rename to `classify_node_kind_or_priority()` or a doc comment clarifying the precedence order would improve discoverability.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new `lexical/` module demonstrates good decomposition: each file has a single responsibility (config, classifier, scoring), functions are well-documented, and the scoring math is isolated into a pure function (`bm25f_score`) that is trivially testable. The `classify_source()` classifier is well-structured with the run-length encoding extracted.

The two HIGH findings are the `search()` and `build()` methods which exceed 50 lines. Both are sequential and have low nesting depth (max 3 levels), so they are readable despite their length. However, they would benefit from extraction to improve testability of individual stages. These are not blockers for merge given the linear control flow, but should be addressed before the module gains further complexity.

Conditions for approval:
1. Consider splitting `search()` into accumulation and result-assembly phases in a follow-up PR if this method gains additional logic (e.g., snippet extraction mentioned in the TODO).
