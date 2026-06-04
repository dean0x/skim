# Testing Review Report

**Branch**: feature-189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T10:27

## Issues in Your Changes (BLOCKING)

### HIGH

**`set_current_dir` in tests creates process-wide flakiness risk (2 occurrences)** -- Confidence: 85%
- `crates/rskim/src/cmd/search/temporal_tests.rs:55`, `crates/rskim/src/cmd/search/temporal_tests.rs:131`
- Problem: `normalize_relative_path` and `normalize_dot_slash_stripped` call `std::env::set_current_dir()` which mutates the global process CWD. When `cargo test` runs tests in parallel (default behaviour), any other test that is sensitive to CWD can observe the changed directory and fail non-deterministically. This is a documented flaky-test anti-pattern.
- Fix: Refactor `normalize_blast_radius_path` to accept an optional `cwd` parameter instead of relying on `std::env::current_dir()`, so tests can inject a controlled CWD without mutating global state. Alternatively, isolate these two tests in a serial test group with `#[serial]` from the `serial_test` crate. At minimum, add a comment explaining the risk and a `// SAFETY:` rationale for why this is acceptable in this test binary.

### MEDIUM

**Standalone temporal empty-table paths untested for `--cold` and `--risky`** -- Confidence: 82%
- `crates/rskim/src/cmd/search/temporal_tests.rs` (gap)
- Problem: `top_hotspots_empty_table_returns_empty` and `top_risks_empty_returns_empty` exist in `storage_tests.rs` (DB layer), but there are no tests that verify `query_standalone(Some(TemporalSort::Cold), None, 10, &db, &root)` and `query_standalone(Some(TemporalSort::Risky), None, 10, &db, &root)` return graceful empty results at the CLI dispatch layer. The `format_temporal_text` formatter has distinct "No coldspot data available." and "No risk data available." message branches that are never exercised.
- Fix: Add two tests: one calling `query_standalone` + `format_temporal_text` with `TemporalSort::Cold` on an empty DB, and one with `TemporalSort::Risky` on an empty DB, asserting the empty-data messages appear.

**No test for `check_temporal_staleness` when data IS stale** -- Confidence: 83%
- `crates/rskim/src/cmd/search/temporal_tests.rs:153` (`staleness_returns_none_when_current`)
- Problem: The only staleness test checks the "no meta key" path (returns `None`). There is no test for the actual stale case where `stored_head != current_head`, which is the primary purpose of the function. The stale branch generates a specific warning message with truncated SHAs that is never validated.
- Fix: Add a test that sets a known `META_GIT_HEAD` value in the DB, then creates a mock git repo (or points at a temp dir that is not a git repo, yielding `None` for current HEAD which also exercises the `None` early-return). Better: create a real git repo in a TempDir (`git init` + `git commit --allow-empty`), set a different HEAD in the DB, and assert the warning message contains "temporal data is stale".

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`temporal_annotation_tag` helper has no unit tests** -- Confidence: 80%
- `crates/rskim/src/cmd/search/query.rs:154`
- Problem: The `temporal_annotation_tag` function contains branching logic (None, hotspot-only, risk-only, both) and formatting rules (3 decimal places, double-space separators). The function is tested indirectly through `format_text_output` integration tests, but the branch for "both hotspot and risk present" is never directly exercised. The integration tests cover hotspot-only and risk-only but not the combined case.
- Fix: Add a direct unit test for `temporal_annotation_tag` with all four cases: `None`, hotspot-only, risk-only, and both. Or add a `format_text_output` test with both annotations populated and assert the output contains both "hotspot:" and "risk:" on the same line.

**No tests for standalone temporal JSON formatters beyond `--hot`** -- Confidence: 80%
- `crates/rskim/src/cmd/search/temporal_tests.rs:390` (`standalone_hot_json_valid`)
- Problem: `format_temporal_json` has three distinct match arms (Hotspots/Coldspots, Risks, Cochanges) each producing different JSON shapes. Only the `Hotspots` arm is tested via `standalone_hot_json_valid`. The Risk JSON (`"mode": "risky"`) and Cochanges JSON (`"mode": "blast_radius"`) branches are never validated for correct JSON structure.
- Fix: Add `standalone_risky_json_valid` and `standalone_blast_radius_json_valid` tests mirroring the existing `standalone_hot_json_valid` pattern.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`enrichment_cold_sorts_by_hotspot_asc` test uses equal BM25F scores** - `crates/rskim/src/cmd/search/temporal_tests.rs:500` (Confidence: 65%) -- Both results have `score: 10.0`, so the test passes even if the sort incorrectly uses BM25F score as a tiebreaker. Using different BM25F scores would make this a stronger assertion.

- **No negative test for `--blast-radius` combined with action flags** - `crates/rskim/src/cmd/search/temporal_tests.rs` (Confidence: 65%) -- What happens if a user passes `--blast-radius src/foo.rs --build`? The current parse_flags would set both `action_flag` and `blast_radius`, and `run()` dispatches to `run_build` ignoring the blast-radius. A test documenting this behavior (or rejecting it) would clarify intent.

- **`cochange_partner` helper function has no direct unit test** - `crates/rskim/src/cmd/search/temporal.rs:168` (Confidence: 62%) -- It is exercised transitively through integration tests, but a direct test would guard against regressions if the canonical ordering convention changes.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The test suite is substantial (~67 new tests) and covers the primary happy paths well: per-file lookups, top-N queries, schema migration, flag parsing, mutual exclusion, standalone dispatch, combined enrichment, and output formatting. The AAA structure is clean throughout, setup is concise via `temp_db()` and `make_result()` helpers, and assertions test behavior rather than implementation details.

The CHANGES_REQUESTED is driven by one HIGH issue: `set_current_dir` mutations in two tests introduce a process-wide flakiness vector. The remaining MEDIUM findings are coverage gaps in empty-table edge cases, staleness detection, combined annotation formatting, and non-hot JSON formatters -- these are worth addressing for completeness but are not blockers on their own.
