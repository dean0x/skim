# Regression Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T15:31

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

### 1. Lost Functionality

**No removals detected.** Confidence: 95%

- Zero deleted files (`git diff --name-status` shows no `D` entries).
- Zero removed public exports (no lines matching `^-.*pub (fn|struct|enum|trait)` in the diff).
- Zero removed CLI options -- the existing flags (`--build`, `--rebuild`, `--update`, `--stats`, `--install-hooks`, `--remove-hooks`, `--json`, `-j`, `--limit`, `-n`, `--root`, `-h`, `--help`) are all preserved in `parse_flags`.
- The `print_help()` text has been extended (not replaced) with new temporal flags.

### 2. Broken Behavior -- Struct Field Additions

Two structs gained new fields. Both are additive-only with correct defaults:

**SearchQuery.file_filter** (library type, `rskim-search`):
- `#[serde(skip)]` -- not serialized, so JSON consumers are unaffected.
- `SearchQuery::new()` initializes it to `None`.
- All 3 struct-literal construction sites updated: `types.rs:723`, `search_api.rs:41`, `query.rs:64` (via `::new()`).
- Explicit test `test_search_query_new_file_filter_none` confirms the default.
- Explicit test `test_search_query_file_filter_skipped_in_serde` confirms it is not serialized.

**ResolvedResult.temporal** (CLI type, `rskim`):
- `#[serde(skip_serializing_if = "Option::is_none")]` -- omitted from JSON when `None`.
- All 8 struct-literal construction sites set `temporal: None`.
- The production code path in `resolve_paths_and_snippets()` sets `temporal: None`.
- Tests verify the JSON shape includes `temporal` when `Some` and omits it when `None`.

**QueryConfig.blast_radius_paths**:
- All 4 construction sites set `blast_radius_paths: None` (or the new `Some(...)` path).

**Verdict**: No struct literal site was missed. All consumers compile and pass tests.

### 3. Intent vs. Reality Mismatch

The PR claims "no breaking changes" -- this is verified:
- No public signatures changed.
- No existing return types altered.
- No default values modified.
- New fields have safe defaults (`None`).
- The new `file_filter` field applies a pre-LIMIT allowlist filter in `NgramIndexReader::search()`. When `None` (the default), the code path is identical to the previous behavior (`doc_scores.into_iter().collect()`).

### 4. Incomplete Migrations

No old API was deprecated or replaced, so no migration concern. The `SearchQuery` struct gained a field; all call sites (including the benchmark harness at `rskim-bench/src/harness.rs:147` which uses `SearchQuery::new()`) receive `file_filter: None` automatically.

### 5. Schema Migration

The `TemporalDb` schema version was bumped from 1 to 2. The migration:
- Adds 3 performance indexes (idempotent `CREATE INDEX IF NOT EXISTS`).
- Is forward-only (v1 databases auto-upgrade to v2 on open).
- Has a version guard: v2+ databases produce a clear error on older code.
- Dedicated test `v1_database_migrates_to_v2_on_reopen` validates the migration path.
- `schema_version_is_2` test updated to assert version 2.

### 6. BM25F Search Path -- file_filter Integration

The new `file_filter` is applied AFTER scoring but BEFORE sorting/limiting:
```rust
let mut scored: Vec<(u32, f64)> = if let Some(ref filter) = query.file_filter {
    doc_scores.into_iter()
        .filter(|(doc_id, _)| filter.contains(&FileId(*doc_id)))
        .collect()
} else {
    doc_scores.into_iter().collect()
};
```
This is correctly placed -- it filters the scored candidates before the limit is applied, ensuring that the limit applies to the filtered set. The `else` branch preserves the original behavior exactly.

### 7. Temporal Enrichment Sort Stability

`apply_temporal_enrichment` uses `sort_by` (stable sort) with a secondary comparator `.then_with(|| a.path.cmp(&b.path))` for deterministic ordering. Files absent from the temporal DB get score `-1.0`, which sorts them last in Hot mode and first in Cold mode. This is explicitly tested by `enrichment_missing_files_sort_last`.

### 8. Exit Code Preservation

The standalone temporal path (`run_temporal_standalone`) returns `ExitCode::SUCCESS` even when no temporal DB exists. This is verified by the test `test_standalone_temporal_no_db_returns_exit_0`. The combined path (`run_query`) also preserves the existing exit code semantics -- it always returns `ExitCode::SUCCESS` on valid query execution.

### 9. Test Coverage for Regression Vectors

All regression-relevant paths have explicit tests:
- Flag parsing mutual exclusion (6 tests).
- Blast-radius path normalization (6 tests covering relative, absolute, outside-repo, nonexistent, dot-slash).
- Standalone temporal dispatch for all 4 modes (hot, cold, risky, blast-radius).
- Combined enrichment for all 3 sort modes.
- Empty table handling for all standalone output formats.
- JSON shape validation for all 3 standalone modes + combined mode.
- Missing temporal DB graceful degradation (exit 0).
- Schema migration v1->v2.
