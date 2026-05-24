# Complexity Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### HIGH

**`search()` function exceeds recommended length and has elevated cyclomatic complexity** - `crates/rskim-search/src/index/reader.rs:256-379`
**Confidence**: 85%
- Problem: The `search()` method is 123 lines long (threshold: 50 lines for warning, 200 for critical) with 4 HashMap accumulators, a nested 2-pass loop (outer: ngrams, inner: postings + scoring), multiple early-continue paths, and an intermediate sort + map pipeline. The nesting depth reaches 4 levels at the deepest point (for -> if -> entry pattern -> if). The function manages 7 mutable local state variables (`doc_scores`, `doc_field_tfs`, `doc_positions`, `doc_meta_cache`, `tf_per_doc`, `pos_per_doc`, and `scoring_config`).
- Fix: Extract the inner loop body (lines 289-342) into a private method like `score_ngram_postings()` that takes the accumulators by mutable reference. This would reduce `search()` to ~60 lines and make the two-pass structure self-documenting:

```rust
// Sketch:
fn score_ngram_postings(
    &self,
    ngram_key: u16,
    idf: f64,
    lang_filter: Option<u8>,
    scoring_config: &BM25FConfig,
    doc_scores: &mut HashMap<u32, f64>,
    doc_field_tfs: &mut HashMap<u32, [f32; FIELD_COUNT]>,
    doc_positions: &mut HashMap<u32, Vec<Range<usize>>>,
    doc_meta_cache: &mut HashMap<u32, FileMetaEntry>,
) -> Result<()> {
    // ... first sub-pass + second sub-pass logic
}
```

### MEDIUM

**`build()` function is at the upper bound of recommended length** - `crates/rskim-search/src/index/builder.rs:247-351`
**Confidence**: 82%
- Problem: The `build()` method is 104 lines. While it is procedural and reads linearly (compute averages, sort, serialize, write), it handles 5 distinct phases: (1) compute averages, (2) sort postings/keys, (3) serialize postings + entries + metadata, (4) checksum + header construction, (5) atomic writes. Each phase is separated by comments, but the function does a lot of different things.
- Fix: Extract the serialization phase (lines 274-311) into a helper like `serialize_postings_and_entries()`. This is not urgent because the function reads top-to-bottom with clear phase comments, but extracting would reduce it to ~60 lines and make each phase independently testable.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`map_priority_to_field()` has two overlapping dispatch mechanisms** - `crates/rskim-search/src/lexical/classifier.rs:43-78` (Confidence: 65%) -- The function first matches on node kind strings (comments, strings, identifiers), then matches on the numeric priority (5/4/3/other). The node-kind match at lines 45-68 lists 18 specific strings across 3 blocks. If the set of recognized node kinds grows, this will become harder to maintain. A lookup table or a single enum-based dispatch might scale better.

- **4 HashMap accumulators in `search()` could be consolidated into a single struct** - `crates/rskim-search/src/index/reader.rs:281-287` (Confidence: 70%) -- The four parallel HashMaps (`doc_scores`, `doc_field_tfs`, `doc_positions`, `doc_meta_cache`) all keyed by `doc_id` could be consolidated into a single `HashMap<u32, DocAccumulator>` struct. This would reduce the number of hash lookups per iteration and make the relationship between the accumulators explicit.

- **Magic index constants `[0]`, `[6]` used for field discriminants in tests** - `crates/rskim-search/src/index/reader_tests.rs:466-467` (Confidence: 62%) -- The test `test_ac2_configurable_boosts_reverse_ranking` uses raw indices `reversed_config.field_boosts[0]` and `reversed_config.field_boosts[6]` with inline comments `// TypeDefinition` and `// StringLiteral`. Using `SearchField::TypeDefinition.discriminant() as usize` would be self-documenting and resilient to future discriminant changes.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new modules (`lexical/scoring.rs` at 119 lines, `lexical/config.rs` at 109 lines, `lexical/classifier.rs` at 208 lines) are well-structured: small, single-purpose functions with clear invariants documented in module-level doc comments. The `bm25f_score()` function is 47 lines with a clean loop, `dominant_field()` is 15 lines, `classify_source()` is 71 lines with a standard tree-sitter cursor walk, and `run_length_encode()` is 19 lines. The `compute_field_lengths()` helper is 17 lines. These are all within recommended thresholds.

The primary concern is the `search()` method at 123 lines, which is functional but would benefit from extracting the inner scoring loop into a helper. The `build()` method at 104 lines is procedurally clear but similarly at the boundary. Both are HIGH/MEDIUM rather than CRITICAL because they read linearly and have good comments -- the complexity is accidental length rather than tangled control flow.

Condition for approval: Consider extracting the inner ngram scoring loop from `search()` into a private method in a follow-up. Not a merge blocker given the linear readability and comprehensive test coverage (acceptance criteria tests AC1-AC4, edge case tests, determinism tests).
