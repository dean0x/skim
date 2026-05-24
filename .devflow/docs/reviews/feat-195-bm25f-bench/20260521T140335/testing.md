---
focus: testing
reviewer: Reviewer
timestamp: 2026-05-21T14:03:35Z
pr: 247
branch: feat/195-bm25f-bench
head: 101385eb
---

# Testing Review

## Summary

The rskim-bench crate ships 89 tests across 10 files with good unit-level coverage of core modules (metrics, configs, split, extractors, tuning, report). The main gaps are a tautological integration test that cannot detect regressions, missing error-path coverage for the harness, and no integration-level testing of the extractor dispatch or the `report` subcommand's deserialization path.

## Findings

### BLOCKING -- Tautological test: file_id_assignment_deterministic_when_sorted
- **File:** crates/rskim-bench/tests/integration.rs:204
- **Confidence:** 95%
- **Description:** This test runs the exact same deterministic `enumerate().map()` expression twice on the same input and asserts the two results are equal. It is a tautology -- it can never fail regardless of how `FileId` assignment changes in production code. It does not exercise any production function; it only tests that `Vec::iter().enumerate()` is deterministic, which is a property of the standard library. The stated goal (AC24 -- deterministic file ID assignment from sorted paths) is not validated because the actual `run_on_files` / `run_bench` file-sorting + ID-assignment logic is never invoked. If someone reorders the sort or changes the assignment scheme, this test will still pass.
- **Suggestion:** Replace with a test that invokes the actual production path (`run_on_files`) twice with the same files in different initial orderings and asserts the resulting `qrel_count`, metrics, or qrel file IDs are identical across both runs. Alternatively, provide an unsorted list, sort it, assign IDs, then verify the mapping is as expected.

### BLOCKING -- Dead type `EvalResult` has no tests and no callers
- **File:** crates/rskim-bench/src/types.rs:24
- **Confidence:** 92%
- **Description:** The `EvalResult` struct is defined but never constructed, used, or tested anywhere in the crate. It is dead code that adds maintenance surface. While not strictly a test issue, the fact that there is zero test coverage for this type (because it has no callers) signals either an incomplete implementation or a cleanup omission. If it is intended for future use, it should be `#[allow(dead_code)]` with a comment; otherwise it should be removed.
- **Suggestion:** Remove `EvalResult` from `types.rs` if unused, or add tests that exercise the intended usage if it is part of the public API contract.

### SHOULD-FIX -- No integration test for `extract_symbols` dispatch (mod.rs)
- **File:** crates/rskim-bench/src/extract/mod.rs:39
- **Confidence:** 85%
- **Description:** The `extract_symbols` dispatch function is tested indirectly (through `generate_qrels` in integration tests which only exercises Rust content). There are no tests that call `extract_symbols` directly with Python or Go content and verify the dispatch routes correctly. The per-language extractors have unit tests, but a bug in the `match` dispatch (e.g., routing Go to the Python extractor) would not be caught by any existing test. Additionally, the fallback for unsupported languages (returning empty `Vec`) is untested.
- **Suggestion:** Add a small integration test that calls `extract_symbols` with known content for each supported language (Rust, Python, Go) plus one unsupported language, asserting non-empty results for supported and empty for unsupported.

### SHOULD-FIX -- Missing error-path test for `run_on_files` when indexing fails
- **File:** crates/rskim-bench/src/harness.rs:36
- **Confidence:** 82%
- **Description:** `run_on_files` has multiple failure points (index builder creation, per-file indexing, qrel generation, qrel coverage validation, index opening). Only the happy path is tested. There are no tests verifying that meaningful errors propagate when, for example, no qrels can be generated from the input files (insufficient symbols), or when `contents` is missing entries for indexed files. The `qrel.rs` unit tests cover the "too few qrels" error, but the harness-level error propagation through `anyhow::Context` is never exercised.
- **Suggestion:** Add at least one test that passes insufficient content to `run_on_files` (e.g., a single file with `"fn x() {}"`) and asserts the error is `Err` with a descriptive message containing "generating qrels".

### SHOULD-FIX -- Weak assertion in full_pipeline_produces_non_zero_metrics
- **File:** crates/rskim-bench/tests/integration.rs:284
- **Confidence:** 80%
- **Description:** The assertion `any_nonzero_mrr` uses an OR condition (`m.mrr > 0.0 || m.query_count > 0`) that will pass even if MRR is 0 for all configs as long as any config has `query_count > 0`. Since `query_count` will almost always be non-zero (there are enough symbols in the synthetic data), this test cannot detect a regression where search stops returning relevant results. The test name promises "non-zero metrics" but only verifies query presence.
- **Suggestion:** Split into two assertions: (1) assert `query_count > 0` for at least one split, and (2) assert `mrr > 0.0` for at least one config+split combination. This ensures the search engine actually finds relevant documents.

### SHOULD-FIX -- No test for `aggregate_results` with mismatched config names
- **File:** crates/rskim-bench/src/harness.rs:171
- **Confidence:** 80%
- **Description:** `aggregate_results` collects config names from `repos[0]` and expects all repos to have the same config names. If repos have different configs, the `macro_average` function will silently produce incorrect averages or zeros. There is no test verifying this edge case, nor any assertion/guard in the code itself.
- **Suggestion:** Add a test that passes repos with different config name sets and either (a) verify the current behavior is documented and acceptable, or (b) add a validation check and test the error path.

### INFORMATIONAL -- Integration tests only exercise Rust content
- **File:** crates/rskim-bench/tests/integration.rs:25
- **Confidence:** 88%
- **Description:** All integration tests (`synthetic_rust_files`, `full_pipeline_produces_non_zero_metrics`, `all_configs_produce_results_for_same_query`, `aggregate_results_macro_average`) use only Rust source content. The full pipeline with Python or Go content is never exercised at the integration level. While the per-language extractors have unit tests, a failure in how Python/Go symbols interact with the qrel generation, split, or evaluation pipeline would not be caught.
- **Suggestion:** Consider adding a synthetic Python or Go file set as a second integration test variant to increase cross-language confidence, or document that single-language integration coverage is intentional for this phase.

### INFORMATIONAL -- `precision_at_k` allows duplicate FileIds without detection
- **File:** crates/rskim-bench/src/metrics.rs:66
- **Confidence:** 65%
- **Description:** If the ranked list contains duplicate `FileId` values (e.g., a bug in search returns the same file twice), `precision_at_k` would count it multiple times, inflating the metric. There is no test for this edge case. This is a lower-confidence concern because the search layer should not produce duplicates, but defensive testing would catch subtle integration bugs.

## Suggestions (Lower Confidence)

- **Missing test for `report` subcommand deserialization** - `crates/rskim-bench/src/main.rs:355` (Confidence: 72%) -- The `run_report` function deserializes a JSON file into `BenchResult`, but there is no test verifying that `to_json` output can be round-tripped through `serde_json::from_str::<BenchResult>()`. A serialization format change could break the report subcommand silently.

- **No test for `partition` with single-item input** - `crates/rskim-bench/src/split.rs:40` (Confidence: 68%) -- Edge case where partition receives exactly 1 item. While trivially correct from the implementation, boundary conditions are worth verifying for IR evaluation correctness.

- **Tuning tests use trivial mock evaluators** - `crates/rskim-bench/src/tuning.rs:203` (Confidence: 62%) -- The mock evaluator is a simple step function that makes convergence trivial. A more realistic evaluator with a smooth landscape would better test the coordinate descent logic, particularly the convergence threshold check.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | - | 2 | - | - |
| Should Fix | - | 3 | 1 | - |
| Pre-existing | - | - | 2 | - |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED
