# Regression Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`file_count` visibility widened from private to `pub(crate)`** - `crates/rskim-search/src/index/builder.rs:47` (Confidence: 65%) -- The field `file_count` was private and is now `pub(crate)`. While this cannot break external consumers (it is not `pub`), it expands the internal surface area without clear justification in the diff. If a test or sibling module needed this, prefer an accessor method to preserve encapsulation.

- **Old `bm25_score` function gated behind `#[cfg(test)]`** - `crates/rskim-search/src/index/format.rs:410-412` (Confidence: 70%) -- The original `bm25_score` function (and its constants `BM25_K1`, `BM25_B`) were demoted to `#[cfg(test)]` since they are no longer used by the reader. If a downstream consumer in the workspace previously depended on calling `bm25_score` at runtime (e.g., through `pub(crate)` reachability), this would silently compile-fail. No such consumer exists in the current codebase, so this is informational.

- **Format v1 to v2 migration has no automatic upgrade path** - `crates/rskim-search/src/index/format.rs:221-224` (Confidence: 75%) -- The error message "please rebuild the index" is clear, but there is no programmatic migration path or CLI command to rebuild. Users with existing v1 indexes will hit this error at runtime. This is documented as intentional in the PR description (pre-1.0 clean break), so it is not blocking, but a future CLI subcommand (e.g., `skim search reindex`) would reduce friction.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED

## Detailed Analysis

### Format Breaking Change (Intentional, Well-Handled)

The v1-to-v2 format bump is the primary regression risk in this PR. The review confirms it is handled correctly:

1. **`FORMAT_VERSION` incremented**: `1 -> 2` in `format.rs:43`.
2. **`decode_header` rejects v1 cleanly**: The error message explicitly says "format version" and "please rebuild the index", giving users a clear action path.
3. **Test coverage for v1 rejection**: `test_v1_header_rejected_with_format_version_message` validates that a 30-byte v1 header is rejected.
4. **Header size constant updated**: `SKIDX_HEADER_SIZE` changed from 30 to 62 bytes.
5. **File meta size constant updated**: `FILE_META_SIZE` changed from 5 to 37 bytes.
6. **CRC32 checksum still covers entries + file metadata**: The checksum validation in the reader is unchanged in structure, just uses the new sizes.

### API Backward Compatibility (Preserved)

1. **`LayerBuilder::add_file` trait method preserved**: The original `add_file` method is retained as a delegate to `add_file_classified` with an empty field map. All existing callers (`LayerBuilder::add_file`) continue to work unchanged with `SearchField::Other` classification.
2. **`SearchQuery::new` unchanged**: The new `bm25f_config` field defaults to `None` in the constructor. Existing callers are unaffected.
3. **`SearchQuery` serde compatibility**: The new `bm25f_config` field uses `#[serde(default, skip_serializing_if = "Option::is_none")]`, so JSON without the field deserializes correctly and JSON with `None` omits it.
4. **`SearchResult.field` enriched from `Other`**: Previously hardcoded to `SearchField::Other`, now populated by `dominant_field()`. This is a behavior improvement, not a regression -- consumers that ignored the field are unaffected, and consumers that used it now get richer data.
5. **No removed public exports**: All existing public types and functions remain exported from `crates/rskim-search/src/lib.rs`.

### Scoring Algorithm Change (BM25 to BM25F)

The scoring algorithm switched from flat BM25 to BM25F (field-weighted). This changes score values for all queries:

1. **Score magnitudes will differ**: The BM25F formula uses per-field TF normalization and field boosts instead of a single document-level TF. Score values from the same query on the same data will be numerically different.
2. **Ranking may change**: Files previously ranked by flat BM25 may be reordered. This is the stated intent of the PR.
3. **Determinism preserved**: `test_ac4_scoring_deterministic` validates 100 identical searches produce identical results. Sort order now includes FileId tie-breaking for total ordering.
4. **Default config is sensible**: `BM25FConfig::default()` provides field boosts that prioritize structural elements (TypeDefinition=5.0) over implementation details (StringLiteral=0.5), which aligns with the PR's stated goal.
5. **Backward-compatible unclassified indexing**: When `add_file` is used (no field map), all bytes are `SearchField::Other` with boost 1.0, so scoring degrades gracefully to a field-unaware approximation.

### Test Coverage Assessment

The PR adds extensive test coverage for the new functionality:

- 7 new reader_tests (AC1-AC4, open_with_config, field population, validation)
- 12 classifier_tests (empty, size limit, non-tree-sitter, Rust, Python, TypeScript, invariants)
- 13 config_tests (defaults, validation, serde, edge cases)
- 12 scoring_tests (zero TF, boost effects, edge cases, determinism, dominant_field)
- 5 format_tests (v1 rejection, header/meta size, field_lengths roundtrip, sum validation)

All 3811 tests pass (up from 3558 on main), with 0 failures.
