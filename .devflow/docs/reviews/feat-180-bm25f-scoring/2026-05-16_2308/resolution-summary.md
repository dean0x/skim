# Resolution Summary

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16_2308
**Review**: .docs/reviews/feat-180-bm25f-scoring/2026-05-16_2308
**Command**: /resolve

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 19 |
| Fixed | 16 |
| False Positive | 0 |
| Deferred | 2 |
| Blocked | 0 |
| Not Batched (Suggestions) | ~18 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| NaN/Infinity bypass in BM25FConfig::validate() | config.rs:64-84 | fa1394a |
| NaN/Infinity in decoded avg_field_lengths from index header | format.rs:228-233 | fa1394a |
| NaN/Infinity test coverage for config and header | config_tests.rs, format_tests.rs | fa1394a |
| Unused tree-sitter direct dependency removed | Cargo.toml:21 | 84ed5a4 |
| FieldClassifier/NodeInfo documented as extensibility point | types.rs:405-441 | 84ed5a4 |
| Pre-existing borrow bug in reader.rs (tf_per_doc consuming loop) | reader.rs | 84ed5a4 |
| HashMap allocation churn per search query | reader.rs:281-296 | 48fb879 |
| search() method extraction (score_ngram_postings helper) | reader.rs:256-379 | 48fb879 |
| file_count visibility reverted to private | builder.rs:47 | b5cff4d |
| debug_assert for compute_field_lengths precondition | builder.rs:200-217 | b5cff4d |
| postings_buf Vec pre-sized with capacity | builder.rs:275 | b5cff4d |
| compute_field_lengths unit tests added (4 tests) | builder_tests.rs | b5cff4d |
| add_file_classified partial field_map tests (2 tests) | builder_tests.rs | b5cff4d |
| Coupling comment for map_priority_to_field | classifier.rs:43-78 | 9255e58 |
| Doc comment separation (classify_source / MAX_SOURCE_BYTES) | classifier.rs:80-99 | 9255e58 |
| Body node stamping (block/compound_statement → FunctionBody) | classifier.rs:149-156 | 9255e58 |
| 100 MiB boundary test marked #[ignore] | classifier_tests.rs:79 | 9255e58 |

## Deferred to Tech Debt
| Issue | File:Line | Risk Factor |
|-------|-----------|-------------|
| Per-byte Vec allocation in classify_source | classifier.rs:131 | Requires complete algorithmic redesign (two-pass range-merge). Current byte-by-byte overwrite with innermost-wins is the documented algorithm. 100 MiB cap bounds worst case; typical files <1 MiB. Future optimization, not correctness bug. |
| build() method at 104 lines | builder.rs:247-351 | Procedural, reads linearly with clear phase comments. Extraction would improve modularity but function is at boundary, not over critical threshold. Follow-up refactor. |

## Suggestions Not Batched (Lower Confidence 60-79%)
Approximately 18 suggestions from reviewers were recorded but not batched for resolution. These include:
- Consolidate 4 HashMaps into DocAccumulator struct (complexity, 70%)
- idf_for_key returns f32 but callers use f64 (consistency, 65%)
- Dead bm25_score behind cfg(test) cleanup (consistency/regression, 70%)
- Tree-sitter AST walk implicit termination bound (reliability, 70%)
- Missing end-to-end classify+index+search integration test (testing, 75%)
- Format v1→v2 no automatic upgrade path (regression, 75%)
- Various minor naming, documentation, and allocation suggestions

## Test Results
- rskim-search: 224 pass, 0 fail, 2 skip (release-only benchmark + ignored 100 MiB boundary test)
