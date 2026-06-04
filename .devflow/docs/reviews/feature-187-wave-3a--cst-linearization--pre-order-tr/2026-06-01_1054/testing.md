# Testing Review Report

**Branch**: feature-187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`max_nodes_guard_truncates_output` does not actually exercise the MAX_AST_NODES cap** - `linearize_tests.rs:242-267`
**Confidence**: 85%
- Problem: The test generates only `"let x = 1;\n".repeat(100)` which produces far fewer than 100,000 AST nodes. The assertion `result.node_count <= MAX_AST_NODES` will always pass trivially -- the test is tautologically true. The test comment even acknowledges the difficulty: "In practice, a file this large is also > MAX_FILE_SIZE, so we set up a tighter scenario." But the tighter scenario still does not approach the cap. Without a test that generates enough nodes to trigger truncation, there is no evidence the inner bounds guard loop (lines 253-267 of `linearize.rs`) works correctly when `node_count >= MAX_AST_NODES`.
- Fix: Generate a source that stays under `MAX_FILE_SIZE` (102,400 bytes) but produces a high node count. A compact approach: a long chain of single-character identifiers separated by operators produces many nodes per byte. Alternatively, reduce `MAX_AST_NODES` locally in a test via a helper that exercises `linearize_tree` directly with a lower cap, or create a dedicated test-only function that accepts a configurable node limit.

**`error_nodes_are_skipped_and_counted` has a weak assertion that never fails** - `linearize_tests.rs:184-194`
**Confidence**: 82%
- Problem: The assertion is `result.error_count > 0 || result.node_count > 0`, which is tautologically true -- tree-sitter always produces at least a root node, so `node_count > 0` is always satisfied regardless of whether error handling works. The test name promises it verifies that error nodes are "skipped and counted," but the assertion does not actually verify `error_count > 0`. This means a regression that stops counting error nodes would not be caught.
- Fix: Assert `result.error_count > 0` directly for the malformed input `"fn foo( {"`. If tree-sitter does not produce ERROR nodes for that specific input, choose a different malformed snippet that reliably produces them (e.g., `"fn foo() { let = ; }"`). The `error_children_still_traversed` test at line 204 has a similar pattern but is less problematic because it primarily tests the invariant.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Performance test generates ~100 functions, not ~1000 lines as the name claims** - `linearize_tests.rs:421-444`
**Confidence**: 85%
- Problem: `linearize_1000_line_file_under_5ms` generates only 100 functions (`(0..100).map(...)`) where each function is ~1 line. The test name says "1000-line file" but the actual source is approximately 100 lines. The benchmark file (`linearize_bench.rs:92`) correctly uses `gen_rust_fns` with sizes up to 1000, but the unit test undershoots by 10x. While the test still validates performance, the name-to-behavior mismatch is misleading and could mask regressions on larger inputs.
- Fix: Change `(0..100)` to `(0..1000)` to match the test name, or rename the test to `linearize_100_fn_file_under_5ms`.

**Benchmark comment "Parsing happens in setup" is misleading** - `linearize_bench.rs:94`
**Confidence**: 80%
- Problem: The comment says "Parsing happens in setup (outside b.iter()) -- benchmark linearization only." But `linearize_source` parses the source *inside* `b.iter()` on every iteration -- there is no separate setup step that pre-parses. The `linearize_source` function creates a new parser and parses on every call. This means the benchmark measures parse + linearize, not linearize alone. The comment misleads future developers about what is actually being measured.
- Fix: Remove or correct the comment. Change to: `// Measures end-to-end cost: parse + linearize.` If the intent is to benchmark only linearization, the benchmark would need to parse once outside the loop and call `linearize_tree` directly (which is currently private).

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Missing test for `SearchError::AstError` path** - `linearize.rs:206-207` (Confidence: 70%) -- The `linearize_source` function has an `Err(SearchError::AstError(...))` return path when `Parser::new` fails. No test exercises this path. Since all 14 tree-sitter languages are compiled in, triggering this error in a unit test would require a language that passes the `LANG_MAPS` check but fails `Parser::new`, which may not be possible without mocking. A test could verify the error message format by constructing the error variant directly, or this could be accepted as untestable in unit tests.

- **`known_kind_roundtrips_through_lang_map` test missing from linearize_tests.rs** - `linearize_tests.rs` (Confidence: 65%) -- The diff includes a `known_kind_roundtrips_through_lang_map` test in the vocabulary lookup cycle, but it does not appear in the final `linearize_tests.rs` file. This test validates that binary-search lookup indices roundtrip correctly through the vocabulary. The `vocabulary_is_sorted` test partially covers this, but a direct roundtrip assertion would strengthen confidence that the `LANG_MAPS` binary search produces correct indices. It may have been removed intentionally during development, but it is a useful test to have.

- **`all_14_ts_languages_produce_output` tests only empty source** - `linearize_tests.rs:272-307` (Confidence: 62%) -- The multi-language test uses empty string `""` for all 14 languages, which only verifies that tree-sitter produces a root node. The benchmark file has proper per-language fixtures that would catch grammar-specific linearization issues. Consider testing with the same non-trivial fixtures used in benchmarks to validate that non-trivial source produces correct results across languages.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 0 | 0 |
| Should Fix | - | 0 | 2 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Testing Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The test suite is well-structured across 8 clearly defined cycles covering types, vocabulary, core linearization, error handling, bounds, multi-language, edge cases, and performance. The test helper trio (`parse_and_linearize`, `resolve_kinds`, `assert_node_count_invariant`) follows good patterns -- they reduce boilerplate while keeping assertions readable and behavior-focused. The Criterion benchmarks provide excellent regression coverage across languages, scaling, and depth dimensions.

The two blocking issues both concern tests that appear to validate important behavior but whose assertions are tautologically true: the MAX_AST_NODES cap test never approaches the cap, and the error-counting test has a disjunction that is always satisfied. Applies ADR-001 -- both should be fixed now rather than deferred, since they represent known gaps in test coverage for critical safety bounds and error handling paths.
