# Consistency Review Report

**Branch**: feat/186-query-engine -> main
**Date**: 2026-05-18

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Test file uses explicit imports instead of `use super::*` glob import** - `crates/rskim-search/src/lexical/query_tests.rs:5-8`
**Confidence**: 95%
- Problem: Every other `*_tests.rs` file in `crates/rskim-search/src/` uses `use super::*;` as its primary import from the parent module. The 8 existing test files (`config_tests.rs`, `scoring_tests.rs`, `classifier_tests.rs`, `reader_tests.rs`, `builder_tests.rs`, `format_tests.rs`, `lang_map_tests.rs`, `ngram_tests.rs`) all follow this convention. `query_tests.rs` uses explicit cherry-picked imports instead: `use super::MAX_QUERY_BYTES;` plus `use crate::lexical::{BM25FConfig, QueryEngine};`.
- Fix: Replace the explicit imports with the glob:
```rust
use super::*;
use crate::index::NgramIndexBuilder;
use crate::{FileId, LayerBuilder, SearchError, SearchLayer, SearchQuery};
```

**Test file uses `====` section dividers instead of `-----` dividers** - `crates/rskim-search/src/lexical/query_tests.rs:10,28,106,202`
**Confidence**: 90%
- Problem: Within the `lexical/` directory, the existing test files (`config_tests.rs`, `scoring_tests.rs`, `classifier_tests.rs`) use `// -----------------------------------------------------------------------` for section dividers. Source (non-test) files like `query.rs`, `classifier.rs`, `types.rs` use `// ============================================================================`. The new `query_tests.rs` uses the `====` style in a test file, breaking the convention.
- Fix: Replace all `// ============================================================================` dividers in `query_tests.rs` with `// -----------------------------------------------------------------------`.

**Error assertion pattern inconsistent with sibling test files** - `crates/rskim-search/src/lexical/query_tests.rs:43-51,73-81,94-97`
**Confidence**: 85%
- Problem: The existing test files in the same crate use `format!("{}", result.unwrap_err())` followed by `msg.contains(...)` for error assertions (seen in `config_tests.rs` 11 times, `builder_tests.rs` 2 times). The new `query_tests.rs` uses `match result.unwrap_err() { SearchError::InvalidQuery(msg) => { ... } other => panic!(...) }` -- a different pattern that also variant-matches on the error type. While the match approach is arguably more precise, it is inconsistent with the established codebase pattern.
- Fix: Align with the existing convention:
```rust
let msg = format!("{}", result.unwrap_err());
assert!(
    msg.contains(&MAX_QUERY_BYTES.to_string()),
    "error message should contain max length: {msg}"
);
```

### LOW

**Missing `#[must_use]` on `QueryEngine::new`** - `crates/rskim-search/src/lexical/query.rs:40`
**Confidence**: 82%
- Problem: `SearchQuery::new` (the only other non-fallible `new` constructor returning `Self` in this crate) has `#[must_use]`. `QueryEngine::new` returns `Self` but omits the attribute.
- Fix: Add `#[must_use]` above the constructor:
```rust
#[must_use]
pub fn new(inner: Box<dyn SearchLayer>) -> Self {
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Module doc comment not updated for new re-exports** - `crates/rskim-search/src/lexical/mod.rs:1-13`
**Confidence**: 90%
- Problem: The module-level doc comment at the top of `mod.rs` enumerates the module's public exports: `BM25FConfig`, `classify_source`, `bm25f_score`, `dominant_field`. Lines 17 and 22 add `pub mod query` and `pub use query::{MAX_QUERY_BYTES, QueryEngine}` but the doc comment was not updated to mention these new items. This creates drift between the documented API surface and the actual exports.
- Fix: Add `QueryEngine` and `MAX_QUERY_BYTES` to the doc comment list:
```rust
//! This module exposes:
//! - [`QueryEngine`] — a [`SearchLayer`] decorator for query validation.
//! - [`MAX_QUERY_BYTES`] — upper bound on query text length.
//! - [`BM25FConfig`] — per-field boost and normalisation parameters.
//! - [`classify_source`] — map source byte ranges to [`crate::SearchField`] variants.
//! - [`bm25f_score`] — compute the BM25F score for a single query term.
//! - [`dominant_field`] — return the [`crate::SearchField`] with the highest TF.
```

## Pre-existing Issues (Not Blocking)

### MEDIUM

**Duplicated validation logic between QueryEngine and NgramIndexReader** - `crates/rskim-search/src/index/reader.rs:310-327` vs `crates/rskim-search/src/lexical/query.rs:47-59`
**Confidence**: 80%
- Problem: `NgramIndexReader::search` already checks `query.text.is_empty()` (returning `Ok(Vec::new())`) at line 310 and validates `bm25f_config` via `cfg.validate()?` at line 323. `QueryEngine::search` repeats both of these checks. While defense-in-depth is valid for a decorator pattern (the decorator cannot assume what inner layer it wraps), it does mean that when `QueryEngine` wraps `NgramIndexReader` specifically, every valid query pays two validation passes. This is worth documenting explicitly as intentional "defense-in-depth" to prevent a future maintainer from removing one thinking it is dead code.
- Fix: Add a brief comment in `QueryEngine::search` acknowledging the intentional overlap:
```rust
// Intentional defense-in-depth: the inner layer may also validate
// empty text and BM25F config, but we validate at the decorator
// boundary so the behaviour is independent of the inner layer.
```

## Suggestions (Lower Confidence)

- **Doc comment uses `vec![]` but implementation uses `Vec::new()`** - `crates/rskim-search/src/lexical/query.rs:5` (Confidence: 65%) -- The module doc says "short-circuit to `Ok(vec![])`" but the actual code at line 48 returns `Ok(Vec::new())`. Minor inconsistency in documentation style.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 3 | 1 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Consistency Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new `QueryEngine` module is structurally sound and follows the crate's core patterns well: Result-based error handling, `SearchLayer` trait implementation, kebab-case naming for `name()`, `Vec::new()` style, section divider use in source files, and alphabetically ordered re-exports. The conditions for approval are minor -- the test file deviates from the established conventions of its sibling test files in three ways (import style, section divider style, error assertion pattern), and the module doc comment needs updating for the new exports. None of these are blocking from a functionality standpoint, but aligning them would maintain the strong consistency this crate currently enjoys.
