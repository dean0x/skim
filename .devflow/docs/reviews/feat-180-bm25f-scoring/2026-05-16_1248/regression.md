# Regression Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**sort_by replaces sort_unstable_by — minor performance regression** - `crates/rskim-search/src/index/reader.rs:336`
**Confidence**: 82%
- Problem: The scoring sort was changed from `sort_unstable_by` to `sort_by` to achieve deterministic ordering on equal-score ties. `sort_by` is a stable sort which performs more allocations (O(n) temporary space) compared to the in-place `sort_unstable_by`. For large result sets this introduces a measurable performance regression.
- Fix: The determinism goal is valid, but the same determinism can be achieved with `sort_unstable_by` since the FileId tie-breaking comparator already guarantees a total order (no two documents share the same FileId). Replace with:
  ```rust
  scored.sort_unstable_by(|a, b| {
      b.1.partial_cmp(&a.1)
          .unwrap_or(std::cmp::Ordering::Equal)
          .then_with(|| a.0.cmp(&b.0))
  });
  ```
  Stable sort is only needed when the comparator does NOT produce a total order and you need to preserve insertion order as a tiebreaker. Here, FileId explicitly breaks all ties.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Per-byte allocation in classifier** - `crates/rskim-search/src/lexical/classifier.rs:116` (Confidence: 65%) — `vec![SearchField::Other; len]` allocates one byte per source byte. For very large files (multi-MB), this could cause memory pressure. A chunked or range-based approach would be more efficient, but acceptable for typical source file sizes.

- **No backward-compatible v1 migration path** - `crates/rskim-search/src/index/format.rs:221-225` (Confidence: 70%) — v1 indexes are hard-rejected with an error. The PR description states this is intentional (pre-1.0 breaking change), so this is likely acceptable. However, if any downstream tooling already persists v1 indexes, users will see "unsupported format version" with no automatic rebuild.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Rationale

This PR introduces a well-structured BM25F scoring engine that replaces flat BM25. From a regression perspective:

1. **No removed public exports** — All existing public API items are preserved. New items are additive (`lexical` module, `BM25FConfig`, `classify_source`, `dominant_field`, `SearchField::ALL`, `SearchField::count()`, `SearchQuery::bm25f_config`, `NgramIndexReader::open_with_config`).

2. **Backward-compatible `add_file()`** — The `LayerBuilder::add_file()` trait method delegates to `add_file_classified()` with an empty field map, so all existing callers produce identical behavior (all bytes classified as `Other`).

3. **Format version bump handled correctly** — v1 indexes are rejected with a clear error message containing "format version" and "please rebuild the index". This is the correct approach for a pre-1.0 library.

4. **Scoring behavior preserved for unclassified files** — When `field_map` is empty (the backward-compat path), all bytes go to `SearchField::Other` (discriminant 7, default boost 1.0, b=0.75). The BM25F formula with a single field and boost=1.0 produces results equivalent to flat BM25 with the same k1 and b parameters.

5. **Deterministic ordering** — The `sort_by` with FileId tie-breaking ensures reproducible results (tested by AC4 with 100 iterations). The only concern is the unnecessary use of stable sort when the comparator already provides a total order.

6. **All 198 rskim-search tests pass** (197 + 1 skipped), and 3091 CLI tests pass. The old BM25 ranking test (`test_bm25_short_dense_ranks_above_long_sparse`) continues to pass, confirming backward-compatible scoring behavior.

7. **Old `bm25_score` function correctly scoped** — The original `bm25_score` in `format.rs` is gated behind `#[cfg(test)]` along with its constants `BM25_K1` and `BM25_B`, keeping the format test suite working while removing the dead production code path.

The single MEDIUM-severity finding (`sort_by` vs `sort_unstable_by`) is a minor performance concern, not a correctness regression. Approve with the recommendation to switch back to `sort_unstable_by` for consistency with the project's performance-critical design principles.
