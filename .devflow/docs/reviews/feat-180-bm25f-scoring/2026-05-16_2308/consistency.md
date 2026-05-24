# Consistency Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`FieldClassifier`/`NodeInfo` trait exists but `classify_source` bypasses it** - `crates/rskim-search/src/lexical/classifier.rs`
**Confidence**: 82%
- Problem: The codebase defines a `FieldClassifier` trait (types.rs:438) and `NodeInfo` struct (types.rs:416) explicitly designed to decouple field classification from tree-sitter. The doc comment on `NodeInfo` states it "captures exactly what `FieldClassifier` needs" and exists so "non-tree-sitter languages can implement `FieldClassifier` without depending on the tree-sitter crate." However, the new `classify_source()` function directly uses `rskim_core::Parser` and `tree_sitter::TreeCursor`, bypassing this abstraction entirely. This creates two parallel classification approaches: the trait-based `FieldClassifier::classify(NodeInfo)` and the concrete `classify_source(source, Language)`.
- Fix: Either (a) implement `classify_source` in terms of `FieldClassifier`/`NodeInfo`, walking the tree and converting each node to `NodeInfo` before classifying, or (b) document in `types.rs` that `FieldClassifier`/`NodeInfo` is a future extensibility point not yet used by the built-in classifier, and add a comment in `classifier.rs` explaining why the direct approach was chosen (performance: avoids per-node `NodeInfo` allocation). Option (b) is the lighter fix and likely the correct one given the per-byte array already dominates allocation.

### MEDIUM

(none)

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`file_count` visibility widened from private to `pub(crate)` without documented justification** - `crates/rskim-search/src/index/builder.rs:47`
**Confidence**: 80%
- Problem: All other fields of `NgramIndexBuilder` remain private. The `file_count` field was widened to `pub(crate)` but no code outside the module reads it directly (no `builder.file_count` references found). The existing pattern in this struct keeps all fields private and exposes behaviour through methods.
- Fix: If `file_count` is needed by tests or other modules, add a `pub(crate) fn file_count(&self) -> u32` accessor method to match the private-field pattern. If nothing reads it, revert to private.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Dead `bm25_score` and BM25 constants kept in `format.rs`** - `crates/rskim-search/src/index/format.rs:56-61,410-426` (Confidence: 70%) -- The old `bm25_score`, `BM25_K1`, and `BM25_B` are gated with `#[cfg(test)]` but are vestigial now that `bm25f_score` is the production scorer. Existing tests in `format_tests.rs` still exercise them, but consider removing the old function and migrating those tests to `bm25f_score` in a follow-up to avoid two scoring implementations coexisting.

- **`idf_for_key` returns `f32` but all callers immediately convert to `f64`** - `crates/rskim-search/src/index/reader.rs:291` (Confidence: 65%) -- The callsite wraps every use in `f64::from(idf_for_key(...))`. If the only consumer needs `f64`, the function's return type could be changed for consistency with the rest of the scoring pipeline, which operates in `f64` throughout.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

---

## Detailed Consistency Assessment

### Positive Patterns (What This PR Does Well)

1. **Module/file organization**: The new `lexical/` module follows the exact same pattern as `index/` -- separate files for logic and tests (`classifier.rs`/`classifier_tests.rs`, `config.rs`/`config_tests.rs`, `scoring.rs`/`scoring_tests.rs`), with a `mod.rs` re-exporting public API. Test modules use `#[path = "..."]` attribute consistently.

2. **Error handling**: All new functions return `Result<T>` using the crate-level `SearchError` enum. The new `FileTooLarge` variant follows the existing structured error pattern (named fields, `#[error(...)]` attribute, `thiserror`). Validation errors use `SearchError::InvalidQuery` consistently with the rest of the codebase.

3. **`#[must_use]` annotations**: Applied consistently on all pure functions (`bm25f_score`, `dominant_field`, `node_kind_priority`, `SearchField::count()`), matching the existing pattern.

4. **Single source of truth for FIELD_COUNT**: `FIELD_COUNT` is derived from `SearchField::count()` which derives from `SearchField::ALL.len()`. A compile-time assertion (`const _: () = assert!(...)`) prevents drift. This is excellent defensive design.

5. **Doc comments**: All new public functions have `///` doc comments with `# Errors` sections matching the existing style. The format module comments were updated to reflect the v2 layout (62-byte header, 37-byte FileMetaEntry) with byte-offset tables.

6. **Format versioning**: Clean v1 to v2 transition with version bump, size constant updates, and clear rejection of v1 indexes with a human-readable error message containing "format version".

7. **Determinism**: Sort order includes `FileId` tie-breaking (reader.rs:348-352), and `dominant_field` resolves ties by lowest discriminant. Both are documented.

8. **Boundary validation**: `BM25FConfig::validate()` is called at both trust boundaries -- `open_with_config()` and per-query override in `search()`. The `classify_source` function validates `MAX_SOURCE_BYTES` before allocating the per-byte array. This matches the project's "validate at boundaries" principle.

9. **Naming conventions**: All new types follow existing patterns -- `BM25FConfig` (PascalCase struct), `bm25f_score` (snake_case function), `FIELD_COUNT` (SCREAMING_SNAKE_CASE const), `classify_source` (verb_noun function name).

10. **Serde integration**: `BM25FConfig` derives `Serialize`/`Deserialize` and is added to `SearchQuery` with `#[serde(default, skip_serializing_if = "Option::is_none")]`, matching the existing `temporal_flags` pattern on the same struct.
