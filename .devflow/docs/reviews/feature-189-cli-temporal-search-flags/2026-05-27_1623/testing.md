# Testing Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T16:23

## Issues in Your Changes (BLOCKING)

### HIGH

**Missing error-path test for `resolve_blast_radius_filter` when temporal DB is `None`** - `crates/rskim/src/cmd/search/mod.rs:424-430`
**Confidence**: 82%
- Problem: The `resolve_blast_radius_filter` function has a branch where `blast_radius` is `Some` but `temporal_db` is `None` -- it prints a warning and returns `Ok(None)`. This degradation path has no unit test. The function is only tested indirectly through the blast-radius query integration tests, which always provide a DB.
- Fix: Add a unit test that calls `resolve_blast_radius_filter(Some("src/auth.rs"), &None, &root)` and asserts it returns `Ok(None)` without panicking.

### MEDIUM

**`staleness_warns_when_stored_head_differs_from_current` depends on external `git` binary** - `crates/rskim/src/cmd/search/temporal_tests.rs:815-882`
**Confidence**: 85%
- Problem: This test spawns `git init`, `git add`, `git commit` as subprocess commands. It handles failure via early `return` with `eprintln!("SKIP ...")`, which means the test silently passes in environments without git (CI containers, restricted sandboxes). A silently skipped test provides no value and masks regressions. The `SKIP` pattern is used correctly (it does not panic), but it means the test has zero coverage guarantee in git-less environments.
- Fix: Consider using `#[ignore]` with a `--ignored` CI step that runs in a git-available environment, or document in the test that the SKIP is intentional and acceptable. At minimum, track skip counts in CI to detect if this test never actually runs.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **No negative/adversarial test for `file_filter` with empty HashSet** - `crates/rskim-search/src/index/reader.rs:362-366` (Confidence: 70%) -- When `file_filter` is `Some(empty_set)`, the search would score zero documents and return empty results. This edge case is not explicitly tested, though the behavior is implicitly correct.

- **`test_standalone_temporal_no_db_returns_exit_0` lacks assertion on output** - `crates/rskim/src/cmd/search/mod.rs` (Confidence: 65%) -- This test verifies exit code but does not assert that the warning message (JSON or text) was actually emitted. The test confirms the function does not panic but does not validate observable output behavior.

- **`cochange_partner_paths` not tested in isolation** - `crates/rskim/src/cmd/search/temporal.rs:187-195` (Confidence: 62%) -- The `cochange_partner_paths` helper is only tested transitively through `standalone_blast_radius_returns_partners` and `resolve_blast_radius_filter`. A direct unit test asserting the partner extraction logic (including the edge case where `target` matches `file_b` rather than `file_a`) would strengthen coverage.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Testing Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

The test suite is strong. Key observations:

**Strengths:**
- 33 new temporal_tests cover standalone queries (hot/cold/risky/blast-radius), combined enrichment with sort validation, flag parsing (including mutual exclusion errors, composability, and missing-value errors), output formatting (text and JSON) for all variants, path normalization edge cases, and staleness detection.
- 4 new query_tests cover temporal annotation rendering in both text and JSON format outputs, plus the blast-radius filter integration with `execute_query`.
- ~30 new storage_tests cover per-file lookups, top-N queries with sort order, limit clamping, UNION ALL bidirectional lookup, no-duplicate guarantee, usize::MAX overflow prevention, and schema migration from v1 to v2.
- Tests follow AAA structure consistently with clear assertion messages.
- Empty-table branches are tested for all three standalone modes (hot, cold, risky) and co-change.
- Regression tests are labeled clearly and document the specific bug they guard against.
- Test helpers (`temp_db`, `make_result`) are minimal and reused properly.
- All 49 temporal tests and 130 storage tests pass.

**Condition for approval:**
- The one HIGH finding (missing error-path test for `resolve_blast_radius_filter` with `None` DB) should be addressed. This is a graceful degradation path that users will encounter when they have not yet run `skim heatmap`, making it a critical user-facing behavior that deserves explicit test coverage.
