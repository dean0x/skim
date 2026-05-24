# Testing Review Report

**Branch**: feat/195-bm25f-bench -> main
**Date**: 2026-05-22

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`aggregate_results` validation only checks `train_metrics` config names, not `test_metrics`** - `crates/rskim-bench/src/harness.rs:191-209`
**Confidence**: 85%
- Problem: The `aggregate_results` function validates that all repos share the same config names in `train_metrics` (lines 192-209), but does not validate `test_metrics`. If `test_metrics` somehow has different config names (e.g., due to a bug in a caller that constructs `RepoBenchResult` manually), the macro_average over `test_metrics` would silently produce incorrect results. The mismatch test (`aggregate_results_rejects_mismatched_config_names` at `tests/integration.rs:511`) only constructs mismatched `train_metrics` names, so it does not exercise a `test_metrics`-only mismatch.
- Fix: Either (a) also validate `test_metrics` config names in `aggregate_results`, or (b) add a test that verifies the current behavior is intentional (documenting that `test_metrics` mismatches are caught by the `macro_average` producing zero-valued entries, which may be acceptable since both splits are always generated from the same `bench_configs` slice).

### MEDIUM

**`file_id_assignment_deterministic_when_sorted` test re-implements the production logic instead of calling it** - `crates/rskim-bench/tests/integration.rs:206-246`
**Confidence**: 82%
- Problem: The test defines its own `assign_ids` closure that mirrors the production sorting-and-enumerate pattern from `main.rs`/`load_repo_files`. If the production code changes its sorting or assignment logic, this test would continue passing with the old logic. This is a form of implementation coupling -- the test validates its own copy of the algorithm, not the actual production function.
- Fix: Call the production `load_repo_files` function (or whichever public API assigns FileIds) with two differently-ordered inputs and assert the resulting mappings are identical. This would test the actual code path.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**No test coverage for `load_repo_files` function** - `crates/rskim-bench/src/main.rs:179-218`
**Confidence**: 82%
- Problem: The new `load_repo_files` function contains meaningful logic -- sorting by path, sequential FileId assignment, checked overflow via `checked_add`. None of this is directly unit-tested. The overflow guard (`checked_add`) is completely untested. While the function is exercised indirectly via the integration pipeline tests, those tests use small file sets that never approach `u32::MAX` and don't verify the sorting-before-assignment invariant at the `load_repo_files` level.
- Fix: Add a unit test for `load_repo_files` using a mock `FileSource` that returns files in random order, then assert the resulting `IndexedFile` vec is sorted by path with sequential IDs. (The overflow path is impractical to test directly but could be tested via a synthetic `FileSource` that pretends to return `u32::MAX` files, or by extracting the ID-assignment loop into a testable helper.)

**No test coverage for `build_index` and `make_train_qrels` helpers** - `crates/rskim-bench/src/main.rs:292-337`
**Confidence**: 80%
- Problem: `build_index` and `make_train_qrels` were extracted as separate functions (per commit message "batch-4 decomposition"). They are exercised indirectly through `run_tune`, but have no direct unit tests. `make_train_qrels` in particular contains the train-split filtering logic that could be tested in isolation.
- Fix: Add unit tests that call `build_index` with known files and verify the index directory contains expected artifacts, and `make_train_qrels` with known content and verify only train-split qrels are returned.

**`run_tune` error-handling closure swallows errors and returns 0.0 MRR** - `crates/rskim-bench/src/main.rs:392-402`
**Confidence**: 80%
- Problem: The evaluate closure inside `run_tune` catches errors from `evaluate_split` and returns 0.0 MRR. While the `eval_error_count` counter and stderr logging exist, there are no tests that verify this error-handling path. A systematic failure (e.g., corrupt index) would silently produce a "best" config with 0.0 MRR, and the test suite would not catch this regression.
- Fix: Add a test with a mock `SearchLayer` that returns errors for some queries, verifying that (a) the tuning completes without panicking, and (b) the error counter is incremented.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Parallel `run_bench` repos each start FileIds at 0** - `crates/rskim-bench/src/main.rs:258` (Confidence: 70%) -- In `run_bench`, each repo is processed independently with `load_repo_files(..., 0)`, meaning all repos use FileId(0), FileId(1), etc. This is correct because each repo gets its own index, but the same pattern with `run_tune` required reassigning global IDs (lines 354-371). A comment explaining why bench repos don't need global IDs would prevent future confusion.

- **`sweep_parameter` helper lacks direct unit tests** - `crates/rskim-bench/src/tuning.rs:54-86` (Confidence: 65%) -- The new `sweep_parameter` function is tested indirectly through `coordinate_descent`, but its edge cases (e.g., all candidates failing validation, no improvement found) are not isolated.

- **Integration test `extract_symbols_dispatch_integration` does not verify symbol names** - `crates/rskim-bench/tests/integration.rs:428-475` (Confidence: 62%) -- The test checks that symbols are non-empty and contain expected field types, but does not assert that the extracted symbol names match expected values (e.g., `test_func` for Rust). This makes the test weaker against extraction regressions that return symbols with wrong names.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 3 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The test suite is well-structured with 92 tests covering configs, extraction, qrels, metrics, splitting, tuning, harness, and reporting. The PR adds 3 solid new integration tests (extract dispatch, error path, mismatch validation) and improves existing assertions (split pipeline into query_count + MRR checks, stronger determinism test with two orderings). The main gaps are: (1) `aggregate_results` only validates train_metrics config names but not test_metrics, which the mismatch test does not fully cover; (2) several newly extracted helper functions (`load_repo_files`, `build_index`, `make_train_qrels`) lack direct tests; and (3) the `run_tune` error-swallowing closure is untested. None of these are blocking since the indirect coverage through integration tests is adequate for merge, but they should be addressed before the next release.
