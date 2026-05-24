# Testing Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### HIGH

**`open_with_config` test does not verify the custom config is actually used for scoring** - `crates/rskim-search/src/index/reader_tests.rs:541`
**Confidence**: 85%
- Problem: `test_open_with_config_stores_config` sets `k1 = 2.0` and verifies that search "returns results without panicking," but never asserts that the custom config actually changes scoring behavior relative to the default. This is a smoke test, not a behavior test. A regression where `open_with_config` silently ignores the config would pass this test.
- Fix: Compare scores from a default-config reader vs the custom-config reader on identical data, and assert they differ:
```rust
let default_reader = NgramIndexReader::open(dir.path()).unwrap();
let default_results = default_reader.search(&SearchQuery::new("main")).unwrap();
assert_ne!(
    results[0].score, default_results[0].score,
    "custom k1 should produce different scores than default"
);
```

---

**Missing unit tests for `resolve_field` and `compute_field_lengths` private helpers** - `crates/rskim-search/src/index/builder.rs:187-222`
**Confidence**: 82%
- Problem: The `resolve_field` binary search and `compute_field_lengths` functions contain non-trivial logic (binary search with `partition_point`, `saturating_add`, edge-case handling for empty maps) but have zero direct unit tests. They are only exercised indirectly through `add_file_classified` in the AC tests. A binary-search off-by-one bug could go undetected.
- Fix: Add a `#[cfg(test)]` module in `builder.rs` with targeted tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn resolve_field_empty_map_returns_other() {
        assert_eq!(resolve_field(0, &[]), SearchField::Other.discriminant());
    }

    #[test]
    fn resolve_field_single_range() {
        let map = vec![(0..10, SearchField::TypeDefinition)];
        assert_eq!(resolve_field(5, &map), SearchField::TypeDefinition.discriminant());
        assert_eq!(resolve_field(10, &map), SearchField::Other.discriminant());
    }

    #[test]
    fn compute_field_lengths_empty_map() {
        let lengths = compute_field_lengths(100, &[]);
        assert_eq!(lengths[SearchField::Other.discriminant() as usize], 100);
    }
}
```

### MEDIUM

**AC2 test has unused variable `layer`** - `crates/rskim-search/src/index/reader_tests.rs:453`
**Confidence**: 88%
- Problem: `test_ac2_configurable_boosts_reverse_ranking` calls `builder.build()` into `layer`, then separately opens a new reader via `NgramIndexReader::open()`. The `layer` is only dropped at line 476 with an explicit `drop(layer)`. The test verifies behavior through the new reader, not the `layer` returned by `build()`. This is confusing and wastes a reader allocation. More importantly, if `build()` returned a reader configured with non-default settings, this discrepancy would go unnoticed since the test uses a fresh `open()`.
- Fix: Remove the `layer` binding and `drop(layer)` — the `build()` call is only needed to write the index to disk:
```rust
builder.build().unwrap(); // writes index to disk
let reader = NgramIndexReader::open(dir.path()).unwrap();
```

---

**No test for `classify_source` error path** - `crates/rskim-search/src/lexical/classifier.rs:104-109`
**Confidence**: 80%
- Problem: The classifier's `match rskim_core::Parser::new(lang)` arm for `Err(_)` returns a single `Other` range, but no test directly verifies this code path for a language whose parser actually fails (distinct from the JSON/YAML/TOML path which is tested). In practice, all supported languages succeed, so the `Err` case is effectively dead code coverage. While not critical, it means a future change to Parser::new could alter behavior silently.
- Fix: If there is a language variant that produces a parser error (or if one can be introduced for testing), add a test. Alternatively, document that this path is unreachable for supported languages and is a defensive guard only.

---

**No negative/zero IDF test in scoring** - `crates/rskim-search/src/lexical/scoring_tests.rs`
**Confidence**: 80%
- Problem: `bm25f_score` accepts `idf: f64` but no test covers `idf = 0.0` (common term in every document) or `idf < 0.0` (term in more than half the corpus with some IDF formulations). The function would return 0.0 for idf=0.0 and negative for idf<0.0 — both mathematically correct but worth asserting explicitly to document the contract.
- Fix: Add edge case tests:
```rust
#[test]
fn test_zero_idf_returns_zero() {
    let cfg = BM25FConfig::default();
    let mut tfs = zero_field_tfs();
    tfs[0] = 5.0;
    let score = bm25f_score(0.0, &tfs, &[100; FIELD_COUNT], &avg_lengths(100.0), &cfg);
    assert_eq!(score, 0.0, "zero IDF should give zero score");
}

