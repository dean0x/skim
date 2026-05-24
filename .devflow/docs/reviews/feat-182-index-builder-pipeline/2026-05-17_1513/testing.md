# Testing Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Missing error-path test for `build_index` when `walk_and_read` returns I/O error** - `crates/rskim/src/cmd/search/index_tests.rs`
**Confidence**: 82%
- Problem: `build_index` propagates fatal I/O errors from `walk_and_read` (via `?`), but no test exercises this error path. The test for an empty directory confirms success on zero files, but there is no test for the case where `walk_and_read` itself returns `Err` (e.g., unreadable root directory). The `run_classify` fallback path (returns empty Vec on classify failure) is also exercised only implicitly via the incremental cache tests -- no test directly verifies that a classify failure does not crash the pipeline.
- Fix: Add a test that passes an invalid/unreadable root to `build_index` and asserts the error propagates correctly:
```rust
#[test]
fn test_index_unreadable_root_returns_error() {
    use super::super::types::IndexConfig;
    use super::build_index;

    let config = IndexConfig {
        root: PathBuf::from("/nonexistent/path/that/does/not/exist"),
        max_files: None,
        force: false,
        cache_dir_override: Some(tempfile::tempdir().unwrap().into_path()),
    };
    // walk_and_read should fail on a nonexistent root
    assert!(build_index(&config).is_err() || /* walk succeeds with 0 files */
            build_index(&config).unwrap().file_count == 0);
}
```

**No test for `SkipReason::Minified` reason content** - `crates/rskim/src/cmd/search/walk_tests.rs:358`
**Confidence**: 80%
- Problem: `test_walk_skips_minified_js_file` verifies the minified file does not appear in accepted files, but does not assert the skip reason type in the `skipped` list (unlike `test_walk_skips_non_utf8_files` and `test_walk_skips_files_over_5mb` which both pattern-match on the `SkipReason` variant). This is an inconsistency -- if the minification logic regresses to returning `Transparent` instead of `Skip(Minified)`, the test would still pass (file absent from `files`) but the diagnostic output would be lost silently.
- Fix: Add an assertion verifying the `Minified` skip reason is recorded:
```rust
let has_minified = _skipped.iter().any(|r| {
    matches!(
        r,
        super::super::types::SkipReason::Minified(path)
        if path.ends_with("bundle.js")
    )
});
assert!(
    has_minified,
    "skipped list should contain Minified for bundle.js, got: {_skipped:?}"
);
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`test_walk_sha256_is_deterministic` still partially relies on ordering invariant** - `crates/rskim/src/cmd/search/walk_tests.rs:186`
**Confidence**: 82%
- Problem: The test comment says "sorting here makes the test robust if that contract ever changes" which is good defensive design. However, the test only compares `rel_path` and `sha256` in the zip -- it does not verify that the two result sets have the same `content` or `lang` fields. If a regression corrupted the `lang` detection to be non-deterministic (e.g., race in parallel walker), this test would still pass. Given the walk was recently changed from sequential to parallel (a major concurrency change), adding `lang` to the assertion would strengthen the signal.
- Fix: Add `assert_eq!(f1.lang, f2.lang);` inside the zip loop.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**No unit tests for `is_minified` helper at boundary values** - `crates/rskim/src/cmd/search/walk.rs:364`
**Confidence**: 85%
- Problem: The `is_minified()` function has subtle edge-case behavior: (1) when `content` is empty (returns false since `probe.len()` is 0, which is not > 500), (2) when average is exactly 500 (boundary -- `>` means it would not be considered minified), (3) when content is shorter than `MINIFY_PROBE_BYTES`. The integration test (`test_walk_skips_minified_js_file`) only tests a clear minified case (10K chars, no newlines). A unit test for the boundary condition (exactly 500 bytes/line) would prevent subtle regressions.
- Fix: Add boundary unit tests for `is_minified`:
```rust
#[cfg(test)]
mod is_minified_tests {
    use super::is_minified;

    #[test]
    fn empty_content_is_not_minified() {
        assert!(!is_minified(""));
    }

    #[test]
    fn exactly_500_avg_bytes_per_line_is_not_minified() {
        // 501 bytes total with 1 newline = 501/1 = 501 > 500 => minified
        // 500 bytes total with 1 newline = 500/1 = 500 => NOT minified (> not >=)
        let content = format!("{}\n", "x".repeat(499)); // 500 bytes, 1 newline
        assert!(!is_minified(&content));
    }

    #[test]
    fn just_over_500_avg_is_minified() {
        let content = format!("{}\n", "x".repeat(500)); // 501 bytes, 1 newline
        assert!(is_minified(&content));
    }
}
```

**No test for `MAX_SKIP_REASONS` cap in walk** - `crates/rskim/src/cmd/search/walk.rs:62`
**Confidence**: 80%
- Problem: The walker caps skip-reason collection at 10,000 entries to prevent memory exhaustion on large monorepos. This safety limit is not tested. A regression that removes the cap would silently pass all existing tests.
- Fix: This would require creating a directory with >10,000 unsupported files, which is expensive in a test. A targeted unit test could exercise the `MAX_SKIP_REASONS` check by mocking the walker or by creating a smaller cap variant for testing. Mark as informational since the cap is a defence-in-depth measure.

## Suggestions (Lower Confidence)

- **Parallel walker TOCTOU over-collection not directly tested** - `crates/rskim/src/cmd/search/walk.rs:312` (Confidence: 65%) -- The `files.truncate(max_files)` line handles over-collection from the parallel walker, but `test_walk_respects_max_files_cap` only verifies the final count is correct. A stress test with many threads could verify truncation actually fires, but this is hard to trigger deterministically.

- **`test_load_stops_at_entry_cap` uses `entries.len()` which accesses private field** - `crates/rskim/src/cmd/search/manifest_tests.rs:256` (Confidence: 70%) -- The test asserts on `manifest.entries.len()` directly (accessing the `HashMap` field). If the struct's visibility changes or the field is wrapped, this test breaks. Using repeated `lookup()` calls for boundary entries would be more resilient, but since this is a `#[cfg(test)]` module in the same crate, direct field access is acceptable in Rust.

- **No test verifying `sync_data()` atomicity guarantee** - `crates/rskim/src/cmd/search/manifest.rs:277` (Confidence: 62%) -- The new `sync_data()` call before `persist()` is a crash-safety improvement, but verifying it requires simulated power-loss scenarios which are impractical in unit tests.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 2 | 0 |

**Testing Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The test suite is well-structured with good coverage of the happy paths, incremental build cache semantics, safety limits, and skip conditions. Tests follow clear Arrange-Act-Assert structure with descriptive names. The defensive sort in `test_walk_sha256_is_deterministic` and the fidelity improvement in `test_walk_skips_non_utf8_files` (using a supported extension to actually exercise the UTF-8 code path) show thoughtful test design. The two blocking MEDIUM issues are about ensuring the skip-reason diagnostic outputs are consistently tested across all skip paths, which would improve regression detection for the new parallel walker refactoring.
