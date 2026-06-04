# Testing Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-28T15:45:00Z

## Issues in Your Changes (BLOCKING)

### HIGH

**Integration test `full_pipeline_synthetic_repo` silently passes on failure via early-return guards** - `crates/rskim-bench/tests/cochange_validation.rs:296-430`
**Confidence**: 90%
- Problem: The `full_pipeline_synthetic_repo` test has 6 early-return guards (`if !git_available() { return; }`, `if history.commits.len() < 51 { return; }`, `if split.test.is_empty() { return; }`, etc.) that print to stderr and return `()`. Because the function returns `()`, these paths produce a passing test even when the entire test body is skipped. In CI environments without git or where timestamp manipulation fails, this test silently passes with zero validation. The `eprintln!("SKIPPED: ...")` messages go to stderr which is not typically inspected after a green run.
- Fix: Use `#[ignore]` for environment-dependent tests, or wrap the entire test in a `should_run()` guard that calls `return` exactly once at the top. Better still, for the guards that represent genuine infrastructure failures (not environment absence), use `panic!("unexpected: ...")` so they fail loudly. At minimum, consolidate the git-availability check to a single top-level guard and convert the mid-test `return`s to `panic!` since they represent unexpected failures after the repo was already set up:
```rust
// At the top:
if !git_available() {
    eprintln!("SKIPPED: git not available");
    return;
}
// Later, after repo setup succeeded:
let history = GixSource.parse_history(dir.path(), 0)
    .expect("parse_history should succeed on a valid synthetic repo");
assert!(history.commits.len() >= 51,
    "synthetic repo should have ≥51 commits, got {}",
    history.commits.len());
```

**Integration test makes weak assertions on the full pipeline -- only checks range [0,1]** - `crates/rskim-bench/tests/cochange_validation.rs:414-429`
**Confidence**: 85%
- Problem: The `full_pipeline_synthetic_repo` test constructs a synthetic repo with 44 A+B co-change commits specifically to produce a strong coupling signal, then asserts only that `macro_recall >= 0.0 && macro_recall <= 1.0` and `macro_precision >= 0.0 && macro_precision <= 1.0`. These assertions are trivially satisfied for any valid float. The comment says "recall must be >= 0" but the intent was clearly to verify the co-change model can detect the A-B coupling. The test as written cannot detect a regression in the evaluation pipeline that produces all-zero results.
- Fix: At the lowest threshold (0.01), the 44/50 A+B co-occurrence rate should produce a Jaccard well above 0.01. Assert that at least the lowest threshold shows non-zero recall:
```rust
// At threshold 0.01, strong A-B coupling should yield detectable signal.
let lowest = &metrics[0];
assert!(
    lowest.macro_recall > 0.0,
    "at threshold {}, recall should be > 0 given 44 A+B co-changes, got {}",
    lowest.threshold, lowest.macro_recall
);
```

### MEDIUM

