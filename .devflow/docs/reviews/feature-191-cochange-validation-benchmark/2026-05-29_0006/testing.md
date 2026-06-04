# Testing Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Silent test skip via early `return` in `full_pipeline_synthetic_repo`** - `crates/rskim-bench/tests/cochange_validation.rs:281-288`
**Confidence**: 85%
- Problem: The `full_pipeline_synthetic_repo` test uses `if !git_available() { return; }` and `let Some(dir) = init_git_repo() else { return; }` to silently skip when git is unavailable. In CI environments or containers without git, this test passes with zero assertions executed — a green suite that validates nothing. The `eprintln!("SKIPPED:...")` message goes to stderr but does not fail the test or appear in `cargo test` default output.
- Fix: Use the `#[ignore]` attribute with a descriptive message, or gate with a compile-time feature flag so CI explicitly tracks which tests ran versus were skipped. Alternatively, assert at test start and panic with a clear message if the precondition is unmet (so CI knows it was skipped):
```rust
#[test]
fn full_pipeline_synthetic_repo() {
    if !git_available() {
        panic!("PRECONDITION: git must be available for this integration test");
    }
    // ...
}
```
Or use `#[ignore]` with a CI job that runs `cargo test -- --ignored` in a git-enabled environment.

**`evaluate_at_thresholds` has no dedicated unit test for empty test commits** - `crates/rskim-bench/src/cochange/validate.rs:180-354`
**Confidence**: 82%
- Problem: The `evaluate_at_thresholds` function handles the case where all test commits have `< 2` known IDs (lines 232-234) and the case where `macro_commit_count[ti] == 0` (line 319), but neither path is exercised by a dedicated test. The only test calling this function (`full_pipeline_synthetic_repo`) always has multi-file test commits with known IDs. A regression in the empty-commit path would go undetected.
- Fix: Add a unit test that calls `evaluate_at_thresholds` with test commits containing only unmapped files or single-file commits to verify it returns zero metrics gracefully:
```rust
#[test]
fn evaluate_at_thresholds_with_no_usable_test_commits() {
    // Single-file test commits should yield zero metrics
    let test_commits = vec![make_commit(0, 100, &["unknown_file.rs"])];
    let path_map = HashMap::new(); // no files mapped
    // ... setup reader with empty matrix ...
    let (metrics, unmapped) = evaluate_at_thresholds(&reader, &test_commits, &path_map, &[0.1])
        .expect("should not error on empty inputs");
    assert_eq!(metrics[0].commit_count, 0);
    assert_eq!(unmapped, 1);
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`aggregate_metrics` with repos having an `error` field set but `quality_gate_passed = true` is not tested** - `crates/rskim-bench/src/cochange/validate.rs:461-524`
**Confidence**: 80%
- Problem: The filter at line 467 excludes repos where `r.error.is_some()` regardless of `quality_gate_passed`. This is correct behavior, but the `aggregate_metrics_skips_failed_repos` integration test only covers the `quality_gate_passed = false` exclusion path. A repo with `quality_gate_passed = true` but `error = Some(...)` (an error during evaluation that occurs after the quality gate passes) is never tested for proper exclusion.
- Fix: Add a test case to `aggregate_metrics_skips_failed_repos` with a repo that has `quality_gate_passed: true` and a non-None `error` field, verifying it is still excluded from aggregation.

**`quality_gate_rejects_short_history` does not assert the error message content** - `crates/rskim-bench/tests/cochange_validation.rs:163-171`
**Confidence**: 80%
- Problem: Unlike `quality_gate_rejects_small_repo` (which asserts the error message contains "multi-file commits"), this test only asserts `is_err()` without checking the error message. If the quality gate logic were refactored to accidentally fail for a different reason (e.g., a new check added first), this test would still pass while not validating the intended behavior.
- Fix: Add a message assertion:
```rust
let err = result.unwrap_err();
let msg = err.to_string();
assert!(
    msg.contains("span") || msg.contains("history"),
    "error should mention history span: {msg}"
);
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **No test for `MAX_FILES_FOR_EVALUATION` guard** - `crates/rskim-bench/src/cochange/validate.rs:193-199` (Confidence: 70%) — The capacity guard that rejects repos with > 20,000 files has no unit test. This is difficult to test without constructing a very large path map, but a targeted test using a mock reader could verify the bail path fires correctly.

- **`full_pipeline_synthetic_repo` does not verify unmapped file count** - `crates/rskim-bench/tests/cochange_validation.rs:386` (Confidence: 65%) — The test assigns `_unmapped` without asserting it. Since the test commits introduce "c.rs" which is novel (not in training), the unmapped count should be predictable and assertable, strengthening the test.

- **`sample_threshold_metrics` hardcodes inconsistent F1 values** - `crates/rskim-bench/tests/cochange_validation.rs:572-584` (Confidence: 62%) — The helper uses `macro_f1: 0.574` and `micro_f1: 0.564` which are approximations rather than exact `compute_f1(p, r)` results. While these are test fixtures not actual metric computations, using `compute_f1` would self-document correctness and prevent drift if the F1 formula ever changes.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The test suite is well-structured overall — 71 unit tests across 5 modules plus 11 integration tests provide good coverage of the core validation pipeline. Tests follow clear Arrange-Act-Assert structure, use meaningful assertion messages, and the shared `test_utils` module avoids helper duplication. The `full_pipeline_synthetic_repo` end-to-end test is particularly strong: it constructs a known coupling pattern and verifies the model detects it.

The conditions for approval:
1. Address the silent skip pattern in `full_pipeline_synthetic_repo` — either convert to `#[ignore]` or make the skip visible in CI (applies ADR-001).
2. Add edge-case coverage for `evaluate_at_thresholds` with empty/unmappable test commits.
