# Consistency Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Test function name does not match its assertion threshold** - `linearize_tests.rs:428`
**Confidence**: 95%
- Problem: The test function is named `linearize_1000_line_file_under_5ms` but the assertion checks `elapsed.as_millis() < 10` (10ms, not 5ms). The threshold was raised from 5ms to 10ms but the function name was not updated. The error message also says `expected < 10ms`, contradicting the function name.
- Fix: Rename the function to match its actual threshold:
  ```rust
  fn linearize_1000_line_file_under_10ms() {
  ```
  Or restore the 5ms threshold if 5ms is the correct target. The name and behavior must agree.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Removed `#[must_use]` attribute from `linearize_source`** - `linearize.rs:195` (Confidence: 65%) -- The original had `#[must_use = "linearize_source returns a Result that must be checked"]`. While Rust's `Result` type already carries `#[must_use]` at the type level (so the compiler still warns on discarded Results), the codebase's `ngram.rs`, `ast_weights.rs`, and `types.rs` consistently apply explicit `#[must_use]` on public functions returning non-trivial values. The removal breaks that local convention. However, the attribute was arguably redundant for `Result`-returning functions, so this is a style judgment call.

- **Removed `known_kind_roundtrips_through_lang_map` test** - `linearize_tests.rs` (Confidence: 70%) -- This test was deleted without replacement. It verified that binary-search vocabulary lookup roundtrips correctly (`NODE_KIND_VOCABULARY.binary_search("function_item")` resolves back to `"function_item"`). While simple, it served as a canary for vocabulary array corruption. The coverage it provided is partially subsumed by `rust_lang_map_contains_known_kinds` and `vocabulary_is_sorted`, but neither directly asserts the binary-search roundtrip property. Applies ADR-001 (fix noticed issues immediately).

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Positive Consistency Observations

1. **AstWalkIter adoption is uniform**: Both consumers (`linearize.rs` and `ast_extract.rs`) use the exact same pattern -- `AstWalkIter::new(tree.walk(), AstWalkConfig { ... })` with `for item in iter.by_ref()` and post-iteration `iter.node_count()` / `iter.error_count()` reads. The shared primitive eliminates the duplicated DFS loop without diverging caller patterns.

2. **Constants are aligned**: `MAX_AST_DEPTH` (500, `u32`) and `MAX_AST_NODES` (100,000, `u32`) match between `linearize.rs` and `ast_extract.rs`, and both match the `AstWalkConfig::default()` in `rskim-core`. The type promotion from `u16` to `u32` for `MAX_AST_DEPTH` in `linearize.rs` is correct -- it now matches the `AstWalkConfig::max_depth: u32` field type directly, eliminating the previous implicit widening.

3. **SearchError::Ast rename is correct**: The rename from `AstError` to `Ast` matches the existing naming convention (`Git`, `Database`, `Io`, `Core` -- none have an `Error` suffix). No remaining references to the old name exist anywhere in the codebase.

4. **Error handling pattern is consistent**: Both modules follow the same grammar-load vs. parse-error distinction: `Parser::new` failure returns `Ok(default)`, parse failure returns `Ok(default)`, only grammar load errors in the public API surface as `Err`. This matches the feature knowledge convention: `SearchError::Ast` for grammar failures, `Ok(default)` for parse failures.

5. **LinearNode conventions preserved**: `Copy` trait, `u16` for both `kind_id` and `depth`, sentinel `0` for unknown kinds -- all match the feature knowledge specification.

6. **Documentation updates are accurate**: The `lib.rs` module doc now describes all public modules. The invariant doc comment fix (`(named, non-error)` to `(non-error)`) corrects a factual inaccuracy -- the code never filtered on `is_named()`.

7. **Test improvements strengthen assertions**: The `error_nodes_are_skipped_and_counted` test now asserts `error_count > 0` directly instead of the weaker `error_count > 0 || node_count > 0` disjunction. The renamed `node_count_never_exceeds_max_ast_nodes` test has an accurate name and improved documentation explaining why the guard cannot be triggered by source alone.
