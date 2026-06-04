# Consistency Review Report

**Branch**: feature-187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01
**PR**: #265

## Issues in Your Changes (BLOCKING)

### HIGH

**SearchError variant naming breaks crate convention** - `crates/rskim-search/src/types.rs:624`
**Confidence**: 95%
- Problem: The new variant is named `AstError(String)`, but every other variant in the `SearchError` enum avoids the "Error" suffix. The existing 9 variants use concise domain nouns: `Core`, `IndexCorrupted`, `InvalidQuery`, `FileNotFound`, `Io`, `Git`, `FileTooLarge`, `CapacityExceeded`, `Database`. Adding "Error" is redundant since these are already variants of `SearchError`.
- Fix: Rename `AstError` to `Ast` to match the established convention:
```rust
/// AST processing error (e.g. grammar load failure for a tree-sitter language).
///
/// Distinct from parse errors (which produce empty results gracefully):
/// this variant signals that the language grammar itself failed to load,
/// which is an unrecoverable configuration problem, not a file-level error.
#[error("AST error: {0}")]
Ast(String),
```
Update the single construction site in `linearize.rs:207` accordingly:
```rust
.map_err(|e| SearchError::Ast(format!("grammar load failure for {language:?}: {e}")))?;
```

### MEDIUM

**`#[must_use]` uses custom message string, but all other instances in the crate use bare `#[must_use]`** - `crates/rskim-search/src/ast_index/linearize.rs:188`
**Confidence**: 90%
- Problem: The new function uses `#[must_use = "linearize_source returns a Result that must be checked"]`, but every other `#[must_use]` annotation in rskim-search (13+ instances across `ngram.rs`, `weights.rs`, `types.rs`, `temporal/scoring.rs`, `temporal/mod.rs`, `ast_weights.rs`, `index/format.rs`, `index/lang_map.rs`, `index/reader.rs`, `lexical/scoring.rs`, `lexical/query.rs`) uses the bare `#[must_use]` form without a custom message.
- Fix: Replace with bare `#[must_use]` to match crate convention:
```rust
#[must_use]
pub fn linearize_source(
```

**Test file `#![allow]` style inconsistency within the PR itself** - `crates/rskim-search/src/ast_index/linearize_tests.rs:13-14`
**Confidence**: 85%
- Problem: The new test file uses two separate `#![allow]` lines:
  ```rust
  #![allow(clippy::unwrap_used)]
  #![allow(clippy::expect_used)]
  ```
  Meanwhile, the same PR modifies `rskim-research/src/ast_extract.rs` to combine them on a single line:
  ```rust
  #![allow(clippy::unwrap_used, clippy::expect_used)]
  ```
  Additionally, existing test files in rskim-search (e.g. `cochange/builder_tests.rs`, `cochange/format_tests.rs`, `cochange/reader_tests.rs`) all use a single `#![allow(clippy::unwrap_used)]` line. The two-separate-lines style deviates from both the PR's own precedent and the crate's existing pattern.
- Fix: Combine into one line matching the style used in the same PR's `ast_extract.rs` change:
  ```rust
  #![allow(clippy::unwrap_used, clippy::expect_used)]
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**lib.rs architecture doc comment does not mention the new `ast_index` module** - `crates/rskim-search/src/lib.rs:1-15`
**Confidence**: 92%
- Problem: The module-level doc comment in `lib.rs` enumerates every module in the crate (`types`, `index`, `ngram`, `temporal`, `cochange`) but does not mention the newly added `ast_index` module. Every other public module is listed in this architecture summary.
- Fix: Add a bullet for `ast_index` to the architecture doc comment:
```rust
//! - The `ast_index` module linearizes tree-sitter CSTs into compact
//!   depth-encoded node sequences for AST n-gram extraction (pure, no I/O).
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Bench file missing inline comment on `#![allow]`** - `crates/rskim-search/benches/linearize_bench.rs:11` (Confidence: 65%) -- The existing `transform_bench.rs` uses `#![allow(clippy::unwrap_used)] // Unwrapping is acceptable in benchmarks` with an explanatory comment. The new bench omits this comment. Very minor stylistic inconsistency.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The new `ast_index` module follows the established module structure pattern well (mod.rs + impl.rs + tests.rs with `#[path]`, public re-exports in lib.rs, `#[non_exhaustive]` SearchError enum, workspace dependency declarations, Criterion bench configuration). The main consistency gap is the `AstError` variant name which breaks the naming convention used by all 9 existing variants. The `#[must_use]` custom message and split `#![allow]` lines are minor but worth aligning. Applies ADR-001 -- all findings surfaced for immediate resolution.
