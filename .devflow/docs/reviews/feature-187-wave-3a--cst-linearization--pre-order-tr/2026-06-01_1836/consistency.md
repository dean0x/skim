# Consistency Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01
**Prior Resolutions**: Cycle 2 resolved 9/11 issues (0 FP, 0 deferred). No re-raised items.

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Test `#![allow]` pattern inconsistency (3 occurrences)** -- Confidence: 90%
- `crates/rskim-core/src/ast_walk.rs:265`, `crates/rskim-search/src/ast_index/linearize_tests.rs:13`, `crates/rskim-research/src/ast_extract.rs:370`
- Problem: These 3 files use `#![allow(clippy::unwrap_used, clippy::expect_used)]` in test modules, while the remaining 38 test files across the codebase consistently use only `#![allow(clippy::unwrap_used)]`. The `clippy::expect_used` addition is not harmful (since `expect` is just a more descriptive `unwrap`), but it creates a two-tier allow pattern that future contributors may be confused by -- should they add `expect_used` or not?
- Fix: Either align with the existing 38-file pattern by removing `clippy::expect_used` from the allow lists (and keeping the `expect` calls since `unwrap_used = "deny"` covers `unwrap` but `expect_used = "deny"` would need the allow to use `expect`), or adopt the expanded pattern project-wide. Given both crates (`rskim-core` and `rskim-search`) have `expect_used = "deny"` in their lint config, adding `clippy::expect_used` to the allow is *necessary* when the test code uses `.expect()`. The inconsistency is that 38 existing test files avoid `.expect()` entirely, using only `.unwrap()`. The new tests introduce `.expect()` calls, which is actually better practice (clearer panic messages). The right fix is no change -- the new pattern is better but should be adopted consistently in future.
- Severity note: Downgraded to MEDIUM because the new pattern is arguably the better one and the code compiles correctly.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Missing SQL `MAX_FILE_SIZE_LARGE` override in `linearize.rs`** -- Confidence: 82%
- `crates/rskim-search/src/ast_index/linearize.rs:50`
- Problem: `ast_extract.rs` (the sibling AST processor in `rskim-research`) uses `MAX_FILE_SIZE_LARGE = 1024 * 1024` (1 MiB) for SQL files because SQL migrations/schema dumps are routinely larger than 100 KiB. The new `linearize.rs` uses a flat `MAX_FILE_SIZE = 100 * 1024` for all languages including SQL. This means SQL files between 100 KiB and 1 MiB will produce bigrams in `ast_extract` but empty linearization results, creating an inconsistency in the downstream search index where some SQL files have lexical n-gram data but no AST structural data.
- Fix: Add the same SQL override:
  ```rust
  const MAX_FILE_SIZE_LARGE: usize = 1024 * 1024;
  
  // In linearize_source:
  let size_limit = match language {
      Language::Sql => MAX_FILE_SIZE_LARGE,
      _ => MAX_FILE_SIZE,
  };
  if source.len() > size_limit {
      return Ok(LinearizeResult::default());
  }
  ```
  Alternatively, if the omission is intentional (linearization is cheaper than n-gram extraction and large SQL files are not useful for structural search), add a comment documenting the deliberate divergence from `ast_extract.rs`.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`LinearNode` missing `Serialize` derive** - `crates/rskim-search/src/ast_index/linearize.rs:66` (Confidence: 65%) -- Most public types in `rskim-search/src/types.rs` derive `Serialize`/`Deserialize` since search results are serialized to JSON for `--json` output. `LinearNode` and `LinearizeResult` do not derive `Serialize`. This may be intentional (they are intermediate data, not user-facing output), but it deviates from the pattern of other search types.

- **`AstWalkNode` missing `Debug`/`Clone` derives** - `crates/rskim-core/src/ast_walk.rs:94` (Confidence: 62%) -- `AstWalkConfig` derives `Debug, Clone, Copy`, `LinearNode` derives `Debug, Clone, Copy, Default, PartialEq, Eq`, but `AstWalkNode` has no derives at all. This is likely because `tree_sitter::Node<'a>` may not implement all those traits, making derives impossible, but it is worth noting as an asymmetry.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Consistency Highlights (Positive)

The PR demonstrates strong consistency awareness across multiple dimensions:

1. **Module structure**: `ast_index/` follows the established `mod.rs` + `{impl}.rs` + `{impl}_tests.rs` pattern used by `cochange/`, `temporal/`, `lexical/`, `index/`, and `fields/`.

2. **Error handling**: `SearchError::Ast(String)` follows the existing string-wrapping pattern established by `SearchError::Git(String)`, `SearchError::Database(String)`, and `SearchError::CapacityExceeded(String)`. The `#[non_exhaustive]` enum accommodates the new variant without breaking downstream. Applies ADR-001 (fix noticed issues immediately) -- the new error variant was added rather than using a generic workaround.

3. **Result type alias**: `linearize_source` returns `crate::types::Result<LinearizeResult>`, matching the `pub type Result<T> = std::result::Result<T, SearchError>` pattern used throughout `rskim-search`.

4. **Graceful-default pattern**: Non-tree-sitter languages (JSON, YAML, TOML) return `Ok(LinearizeResult::default())` rather than errors, matching the established pattern in `ast_extract.rs` and consistent with the feature knowledge note.

5. **Re-export pattern**: `lib.rs` re-exports `LinearNode`, `LinearizeResult`, and `linearize_source` via `pub use ast_index::...`, matching the flat re-export pattern used for `cochange`, `temporal`, `ngram`, etc.

6. **Constants centralization**: Traversal bounds are correctly centralized on `AstWalkConfig::DEFAULT_MAX_DEPTH` / `DEFAULT_MAX_NODES`, eliminating the prior duplication. Both `linearize.rs` and the refactored `ast_extract.rs` reference the canonical constants. Avoids PF-002 (not deferring noticed duplication).

7. **Benchmark pattern**: `linearize_bench.rs` follows the same Criterion structure as `transform_bench.rs` -- module doc comment, `#![allow(clippy::unwrap_used)]`, fixture constants, `criterion_group!`/`criterion_main!`.

8. **`#[path]` test split**: `linearize.rs:273` uses `#[path = "linearize_tests.rs"]` for the test module, consistent with 27 other files in the codebase.

9. **Section dividers**: The `// ============================================================================` comment style matches the established convention throughout the codebase.

10. **`AstWalkConfig::default()` usage**: Both `linearize.rs` and the refactored `ast_extract.rs` use `AstWalkConfig::default()` rather than constructing config manually, ensuring the canonical bounds are picked up automatically.
