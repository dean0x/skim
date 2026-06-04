# Testing Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29

## Cross-Cycle Awareness

Cycle 2 raised 4 testing findings. All 4 have been addressed in subsequent commits:

1. Silent test skip in `full_pipeline_synthetic_repo` -- converted to `assert!` + `panic!` with descriptive messages. VERIFIED FIXED.
2. Missing `evaluate_at_thresholds` empty-commit tests -- two dedicated unit tests added (`zero_metrics_when_all_commits_unmappable`, `zero_metrics_for_single_file_commits`). VERIFIED FIXED.
3. Missing `aggregate_metrics` error-branch test -- `aggregate_metrics_skips_errored_passing_repos` added. VERIFIED FIXED.
4. Weak assertion in `quality_gate_rejects_short_history` -- now asserts error message content. VERIFIED FIXED.

No regressions from prior-cycle fixes detected.

## Issues in Your Changes (BLOCKING)

(none)

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`repo_section` error-rendering path is untested in report.rs** - `crates/rskim-bench/src/cochange/report.rs:181-183`
**Confidence**: 80%
- Problem: The `repo_section` function has a branch at line 181 that renders `**Error:** {err}` when `repo.error` is `Some(...)`. This branch is never exercised by any test. The existing `sample_result()` helper includes a failing repo with `quality_gate_reason: Some(...)` but no `error` field, so only the `quality_gate_passed == false` rendering path is tested. The `report::tests::markdown_failed_repo_shows_reason` test covers the quality-gate-failure case but not the error case.
- Fix: Add a test that constructs a repo with `error: Some("clone failed: ...")` and verifies the markdown output contains `**Error:**`:
```rust
#[test]
fn markdown_errored_repo_shows_error_message() {
    let mut result = sample_result();
    result.repos.push(RepoCochangeResult {
        repo_name: "errored-repo".to_string(),
        error: Some("clone failed: timeout".to_string()),
        ..Default::default()
    });
    let md = to_markdown(&result);
    assert!(md.contains("**Error:** clone failed: timeout"));
}
```
Applies ADR-001 -- fix noticed issue immediately rather than deferring.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **No test for `MAX_FILES_FOR_EVALUATION` or `MAX_TEST_COMMITS` guard paths** - `crates/rskim-bench/src/cochange/validate.rs:214-229` (Confidence: 68%) -- The capacity guards that bail when file count exceeds 20,000 or test commit count exceeds 50,000 have no unit tests. These would require constructing very large path maps, but a targeted test could verify the bail path fires at the boundary.

- **`clone_with_history` and `git_output_with_timeout` have no direct unit tests** - `crates/rskim-research/src/clone.rs:128-169,348-388` (Confidence: 65%) -- These new public functions are exercised indirectly through `full_pipeline_synthetic_repo` and `validate_repo`, but have no unit-level tests for their error paths (e.g., non-HTTPS URL rejection in `clone_with_history`, timeout behavior in `git_output_with_timeout`). The HTTPS rejection is a security boundary worth testing directly.

- **`full_pipeline_synthetic_repo` discards the `_unmapped` count without asserting** - `crates/rskim-bench/tests/cochange_validation.rs:394` (Confidence: 62%) -- The test assigns `_unmapped` without verification. Since the test commit introduces "c.rs" which is novel (not in training), the unmapped count should be predictable and assertable, strengthening regression detection.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The test suite is strong and well-structured after the cycle-2 fixes. Key strengths:

- **85 cochange-related tests** (73 unit + 12 integration) provide comprehensive coverage across all 5 modules.
- **Behavior-focused assertions** -- tests verify observable outcomes (metrics values, error messages, set membership) not implementation details.
- **Clear Arrange-Act-Assert structure** with descriptive assertion messages throughout.
- **Shared `test_utils` module** behind a feature gate avoids helper duplication between unit and integration tests.
- **Edge-case coverage** is thorough: empty inputs, single commits, NaN fractions, clamping boundaries, unmapped files, single-file commits, errored-but-passing repos.
- **`full_pipeline_synthetic_repo`** is an excellent end-to-end test that constructs a known coupling pattern and verifies the model detects it -- including a regression-catching assertion that recall > 0 at the lowest threshold.
- **Serde roundtrip tests** for all types ensure serialization stability.
- **Resource cleanup** uses `TempDir` (automatic `Drop`) for all temporary directories.
- **Prior-cycle silent-skip issue** fully resolved with `assert!` + `panic!` pattern.

The single remaining condition: add a test for the `repo.error` rendering path in `report.rs` (avoids PF-002 -- do not defer noticed issues).
