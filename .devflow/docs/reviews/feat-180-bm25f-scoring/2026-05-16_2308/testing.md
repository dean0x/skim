# Testing Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Missing unit tests for `compute_field_lengths` helper** - `crates/rskim-search/src/index/builder.rs:200`
**Confidence**: 85%
- Problem: `compute_field_lengths()` is a private helper function containing non-trivial logic (empty-field-map fallback to `SearchField::Other`, `saturating_add` for overflow, `unwrap_or(u32::MAX)` for huge ranges). It is exercised only indirectly through `add_file_classified()` in the reader integration tests. No unit tests verify the empty-map fallback path, the saturating overflow path, or multi-range accumulation into the same field discriminant. The function is private, but it can be tested from within the same module via `builder_tests.rs`.
- Fix: Add focused unit tests in `builder_tests.rs` covering: (1) empty `field_map` maps everything to `SearchField::Other`, (2) multiple ranges mapping to the same field produce correct sum, (3) `source_len = 0` with empty map returns all-zero lengths.

**Missing test for `add_file_classified` with partial field_map (gaps/mismatched ranges)** - `crates/rskim-search/src/index/builder.rs:113`
**Confidence**: 82%
- Problem: `add_file_classified` advances `range_idx` linearly through `field_map` but the contract states the map must be "sorted, non-overlapping, contiguous." The tests always pass well-formed single-range field maps (e.g., `0..len` with one field). No test verifies the fallback to `SearchField::Other` when `field_map` ranges do not fully cover the source (positions between or beyond ranges). While `classify_source` guarantees contiguity, the API is `pub` so callers could pass non-contiguous maps.
- Fix: Add a test that calls `add_file_classified` with a field_map that leaves some bytes uncovered (e.g., `[(5..10, TypeDefinition)]` on a 20-byte source). Verify that positions outside the map's ranges get `SearchField::Other` field_id in the posting entries. Alternatively, add a debug assertion or validation in `add_file_classified` to reject non-contiguous field maps.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**No test for `NaN`/`Inf` field in `BM25FConfig`** - `crates/rskim-search/src/lexical/config.rs:64`
**Confidence**: 82%
- Problem: `BM25FConfig::validate()` checks `k1 < 0.0`, `boost < 0.0`, and `b` range `[0.0, 1.0]`, but does not reject `NaN` or `Inf` values for `k1` or `field_boosts`. `NaN < 0.0` evaluates to `false`, so `NaN` passes validation. If `k1 = NaN` is used in `bm25f_score`, the formula `tf_weighted / (tf_weighted + NaN)` produces `NaN` scores, which would corrupt search results. The validation tests do not cover this edge case.
- Fix: Add tests that set `k1 = f32::NAN` and `k1 = f32::INFINITY`, then assert that `validate()` returns `Err`. Correspondingly, update `validate()` to reject non-finite values using `f32::is_finite()`.

**`test_source_at_limit_boundary_does_not_error` allocates 100 MiB** - `crates/rskim-search/src/lexical/classifier_tests.rs:79`
**Confidence**: 80%
- Problem: This test creates a 100 MiB `String` (`" ".repeat(MAX_SOURCE_BYTES)`) to verify the boundary condition. While it uses JSON (non-tree-sitter) to avoid parser overhead, the 100 MiB allocation is excessive for a unit test and makes the test slow in memory-constrained CI environments. The test above it (`test_source_exceeding_limit_returns_error`) similarly allocates 100 MiB + 1 byte.
- Fix: Consider reducing `MAX_SOURCE_BYTES` for test purposes or testing with a much smaller sentinel value. Alternatively, add a `#[cfg(not(miri))]` gate or a `#[ignore]` attribute with a comment explaining the memory requirement, and run these only in CI. A cleaner approach: extract the limit as a parameter to an internal `classify_source_with_limit` function, test with a small limit (e.g., 1024), and have the public function hardcode the 100 MiB limit.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Missing integration test through the full classify+index+search pipeline** - (Confidence: 75%) -- The AC1/AC2 tests in `reader_tests.rs` manually construct field maps. No test feeds real source code through `classify_source()` and then indexes the result via `add_file_classified()` to verify end-to-end correctness of the classifier-to-scorer pipeline for actual Rust/Python/TypeScript code.

- **`dominant_field` tie-break with all-zero TF returns `Other` without test for non-obvious semantic** - `scoring_tests.rs:223` (Confidence: 65%) -- The `test_dominant_field_all_zero_returns_other` test verifies the return value, but the docstring says "lowest discriminant wins on ties" while `Other` has discriminant 7 (highest). The behavior is correct (0.0 never beats `best_tf = 0.0` via strict `>`), but the semantic gap between docs and implementation could confuse future maintainers.

- **No negative test for `add_file_classified` with overlapping ranges** - `builder.rs:113` (Confidence: 62%) -- If a caller passes overlapping ranges (violating the documented precondition), the linear scan may double-count field bytes. A test that documents the expected behavior (error, or last-range-wins) would guard the contract.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The test suite is strong. The PR adds 67 new test functions across 4 new test files plus additions to 3 existing files, all 203 tests pass (1 skipped release-only benchmark). The acceptance criteria (AC1-AC4) are explicitly tested. Determinism, edge cases (zero TF, zero avg length, extreme length ratios, b=0, b=1, k1=0), validation boundaries, format roundtrips, and invariant assertions (contiguity, non-overlap, field_lengths sum) are all covered. The conditions are: (1) add unit tests for `compute_field_lengths` to cover the empty-map and overflow paths directly, and (2) validate or test `NaN`/`Inf` handling in `BM25FConfig::validate()` to prevent NaN scores from corrupting search results.