**No tests for `parse_thresholds` boundary validation in the binary** - `crates/rskim-bench/src/bin/cochange_validate.rs:181-205`
**Confidence**: 85%
- Problem: `parse_thresholds` implements range validation `(0.0, 1.0]`, NaN rejection, empty-input rejection, deduplication, and sorting. None of these behaviors are tested. Since this function sits in a binary (`src/bin/`), it cannot be tested by the `rskim-bench` lib tests. The validation logic (especially the boundary rejection at 0.0 and acceptance at exactly 1.0) is a critical correctness requirement for the benchmark -- invalid thresholds would produce meaningless metrics.
- Fix: Extract `parse_thresholds` into a library module (e.g., `cochange/cli.rs` or `cochange/validate.rs`) and add unit tests. Alternatively, add a `#[cfg(test)] mod tests` block in the binary file if the Cargo test harness supports it:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_thresholds_rejects_zero() {
        assert!(parse_thresholds("0.0").is_err());
    }

    #[test]
    fn parse_thresholds_accepts_one() {
        let t = parse_thresholds("1.0").unwrap();
        assert_eq!(t, vec![1.0]);
    }

    #[test]
    fn parse_thresholds_rejects_above_one() {
        assert!(parse_thresholds("1.5").is_err());
    }

    #[test]
    fn parse_thresholds_rejects_empty() {
        assert!(parse_thresholds("").is_err());
    }

    #[test]
    fn parse_thresholds_deduplicates() {
        let t = parse_thresholds("0.1,0.1,0.3").unwrap();
        assert_eq!(t, vec![0.1, 0.3]);
    }
}
```

**No test for `temporal_split` with NaN `train_fraction`** - `crates/rskim-bench/src/cochange/temporal_split.rs:80-84`
**Confidence**: 82%
- Problem: The implementation explicitly handles NaN by falling back to 0.8 (`if train_fraction.is_finite() { ... } else { 0.8 }`), but there is no test exercising this code path. NaN handling is easy to break during refactoring (e.g., someone might change the guard to `if !train_fraction.is_nan()`, missing `Infinity`).
- Fix: Add a unit test in `temporal_split::tests`:
```rust
#[test]
fn nan_fraction_falls_back_to_default() {
    let commits = make_commits_newest_first(10);
    let split = temporal_split(&commits, f64::NAN);
    // Should behave like 0.8 → 8 train, 2 test.
    assert_eq!(split.train.len(), 8);
    assert_eq!(split.test.len(), 2);
}
```

**`aggregate_metrics` test uses direct float equality with `abs() < 1e-9` but does not test the aggregation formula** - `crates/rskim-bench/tests/cochange_validation.rs:436-497`
**Confidence**: 80%
- Problem: The test provides one passing repo and one failing repo, then asserts the aggregate precision matches the single passing repo's value exactly. This is correct for a single-repo case but does not test the actual averaging logic. A bug where `aggregate_metrics` sums without dividing by count would pass this test because N=1 means sum == average. Adding a second passing repo with different metrics would exercise the mean calculation.
- Fix: Add a second passing repo with different metric values to verify averaging:
```rust
let passing2 = RepoCochangeResult {
    // ... same structure but with macro_precision: 0.7, macro_recall: 0.8
    ..passing.clone()
};
passing2.macro_precision = 0.7;
// Then assert aggregate = (0.5 + 0.7) / 2 = 0.6
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Duplicate `make_commit` helper and `sample_*` helpers across unit and integration tests** - `crates/rskim-bench/tests/cochange_validation.rs:91-106`, `crates/rskim-bench/src/cochange/validate.rs:645-660`, `crates/rskim-bench/tests/cochange_validation.rs:542-593`
**Confidence**: 82%
- Problem: `make_commit` is defined identically in both the integration test file and the `validate.rs` unit tests. `sample_threshold_metrics` and `sample_validation_result` are duplicated between the integration test file and `types.rs` unit tests (with slightly different field values). This is not blocking but increases maintenance burden -- a change to `CommitInfo` fields would require updating the helper in multiple places.
- Fix: Consider adding a `#[cfg(test)]` test utilities module in `cochange/mod.rs` or a shared test fixture module that both unit and integration tests can import. This is a should-fix-while-here item, not blocking.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **No test for `evaluate_at_thresholds` error paths** - `crates/rskim-bench/src/cochange/validate.rs:235-238` (Confidence: 70%) -- The `Err(SearchError::IndexCorrupted(msg))` and `Err(e)` branches in the jaccard evaluation loop are not tested. These are error-forwarding paths that are hard to unit test without a mock `CochangeMatrixReader`, but they represent uncovered branches.

- **`chrono_now` produces an incorrect date format** - `crates/rskim-bench/src/bin/cochange_validate.rs:207-222` (Confidence: 75%) -- The function uses `1970 + (secs / 86400) / 365` which does not account for leap years, producing increasingly wrong year values over time. In 2026 the error is ~14 days which affects the year calculation near year boundaries. No test covers this function.

- **Integration test `full_pipeline_synthetic_repo` uses `--date` to amend timestamps but ignores the amend result** - `crates/rskim-bench/tests/cochange_validation.rs:328-332` (Confidence: 65%) -- The `git commit --amend --no-edit --date` call's result is discarded with `let _ = ...`. If the amend fails, the commit timestamps are uncontrolled, but the test proceeds and may produce different split boundaries. The test is resilient to this via its weak assertions, but it means the test may not actually be testing the intended temporal split scenario.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | - | 0 | 1 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

**Rationale**: The test suite is well-structured with good coverage breadth -- 70 unit tests covering deny list, temporal split, metrics, quality gates, serde roundtrips, and reports, plus 11 integration tests exercising the full pipeline. Test design follows AAA patterns, names describe expected behavior, and edge cases (empty input, single commit, clamped fractions) are well covered. The deny list has exemplary false-positive resistance tests (applies ADR-001 -- noticed issues are addressed inline rather than deferred). However, the integration test's silent pass-through pattern and trivially weak assertions on the full pipeline test undermine confidence that the evaluation logic is actually validated end-to-end. The untested `parse_thresholds` boundary validation in the binary is a meaningful gap given its role as the user-facing entry point.
