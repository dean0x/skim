# Rust Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-11

## Issues in Your Changes (BLOCKING)

### MEDIUM

**NodeInfo.kind uses &'static str which may constrain non-tree-sitter implementors** - `crates/rskim-search/src/types.rs:244`
**Confidence**: 82%
- Problem: `NodeInfo` uses `pub kind: &'static str` for the grammar rule name. Tree-sitter nodes return `&'static str` from `node.kind()`, so this works naturally for tree-sitter consumers. However, for non-tree-sitter languages (JSON, YAML, TOML) referenced in the doc comments, implementors would need to use string literals or `Box::leak()` to produce `&'static str` from dynamically-determined node kinds, which is ergonomically awkward and could leak memory if done naively.
- Fix: Consider using `Cow<'static, str>` to allow both static references (tree-sitter) and owned strings (non-tree-sitter):
  ```rust
  pub kind: Cow<'static, str>,
  ```
  Alternatively, if all non-tree-sitter kinds are known at compile time (which they likely are — fixed set like "key", "value", "document"), `&'static str` is fine and the current design holds. Document this constraint explicitly.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **SearchResult does not implement PartialEq; consider a helper method** - `crates/rskim-search/src/types.rs:158` (Confidence: 65%) — The comment explains why `PartialEq` is omitted (f64 NaN), but downstream test code will need to compare results field-by-field. A `fn approx_eq(&self, other: &Self, epsilon: f64) -> bool` convenience method could reduce boilerplate in future test code.

- **Result type alias shadows std::result::Result** - `crates/rskim-search/src/types.rs:293` (Confidence: 70%) — The `pub type Result<T> = std::result::Result<T, SearchError>` alias is re-exported from `lib.rs`. This is idiomatic Rust (matches `std::io::Result`), but consumers who also use `anyhow::Result` or `std::result::Result` in the same scope may need explicit disambiguation. Consider whether the alias should remain module-private or if it should continue to be re-exported publicly.

- **Edition 2024 if-let chains reduce nesting but decrease grep-ability** - multiple files (Confidence: 62%) — The collapsible_if refactoring using edition 2024 if-let chains (e.g., `crates/rskim/src/cmd/git/diff/render.rs:216-220`) chains three conditions into a single `if`. While syntactically cleaner, deeply chained conditions can be harder to set breakpoints on or reason about individually during debugging. This is a style preference, not a defect.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Assessment

This PR introduces a well-designed foundation crate with excellent Rust practices:

**Strengths:**
- Proper use of `thiserror` for library error types with `#[from]` conversions
- Newtype pattern (`FileId`) for type-safe identifiers (C-NEWTYPE)
- `#[must_use]` on `SearchField::name()` and `SearchQuery::new()`
- Clean separation: pure library (no I/O), CLI handles I/O boundary
- Trait design (`SearchLayer`, `LayerBuilder`, `FieldClassifier`) follows the Strategy Pattern with appropriate `Send + Sync` bounds
- `NodeInfo` decouples from tree-sitter — non-tree-sitter languages can implement `FieldClassifier`
- Clippy lint configuration (`unwrap_used = "deny"`, `expect_used = "deny"`) enforces quality at the crate level
- Comprehensive test coverage (19 tests) including serialization roundtrips and trait contract tests
- Correct `pub(crate)` boundary — types.rs is private module, public API exports via lib.rs
- Edition 2024 upgrade applied correctly with clean if-let chains replacing nested conditionals
- `thiserror` 1.0 to 2.0 migration is a safe drop-in replacement (no breaking changes)
- Dev-dependency canary pattern in rskim/Cargo.toml catches API breakage early

**The single MEDIUM finding** (NodeInfo `&'static str`) is a design consideration rather than a bug — it works correctly for all documented use cases (tree-sitter kinds are inherently `'static`). The condition for approval is to either document the constraint or switch to `Cow<'static, str>` if dynamic kinds are anticipated.
