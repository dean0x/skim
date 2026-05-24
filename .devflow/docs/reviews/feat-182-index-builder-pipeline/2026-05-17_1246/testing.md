# Testing Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**Missing error path test for `open_and_read` TOCTOU fallback** - `crates/rskim/src/cmd/search/walk.rs:183-190`
**Confidence**: 82%
- Problem: The new `open_and_read` function introduced error-kind routing logic at lines 181-196 that distinguishes `InvalidData` (non-UTF-8), `ErrorKind::Other` with "too large" message, and generic I/O errors. The "too large" branch (file grew between pre-screen and open) is untested. This relies on string matching (`e.to_string().contains("too large")`) which is fragile and has no test exercising the fallback path.
- Fix: Add a unit test for `open_and_read` directly (or integration test via `walk_and_read`) that covers the "file grew past limit" scenario. Since it is hard to simulate the TOCTOU race in a unit test, at minimum test `open_and_read` with a file larger than `MAX_FILE_BYTES` to confirm the `ErrorKind::Other` path produces the correct `SkipReason::TooLarge`:
```rust
#[test]
fn test_open_and_read_file_over_limit_returns_too_large_error() {
    let dir = tempfile::tempdir().unwrap();
    let big_file = dir.path().join("big.rs");
    let content = vec![b'x'; 6 * 1024 * 1024]; // > 5 MB
    fs::write(&big_file, &content).unwrap();
    let result = open_and_read(&big_file);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert!(err.to_string().contains("too large"));
}
```

**String-matching for error classification is brittle and untested** - `crates/rskim/src/cmd/search/walk.rs:183-184`
**Confidence**: 85%
- Problem: Error classification uses `e.to_string().contains("too large")` to detect the TOCTOU fallback case. This is an implementation detail that couples to the exact error message string produced by `open_and_read`. If the message changes, the classification silently degrades to `ReadError` instead of `TooLarge`. No test validates this coupling remains correct.
- Fix: Use a custom error type or an extension trait with a typed variant rather than string matching. At minimum, add a test that verifies `io::Error::other("too large")` is correctly classified:
```rust
#[test]
fn test_walk_classifies_too_large_from_open_and_read() {
    // Verifies the error string coupling between open_and_read and walk_and_read
    let err = io::Error::other("too large");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert!(err.to_string().contains("too large"));
}
```
  Better architectural fix: Replace string matching with a typed error enum that both `open_and_read` and the caller share.

### MEDIUM

**No test for `run_classify` debug logging path** - `crates/rskim/src/cmd/search/index.rs:262-273`
**Confidence**: 80%
- Problem: The `run_classify` function has two branches: success and error-with-debug-logging. The error branch's fallback to `Vec::new()` is tested implicitly through the full pipeline (files that fail classification still get indexed), but the `SKIM_DEBUG` conditional logging path is not exercised. This is a lower priority since it is a diagnostic-only path, but the env-var gating pattern could regress.
- Fix: Consider adding a test that sets `SKIM_DEBUG=1`, provides content that causes `classify_source` to fail, and verifies stderr contains the debug message. Or accept coverage gap as acceptable for a diagnostic-only path.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Tests lack assertion on `cache_hits` count in incremental builds** - `crates/rskim/src/cmd/search/index_tests.rs:112-124`
**Confidence**: 83%
- Problem: `test_index_incremental_second_build_faster_or_same` only asserts exit code `SUCCESS` for both builds. It does not verify that the second build actually achieved cache hits. The test name implies incremental behavior, but only tests that the command doesn't crash. The newer `test_index_incremental_manifest_correctness` partially addresses this by verifying SHA stability, but no test asserts `cache_hits > 0` on the second build.
- Fix: The `build_index` function returns `IndexResult` with `cache_hits`, but `run()` only returns `ExitCode`. Either expose `IndexResult` for testing or capture stderr output and assert it contains "cache hits":
```rust
// Option: Parse the stderr summary for "N cache hits" where N > 0
// Option: Extract build_index into a testable function that returns IndexResult
```

**`test_index_max_files_limits_manifest_entries` makes implicit ordering assumption** - `crates/rskim/src/cmd/search/index_tests.rs:253-290`
**Confidence**: 80%
- Problem: The test creates 10 files, indexes with `--max-files=2`, and asserts exactly 2 entries exist. This correctly tests the cap, but implicitly depends on the walker's lexicographic ordering (files are `file_00.rs` through `file_09.rs`). If the walker order changes, the test still passes (it counts, doesn't check which 2). This is actually good test design. However, the assertion `entry_count == 2` is stronger than the guarantee: `--max-files=2` should produce at most 2, not exactly 2 (edge case: if 1 file is unsupported, the count could be less). In this test all files are `.rs` so the assertion holds, but the test name says "limits" not "equals".
- Fix: This is minor. No change needed if the intent is to test the exact behavior with all-supported-language files. Consider adding a comment clarifying the assertion strength.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**No test for `resolve_search_cache_dir` fallback when `SKIM_CACHE_DIR` is unset** - `crates/rskim/src/cmd/search/index.rs:282-290`
**Confidence**: 80%
- Problem: `resolve_search_cache_dir` depends on `crate::cmd::resolve_cache_dir()` which reads `SKIM_CACHE_DIR` or falls back to `~/.cache/skim/`. All index tests use `--index-dir` to override this. The default resolution path has no unit test.
- Fix: Add a test that unsets `SKIM_CACHE_DIR`, calls `resolve_search_cache_dir` with a known path, and verifies the result is under `~/.cache/skim/search/`.

**No test verifies `project_root_hash` produces stable, deterministic output** - `crates/rskim/src/cmd/search/index.rs:295-305`
**Confidence**: 80%
- Problem: `project_root_hash` is a critical correctness function (determines cache directory location). If the hash changes between builds, incremental caching breaks. No test validates its output is stable for a given input.
- Fix: Add a unit test:
```rust
#[test]
fn test_project_root_hash_is_stable() {
    let hash1 = project_root_hash(Path::new("/tmp/myproject"));
    let hash2 = project_root_hash(Path::new("/tmp/myproject"));
    assert_eq!(hash1, hash2);
    assert_eq!(hash1.len(), 16);
    assert!(hash1.chars().all(|c| c.is_ascii_hexdigit()));
}
```

## Suggestions (Lower Confidence)

- **Missing negative test for `--max-files` with non-numeric value** - `crates/rskim/src/cmd/search/index_tests.rs` (Confidence: 72%) — The `parse_positive_usize` validator rejects non-numeric input, but no test exercises `--max-files=abc`. Clap handles this but explicit regression coverage would be prudent.

- **Walker test coverage for `.gitignore` respect** - `crates/rskim/src/cmd/search/walk_tests.rs` (Confidence: 65%) — No test creates a `.gitignore` file and verifies excluded files are actually skipped by the walker. The `WalkBuilder` configuration is tested implicitly, but a focused regression test would catch accidental flag changes.

- **Manifest version migration path untested** - `crates/rskim/src/cmd/search/manifest.rs:144` (Confidence: 68%) — When `FORMAT_VERSION` is bumped in the future, the version check silently discards old manifests. No test validates this behavior with a `version: 99` header.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 2 | 0 |

**Testing Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The test suite is well-structured with good coverage of the happy path, incremental builds, edge cases (empty directories, mixed languages, max-files cap), and argument validation. Tests follow AAA structure, use behavior-focused assertions, and maintain clean separation between unit tests (walk_tests, manifest_tests) and integration tests (index_tests). The main gap is the brittle string-matching error classification in `walk.rs` which has no direct test coverage — this is the only area that could silently regress without detection.
