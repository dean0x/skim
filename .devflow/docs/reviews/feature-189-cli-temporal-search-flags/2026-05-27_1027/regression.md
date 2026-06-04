# Regression Review Report

**Branch**: feature-189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T10:27

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Blast-radius path normalization silently degrades on error** - `crates/rskim/src/cmd/search/mod.rs:429-431`
**Confidence**: 82%
- Problem: When `normalize_blast_radius_path` fails (e.g., file not found, outside repo), the error is logged to stderr but `run_query` continues with unfiltered results. The user explicitly requested `--blast-radius FILE` filtering and will receive unfiltered BM25F results without any indication in stdout that filtering was skipped. In JSON mode, the output contains no field indicating the blast-radius filter was not applied.
- Fix: Either propagate the error (return early with exit code 1), or add a `"blast_radius_applied": false` field to the JSON output so programmatic consumers can detect the degradation. The stderr warning alone is insufficient for piped/agent workflows where stderr may be discarded.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Blast-radius filter excludes the source file itself** - `crates/rskim/src/cmd/search/mod.rs:417-426` (Confidence: 65%) -- The `--blast-radius FILE` allowlist collects only co-change partners of FILE, not FILE itself. If FILE matches the text query, it will be excluded from results. This may be intentional ("show me what else gets affected") but could surprise users who expect FILE to appear in its own blast radius. Consider documenting this behavior in the help text.

- **`output.total = output.results.len()` is a no-op after enrichment** - `crates/rskim/src/cmd/search/mod.rs:451` (Confidence: 72%) -- `apply_temporal_enrichment` re-sorts but does not add or remove elements, so `output.results.len()` is unchanged from what `execute_query` already set in `output.total`. The assignment is harmless but suggests a stale assumption that enrichment might filter results.

- **`run_query` clones `root` and `cache_dir` only when temporal flags are active paths** - `crates/rskim/src/cmd/search/mod.rs:441-442` (Confidence: 60%) -- The `.clone()` on `root` and `cache_dir` was added to satisfy the borrow checker (they are used after being moved into `QueryConfig`). When no temporal flags are present, these clones are wasted. Could be avoided by restructuring to borrow rather than move, though the performance impact is negligible.

## Regression Checklist

| Check | Status | Notes |
|-------|--------|-------|
| No exports removed | PASS | No public API removals detected |
| Return types backward compatible | PASS | `SearchQuery`, `ResolvedResult`, `QueryConfig` all have additive-only changes with `None` defaults |
| Default values unchanged | PASS | All new fields default to `None`; existing defaults preserved |
| Side effects preserved | PASS | Existing search behavior unchanged when no temporal flags provided |
| All consumers of changed code updated | PASS | All `SearchQuery {}` literals updated with `file_filter: None`; all `ResolvedResult {}` literals updated with `temporal: None`; `QueryConfig {}` updated with `blast_radius_paths: None` |
| Migration complete across codebase | PASS | Grep confirms all struct literal sites updated (3 `SearchQuery`, 10+ `ResolvedResult`, 2 `QueryConfig`) |
| CLI options preserved | PASS | All existing flags (`--build`, `--rebuild`, `--update`, `--stats`, `--json`, `--limit`, `--root`, `--install-hooks`, `--remove-hooks`) preserved; new flags are purely additive |
| Commit message matches implementation | PASS | Commit message accurately describes temporal CLI flags, file_filter, schema v2 migration |
| Breaking changes documented | PASS | PR description notes "older skim refuses v2 temporal.db"; forward-compat guard in `run_migrations` rejects future versions with clear error |
| Schema migration tested | PASS | `v1_database_migrates_to_v2_on_reopen` test creates a v1 DB manually and verifies upgrade |
| `is_flag_with_value` synced | PASS | `--blast-radius` added to `main.rs:is_flag_with_value` so its value is not misinterpreted as a subcommand |
| Graceful degradation when temporal.db missing | PASS | Both standalone and combined paths handle missing DB with warnings and exit 0; tested by `test_standalone_temporal_no_db_returns_exit_0` |
| Serde compatibility preserved | PASS | `file_filter` is `#[serde(skip)]`; `temporal` on `ResolvedResult` is `#[serde(skip_serializing_if = "Option::is_none")]`; existing JSON consumers see identical output when no temporal flags used |
| Mutually exclusive flag validation | PASS | `--hot`, `--cold`, `--risky` are validated as mutually exclusive with clear error messages; tested by `parse_hot_cold_conflict_error` and `parse_hot_risky_conflict_error` |
| Unrecognised flag error message updated | PASS | Error message now lists the new flags in the valid-flags enumeration |

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The changes are additive and well-structured with no regressions to existing functionality. All struct literal sites have been updated, schema migration is forward-compatible with a clear rejection guard, serde output is unchanged for non-temporal queries, and the test suite passes fully (315+ tests across the affected packages).

The single MEDIUM finding (silent degradation on blast-radius path errors) is a usability concern for programmatic consumers rather than a correctness regression. The condition is: approve if the team accepts the current stderr-only degradation behavior for `--blast-radius` path errors, or address it before merge.