#[test]
fn test_negative_idf_produces_negative_score() {
    let cfg = BM25FConfig::default();
    let mut tfs = zero_field_tfs();
    tfs[0] = 1.0;
    let score = bm25f_score(-1.0, &tfs, &[100; FIELD_COUNT], &avg_lengths(100.0), &cfg);
    assert!(score < 0.0, "negative IDF should produce negative score");
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**No integration test with real `classify_source` + builder + reader pipeline** - `crates/rskim-search/src/index/reader_tests.rs`
**Confidence**: 82%
- Problem: The AC1 and AC2 tests manually construct field maps and pass them to `add_file_classified`. No test exercises the full pipeline: `classify_source(source, lang)` -> `add_file_classified(id, source, lang, &ranges)` -> `build()` -> `search()`. This means a mismatch between the classifier's output format and the builder's expectations could go undetected (e.g., if `classify_source` produced overlapping ranges, the binary search in `resolve_field` could silently misclassify bytes).
- Fix: Add one end-to-end test:
```rust
#[test]
fn test_end_to_end_classify_then_search() {
    use crate::lexical::classify_source;
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    
    let source = "struct Widget { name: String }";
    let field_map = classify_source(source, rskim_core::Language::Rust).unwrap();
    builder.add_file_classified(FileId(0), source, rskim_core::Language::Rust, &field_map).unwrap();
    builder.build().unwrap();
    
    let reader = NgramIndexReader::open(dir.path()).unwrap();
    let results = reader.search(&SearchQuery::new("Widget")).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].field, SearchField::TypeDefinition);
}
```

## Pre-existing Issues (Not Blocking)

_None identified at CRITICAL severity._

## Suggestions (Lower Confidence)

- **Missing multi-field interaction test** - `crates/rskim-search/src/lexical/scoring_tests.rs` (Confidence: 70%) — No test verifies that `bm25f_score` with multiple non-zero fields produces a score higher than any single field alone. This would document the additive property.

- **`test_bm25_short_dense_ranks_above_long_sparse` uses old BM25 naming** - `crates/rskim-search/src/index/reader_tests.rs:204` (Confidence: 65%) — The function name references "bm25" but the implementation is now BM25F. Renaming would improve clarity but is cosmetic.

- **Classifier tests lack a Comment/StringLiteral-specific assertion** - `crates/rskim-search/src/lexical/classifier_tests.rs` (Confidence: 72%) — Tests verify TypeDefinition, FunctionSignature, and ImportExport fields are produced, but no test explicitly checks that a `// comment` line classifies as Comment, or that a string literal classifies as StringLiteral. The `map_priority_to_field` function has dedicated branches for these but they lack dedicated assertions.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | - | 0 | 1 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The test suite is solid overall — 90+ new tests cover the BM25F scoring formula, config validation, classifier invariants (contiguous, non-overlapping, sum-equals-length), determinism, edge cases (zero avg lengths, extreme length ratios, b=1 with dl=0), and acceptance criteria. The coverage of boundary conditions in the scoring function is particularly thorough.

Conditions for unconditional approval:
1. Add a test that verifies `open_with_config` actually changes scoring behavior (not just "doesn't panic").
2. Add direct unit tests for the `resolve_field` binary search helper — off-by-one errors in binary search are notoriously subtle and the current indirect coverage via AC tests may not catch edge positions.
