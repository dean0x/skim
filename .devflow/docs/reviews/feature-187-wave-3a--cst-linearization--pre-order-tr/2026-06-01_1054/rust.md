# Rust Review Report

**Branch**: feature-187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Redundant `Parser::new` on every `linearize_source` call** - `crates/rskim-search/src/ast_index/linearize.rs:206`
**Confidence**: 85%
- Problem: `LANG_MAPS` (LazyLock) already calls `Parser::new` + `parser.parse("")` for each language during initialization to build lookup tables. Then `linearize_source` creates a **second** `Parser::new` on every single call (line 206). `Parser::new` internally allocates a tree-sitter parser and sets its language grammar -- this is redundant work on the hot path. Since the grammar is already validated during `LANG_MAPS` init, the only failure mode for the second `Parser::new` would be an ABI-level inconsistency that should never happen at runtime.
- Fix: Consider caching `Parser` instances inside `LANG_MAPS` (using thread-local storage since `Parser` is not `Sync`), or accept the overhead as documented. At minimum, the `parser.language()` API on the tree-sitter `Parser` type could be used to get the `LanguageRef` for `LANG_MAPS` init (lines 133-143) instead of parsing an empty string, removing the unnecessary parse step during initialization.

### MEDIUM

**`linearize_tree` returns `Result` but is infallible** - `crates/rskim-search/src/ast_index/linearize.rs:232-235`
**Confidence**: 82%
- Problem: The `linearize_tree` function's return type is `crate::types::Result<LinearizeResult>` but it can only ever return `Ok(...)`. Both exit points (lines 261 and 308) return `Ok(result)`. There is no path that produces an `Err`. This violates the Rust principle that the type system should represent actual possibilities -- returning `Result` when no error is possible is misleading to callers.
- Fix: Change `linearize_tree` to return `LinearizeResult` directly. Since it's a private function, this has no public API impact. The `?` operator is not used inside it, so there's nothing to unwind:
  ```rust
  fn linearize_tree(
      tree: &tree_sitter::Tree,
      lang_map: &[Option<u16>],
  ) -> LinearizeResult {
  ```

**Doc comment says "named" but code emits all non-error nodes** - `crates/rskim-search/src/ast_index/linearize.rs:83`
**Confidence**: 84%
- Problem: The `LinearizeResult` doc comment at line 83 says "named, non-error" but the traversal code does NOT filter by `node.is_named()`. Anonymous nodes (punctuation tokens like `{`, `}`, `(`, `)`, `,`, `;`) are emitted into `nodes`. The `NODE_KIND_VOCABULARY` includes these tokens (confirmed: `{`, `}`, `(`, `)`, `,`, `;` all present). This is consistent with the existing `rskim-research/src/ast_extract.rs` which also visits all nodes, but the doc comment is misleading.
- Fix: Either update the doc comment to say "non-error" instead of "named, non-error", or if the intent is to only emit named nodes, add `if !node.is_named() { continue; }` filtering. Given consistency with existing `ast_extract.rs`, updating the comment is the correct fix:
  ```rust
  /// `nodes` (non-error) or is counted in `error_count` (ERROR/MISSING).
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`is_missing()` not checked in `rskim-research/src/ast_extract.rs`** - `crates/rskim-research/src/ast_extract.rs:190`
**Confidence**: 80%
- Problem: The modified file `ast_extract.rs` (where `clippy::expect_used` was added to test module) has a pre-existing inconsistency with the new code. The new `linearize.rs` correctly checks `node.is_error() || node.is_missing()` (line 273), but the existing `ast_extract.rs` only checks `node.is_error() || kind == "ERROR"` (line 190) -- it never checks `node.is_missing()`. MISSING nodes are synthetic nodes inserted by tree-sitter for error recovery and should arguably receive the same treatment as ERROR nodes. applies ADR-001.
- Fix: Update `ast_extract.rs` line 190 to match the new pattern:
  ```rust
  let is_error = node.is_error() || node.is_missing();
  ```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`LANG_MAPS` init could use `parser.language()` instead of `parser.parse("")`** - `crates/rskim-search/src/ast_index/linearize.rs:140` (Confidence: 70%) -- The tree-sitter `Parser` type has a `language()` method that returns `Option<LanguageRef>` directly, which has `node_kind_count()` and `node_kind_for_id()`. Parsing an empty string to get the language object works but is an unnecessary roundabout during init.

- **Performance test generates 100 functions, not 1000 as comment states** - `crates/rskim-search/src/ast_index/linearize_tests.rs:427` (Confidence: 75%) -- The comment says "1000-function Rust file" but the generator only creates 100 functions: `(0..100).map(...)`. The test name is `linearize_1000_line_file_under_5ms` which refers to lines rather than functions, and 100 functions at ~1 line each is roughly 100 lines, not 1000.

- **`known_kind_roundtrips_through_lang_map` test present in diff but absent from final file** - (Confidence: 65%) -- The diff context shows a test called `known_kind_roundtrips_through_lang_map` between the vocabulary and core linearization cycles, but it does not appear in the final test file. This may be an intentional removal during development, but worth confirming nothing was accidentally dropped.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Conditions

1. Fix the `linearize_tree` return type (infallible function returning `Result`)
2. Fix the doc comment at line 83 ("named" -> "non-error")
3. Evaluate the redundant `Parser::new` on the hot path (may be acceptable for simplicity, but should be a conscious decision)

### Strengths

- Excellent use of `LazyLock` for thread-safe one-time initialization of per-language lookup tables
- `LinearNode` is correctly `Copy` (4 bytes) -- zero-cost pass-by-value
- Iterative `TreeCursor` DFS avoids stack overflow on deep trees
- Explicit bounds guards (`MAX_AST_DEPTH`, `MAX_AST_NODES`, `MAX_FILE_SIZE`) prevent pathological inputs -- avoids unbounded resource consumption (aligns with reliability principles)
- `unwrap_or(0)` sentinel fallback pattern is consistent with project conventions
- `clippy::unwrap_used=deny` and `clippy::expect_used=deny` enforced in production code; correctly relaxed only in test modules
- `#[must_use]` on `linearize_source` with descriptive message
- `SearchError::AstError(String)` variant correctly follows the existing pattern of string-wrapping at boundaries (consistent with `Git(String)`, `Database(String)`)
- `#[non_exhaustive]` on `SearchError` allows future variant additions
- Comprehensive test suite with 8 test cycles covering types, vocabulary, core traversal, error handling, bounds, multi-language, edge cases, and performance
- `Send + Sync` compile-time assertion tests
- `saturating_add` for depth increment prevents overflow
- No `unsafe` code
