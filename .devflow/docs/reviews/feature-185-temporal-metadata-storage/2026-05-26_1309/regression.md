# Regression Review Report

**Branch**: feature/185-temporal-metadata-storage -> main
**Date**: 2026-05-26T13:09

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED

## Detailed Analysis

### 1. Lost Functionality Check

**No exports removed.** All pre-existing public API items are preserved:

- `lib.rs` re-exports: Every symbol from the `main` branch (`DEFAULT_HALF_LIFE_DAYS`, `GixSource`, `compute_file_risk_scores`, `decay_weight`, `is_fix_commit`, `CochangeMatrixBuilder`, `CochangeMatrixReader`, and all 22 types re-exports) remains in the current branch. The diff only **adds** new exports (`compute_file_temporal_stats`, `FileTemporalStats`, `TemporalDb`, `HotspotRow`, `RiskRow`, `CochangeRow`, `META_GIT_HEAD`, `META_LAST_UPDATED`).
- `temporal/mod.rs` re-exports: `DEFAULT_HALF_LIFE_DAYS`, `compute_file_risk_scores`, `decay_weight` all preserved; `compute_file_temporal_stats` added.
- No files were deleted in this PR.
- No CLI options were removed or changed.

### 2. Broken Behavior Check

**No existing function signatures were modified.** The changes are additive:

- `scoring.rs`: Existing functions `compute_file_risk_scores` and `decay_weight` are untouched. Only module-level doc comment was extended and a new function `compute_file_temporal_stats` was appended.
- `types.rs`: Existing `SearchError` variants are unchanged. New `Database(String)` variant was added. `#[non_exhaustive]` was added (noted as already fixed in prior resolution cycle 1). New struct `FileTemporalStats` was added after `FileRiskScores`.
- `scoring_tests.rs`: Only the import line was modified (removed unused `DEFAULT_HALF_LIFE_DAYS` import, added `compute_file_temporal_stats`). All existing test functions remain untouched; new tests were appended after line 627.
- `temporal/mod.rs`: `pub mod storage;` added. Existing `pub use` statement expanded from single-line to multi-line form preserving all original items.

### 3. Intent vs Reality Mismatch Check

**PR description matches implementation.** The PR states:
- "Introduces a SQLite persistence layer" -- confirmed: `TemporalDb` with `storage.rs`, `storage_ops.rs`, `storage_types.rs`.
- "SearchError gains a new Database(String) variant" -- confirmed: added at `types.rs:607`.
- "No exhaustive matches exist in the codebase -- verified safe" -- confirmed: all `SearchError` matches use `matches!()` macro with specific variant patterns, never exhaustive `match`.

### 4. Incomplete Migration Check

**Not applicable.** This PR introduces new API surface; it does not deprecate or replace any existing API. No migration is needed.

### 5. Downstream Consumer Compatibility

**Integration tests pass.** The `crates/rskim/tests/search_api.rs` integration test suite (11 tests) passes without modification, confirming that the downstream binary crate can still construct and use all `SearchError` variants and other `rskim_search` types without breakage.

### 6. Test Suite Verification

- `rskim-search` package: 436 pass, 0 fail, 3 skip
- `rskim` search_api integration: 11 pass, 0 fail
- No existing tests were modified in ways that weaken coverage
- New tests comprehensively cover `compute_file_temporal_stats` (12 tests) and `TemporalDb` CRUD/sync/perf/error (25+ tests)

### 7. Type Width Mismatch (Informational)

`FileTemporalStats` uses `u32` fields while `HotspotRow`/`RiskRow` use `i64` (SQLite INTEGER). This is a deliberate design choice documented in the KNOWLEDGE.md gotchas section. Callers bridging between the two representations must cast carefully, but this is not a regression -- it is a new API boundary with documented constraints. The `u32` fields use `saturating_add` to prevent overflow.

### Cross-Cycle Awareness

The prior resolution cycle fixed 13 issues including `#[non_exhaustive]` on `SearchError`. This attribute is confirmed present in the current branch at `types.rs:561`. No issues from cycle 1 are re-raised.
