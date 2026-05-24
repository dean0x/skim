# Consistency Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Inconsistent Path type import in `open_with_config`** - `crates/rskim-search/src/index/reader.rs:153`
**Confidence**: 92%
- Problem: `open` uses `dir: &Path` (with `use std::path::Path` at line 25), while `open_with_config` uses `dir: &std::path::Path` (fully qualified). These are functionally identical but stylistically inconsistent within the same impl block.
- Fix:
```rust
pub fn open_with_config(dir: &Path, config: BM25FConfig) -> Result<Self> {
```

**Magic number `8` used instead of `FIELD_COUNT` constant in builder and format modules (12 occurrences)** - Confidence: 85%
- `crates/rskim-search/src/index/builder.rs:51`, `:74`, `:187`, `:188`, `:259`, `:263`
- `crates/rskim-search/src/index/format.rs:93`, `:156`, `:229`, `:337`
- Problem: The `FIELD_COUNT` constant exists in `lexical::config` and is exported publicly, yet `builder.rs` and `format.rs` use raw `8` literals for array declarations (e.g., `[u64; 8]`, `[0u32; 8]`, `[f32; 8]`). The `reader.rs` and `scoring.rs` modules correctly use `FIELD_COUNT`. This inconsistency means if the field count ever changes, these modules would silently break.
- Fix: Import `FIELD_COUNT` in `builder.rs` and `format.rs` and use it for all array types:
```rust
// builder.rs — add to imports:
use crate::lexical::FIELD_COUNT;
// Then replace [u64; 8] with [u64; FIELD_COUNT], etc.

// format.rs — add to imports:
use super::super::lexical::config::FIELD_COUNT;
// Or accept the magic number here since format.rs is the binary layout definition
// and sizes are fixed as part of the on-disk format spec.
```
Note: This is debatable for `format.rs` where the literal `8` documents the exact on-disk byte layout, but `builder.rs` should use the constant for internal logic.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`SearchField::count()` returns hardcoded `8` instead of referencing `FIELD_COUNT` or `ALL.len()`** - `crates/rskim-search/src/types.rs:98-100`
**Confidence**: 82%
- Problem: Three sources of truth for the number of fields exist: `SearchField::count()` (hardcoded `8`), `SearchField::ALL.len()` (compile-time array of 8), and `FIELD_COUNT` constant (hardcoded `8`). The doc comment on `count()` says "Must equal `FIELD_COUNT`" but this is not enforced at compile time. If a field is added, developers must update all three locations manually.
- Fix: Consider a compile-time assertion to bind them:
```rust
pub const fn count() -> usize {
    Self::ALL.len()
}
// In config.rs or a shared location, add:
const _: () = assert!(FIELD_COUNT == SearchField::ALL.len());
```

**`sort_by` used instead of `sort_unstable_by` for scored results** - `crates/rskim-search/src/index/reader.rs:336`
**Confidence**: 80%
- Problem: The builder uses `sort_unstable` and `sort_unstable_by` for its sorting operations (lines 272, 277). The reader previously used `sort_unstable_by` (visible in the diff removal at line 301 of the old code). The new code switches to `sort_by` with a tie-breaking comparator. While the tie-breaking logic justifies stability (to preserve deterministic ordering on equal scores), `sort_unstable_by` would also be deterministic here since the tie-breaker already resolves all ambiguity. The pattern change is minor but inconsistent with the rest of the codebase's preference for `sort_unstable_*`.
- Fix:
```rust
scored.sort_unstable_by(|a, b| {
    b.1.partial_cmp(&a.1)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.0.cmp(&b.0))
});
```

## Pre-existing Issues (Not Blocking)

No critical pre-existing issues found.

## Suggestions (Lower Confidence)

- **`pub(crate) file_count` visibility inconsistency** - `crates/rskim-search/src/index/builder.rs:47` (Confidence: 65%) -- All other fields on `NgramIndexBuilder` are private. `file_count` was changed to `pub(crate)` but no crate-internal code outside the struct's impl blocks appears to need direct field access. This may be test-driven; if so, consider adding a `pub(crate) fn file_count(&self) -> u32` accessor instead for encapsulation consistency.

- **Module-level doc comment on `reader.rs` still says "BM25 query layer"** - `crates/rskim-search/src/index/reader.rs:0` (Confidence: 72%) -- The module-level doc comment says "mmap'd BM25 query layer" but the implementation now uses BM25F exclusively. The comment in the search method was updated but the top-of-file doc was not.

- **Inconsistent error type usage for validation** - `crates/rskim-search/src/lexical/config.rs:56` (Confidence: 62%) -- `BM25FConfig::validate()` returns `SearchError::InvalidQuery` for configuration validation failures. Semantically these are not invalid queries but invalid configurations. This is a naming/semantic concern, not a bug.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new `lexical/` module is well-structured and follows existing codebase patterns: Result types throughout, `#[must_use]` annotations on pure functions, separated test files via `#[path = "..."]`, comprehensive doc comments with `# Errors` sections, and consistent error handling via `SearchError`. The module organization (config, classifier, scoring with a `mod.rs` re-exporting) mirrors the existing `index/` and `temporal/` module patterns. The main consistency concerns are the magic number `8` vs `FIELD_COUNT` in format/builder files and a minor path type inconsistency.
