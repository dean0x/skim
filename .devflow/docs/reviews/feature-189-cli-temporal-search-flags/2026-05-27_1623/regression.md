# Regression Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T16:23

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Public struct `SearchQuery` gains required field without `Default` derive** - `crates/rskim-search/src/types.rs:368`
**Confidence**: 82%
- Problem: `SearchQuery` is a public type exported from `rskim-search` via `pub use`. The new `file_filter: Option<HashSet<FileId>>` field is added to a struct that does NOT derive `Default`. Any downstream consumer constructing `SearchQuery` via struct literal syntax (not via `SearchQuery::new()`) will get a compile error after upgrading the dependency. The PR description claims "Breaking changes: none" but this is technically a minor semver-breaking change for library consumers.
- Fix: This is acceptable if no external consumers use struct-literal construction (all internal callers have been updated, and the bench crate uses `SearchQuery::new()`). To be strictly non-breaking, add `#[non_exhaustive]` to `SearchQuery` (though this would itself be breaking if not already present). Alternatively, document the field addition in CHANGELOG as a minor API surface change. Since `SearchQuery::new()` initializes `file_filter: None`, most callers are unaffected.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none -- all findings are above threshold or not applicable)

## Regression Checklist

- [x] No exports removed without deprecation
- [x] Return types backward compatible
- [x] Default values unchanged (or documented) -- `SearchQuery::new()` initializes all new fields to `None`
- [x] Side effects preserved (events, logging)
- [x] All consumers of changed code updated -- 4 `ResolvedResult` struct literals updated with `temporal: None`, 1 `SearchQuery` struct literal updated with `file_filter: None`, 3 `QueryConfig` struct literals updated with `blast_radius_paths: None`, bench crate uses `SearchQuery::new()` so unaffected
- [x] Migration complete across codebase -- no incomplete migration detected
- [x] CLI options preserved or deprecated -- all existing flags remain, 4 new flags added (--hot, --cold, --risky, --blast-radius)
- [x] Commit message matches implementation -- verified across all 5 commits
- [x] Breaking changes documented -- minor struct field addition not documented in PR (see MEDIUM finding above)
- [x] Schema migration v1->v2 is additive only (adds indexes, no column changes) -- safe for existing databases
- [x] `run_query` signature change from individual args to `&Flags` is internal (`fn`, not `pub`) -- no external breakage
- [x] `format_text_output` format string change is additive (appends temporal_tag) -- empty string when no temporal annotation, zero regression for non-temporal queries
- [x] `execute_query` behavior unchanged when `blast_radius_paths: None` -- `file_filter` is `None`, no filtering applied

## Cross-Cycle Awareness

Prior resolution summary: Cycle 2 addressed 23 issues (19 fixed, 4 FP, 0 deferred). This cycle's review found no recurrence of previously-fixed patterns. The codebase shows evidence of prior fixes being well-maintained (e.g., the `sorted_paths()` hoist in `query.rs` addresses the duplicate-manifest-load pattern, pre-filter in BM25F first sub-pass addresses the blast-radius performance concern).

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

This is a well-executed additive feature PR with strong regression prevention:

1. **No functionality removed**: Zero removed exports, zero deleted files, zero removed CLI flags. All existing behavior is preserved.

2. **Struct field additions handled correctly**: All 4 `ResolvedResult` struct literals, all `QueryConfig` struct literals, and the integration test `SearchQuery` struct literal have been updated with the new fields initialized to `None`.

3. **Schema migration is safe**: The v1->v2 migration only adds indexes (`CREATE INDEX IF NOT EXISTS`), no column changes. The `IF NOT EXISTS` guard prevents re-migration failures. The test `v1_database_migrates_to_v2_on_reopen` validates the migration path.

4. **Format string changes are additive**: `format_text_output` appends `temporal_tag` which is an empty string when no temporal annotation is present, ensuring zero visual regression for non-temporal queries.

5. **Internal signature changes are non-breaking**: `run_query` changed from individual params to `&Flags` but is `fn` (private), not `pub`.

6. **Pre-filter applied before LIMIT**: The `file_filter` is applied inside the BM25F loop (first sub-pass) and again in the scored-results collection, ensuring the limit applies to filtered results. This prevents the subtle regression where blast-radius partners would be silently discarded by a top-N limit applied to unfiltered results.

7. **Graceful degradation**: Missing `temporal.db` returns exit 0 with a warning, not exit 1. Tested explicitly.

8. **Comprehensive test coverage**: 3,140 lines added with extensive tests covering all new paths including empty-table branches, mutual-exclusion errors, staleness detection, and JSON/text format validation.

The single MEDIUM finding (public struct field addition) is a technical semver concern but has no practical impact since all known consumers use `SearchQuery::new()`.
