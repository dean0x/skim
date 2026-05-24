# Testing Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**Redundant test covers identical assertions as sibling test** - `crates/rskim/src/cmd/search/index_tests.rs:128,176`
**Confidence**: 85%
- Problem: `test_index_incremental_cache_hits_verified_via_manifest` (line 128) and `test_index_incremental_manifest_correctness` (line 176) perform nearly identical work: both run two consecutive builds, load manifests for each, and assert SHA-256 stability and field_map preservation across builds. The only additional assertion in the second test is checking that Rust files have a non-empty `field_map` (lines 217-223). This redundancy inflates the test count without meaningfully increasing coverage, and creates maintenance burden when the build pipeline changes (two tests must be updated instead of one).
- Fix: Merge the two tests into a single `test_index_incremental_manifest_correctness` test that covers all three assertions (entry existence, SHA stability, non-empty field_map for Rust files). Alternatively, keep the second test but have it only test the incremental-specific field_map non-emptiness assertion, removing the duplicated SHA checks.

**No test for incremental cache hit count** - `crates/rskim/src/cmd/search/index.rs:76-80`
**Confidence**: 82%
- Problem: The pipeline returns `IndexResult.cache_hits` (line 212) and prints it to stderr (line 79), but no test asserts that a second build on unchanged files produces `cache_hits > 0`. The existing incremental tests (`test_index_incremental_cache_hits_verified_via_manifest`) verify SHA stability in the manifest, but they test indirectly via manifest file comparison. The actual `cache_hits` counter in `IndexResult` is never inspected. Since the `run()` function returns `ExitCode` (not `IndexResult`), the pipeline's internal cache-hit tracking is untestable through the current public API.
- Fix: Either (a) expose `build_index` as `pub(super)` and add a test that calls it directly to assert `result.cache_hits == 3` after a second build on `make_project()`, or (b) capture stderr output in the integration test and assert it contains `"3 cache hits"`. Option (a) is preferred as it tests behavior rather than output formatting.

### MEDIUM

**Test does not verify file content after modification** - `crates/rskim/src/cmd/search/index_tests.rs:227`
**Confidence**: 85%
- Problem: `test_index_incremental_modified_file_reindexed` modifies `src/main.rs`, runs a second build, and asserts `ExitCode::SUCCESS`. However, it does not verify that the manifest SHA actually changed for the modified file. A build that silently ignores the modification and reuses the cached entry would still pass this test. The test name claims the file is "reindexed" but only checks exit code, not reindexation behavior.
- Fix: Load the manifest after the second build and assert that the SHA-256 for `src/main.rs` differs from the SHA after the first build:
  ```rust
  let manifest1 = FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();
  let sha1 = manifest1.lookup("src/main.rs").unwrap().sha256.clone();
  // ... modify file ...
  run(&args).unwrap();
  let manifest2 = FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();
  let sha2 = manifest2.lookup("src/main.rs").unwrap().sha256.clone();
  assert_ne!(sha1, sha2, "SHA should change after modification");
  ```

**Missing error path tests for manifest save failures** - `crates/rskim/src/cmd/search/manifest.rs:210`
**Confidence**: 80%
- Problem: The manifest `save()` method has multiple error paths (temp file creation, JSON serialization, flush, persist/rename). No test validates that save errors propagate correctly. While testing I/O errors is hard, the `persist()` failure path (line 241-242) can be triggered by writing to a read-only directory, and the error message formatting should be validated.
- Fix: Add a test that attempts to save a manifest to a non-writable directory and asserts the error is `Err`:
  ```rust
  #[test]
  fn test_save_to_unwritable_dir_returns_error() {
      let dir = tempfile::tempdir().unwrap();
      let unwritable = dir.path().join("readonly");
      fs::create_dir_all(&unwritable).unwrap();
      fs::set_permissions(&unwritable, fs::Permissions::from_mode(0o444)).unwrap();
      let mut manifest = FileManifest::new(dir.path().to_path_buf(), unwritable);
      manifest.insert(sample_entry("a.rs", &"a".repeat(64)));
      assert!(manifest.save().is_err());
  }
  ```

**Missing test for `--force` actually re-classifies cached files** - `crates/rskim/src/cmd/search/index_tests.rs:253`
**Confidence**: 80%
- Problem: `test_index_force_flag_ignores_manifest` only asserts `ExitCode::SUCCESS`. It does not verify that the force flag actually caused re-classification (zero cache hits). A bug where `--force` is silently ignored would pass this test. The test name is stronger than its assertions.
- Fix: Similar to the cache hit suggestion above, either access `build_index` directly to check `cache_hits == 0`, or capture stderr and assert the output contains `"0 cache hits"`.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`walk_and_read` determinism test relies on implicit ordering assumption** - `crates/rskim/src/cmd/search/walk_tests.rs:160`
**Confidence**: 82%
- Problem: `test_walk_sha256_is_deterministic` compares `files1` and `files2` element-by-element via `zip`. The test assumes that two calls to `walk_and_read` on the same directory return files in the same order. While the walker uses `sort_by_file_path`, this is documented as a should-have, not a contract. If the walker's sort ever became non-deterministic (e.g., from parallel traversal), this test would flake rather than fail clearly.
- Fix: Either (a) sort both result lists by `rel_path` before comparing (making the test robust to ordering changes), or (b) add a comment noting the test intentionally validates the sorting guarantee. Option (b) is lighter since the sort is intentional behavior.

**`test_walk_skips_non_utf8_files` uses extension-based filtering instead of path matching** - `crates/rskim/src/cmd/search/walk_tests.rs:120`
**Confidence**: 80%
- Problem: The test asserts no path has extension `.bin`, but `.bin` files are skipped because `Language::from_path` returns `None` for `.bin` (unsupported language), not because of the non-UTF-8 content. The test would pass even if non-UTF-8 detection was completely broken. The test name claims it validates "skips non-UTF8 files" but actually validates "skips unsupported extensions".
- Fix: Use a supported extension for the binary file (e.g., `binary.rs` with invalid UTF-8 bytes) and assert it does not appear in the results. This would actually test the non-UTF-8 skip path:
  ```rust
  fs::write(root.join("binary.rs"), b"\xFF\xFE invalid utf8 \x80").unwrap();
  // ... then assert binary.rs does not appear in files
  ```

## Pre-existing Issues (Not Blocking)

No pre-existing issues above CRITICAL threshold.

## Suggestions (Lower Confidence)

- **No negative test for manifest format version mismatch** - `crates/rskim/src/cmd/search/manifest.rs:144` (Confidence: 70%) -- The manifest loader checks `header.version != Self::FORMAT_VERSION` but no test validates this branch. Writing a manifest file with `version: 99` and asserting it loads as empty would exercise this defensive check.

- **Missing test for `CapReached` skip reason reporting** - `crates/rskim/src/cmd/search/walk.rs:148-149` (Confidence: 65%) -- When `max_files` is hit, the walker pushes `SkipReason::CapReached` and breaks. No test asserts the skip reason list contains this variant after hitting the cap. `test_walk_respects_max_files_cap` only checks `files.len()`.

- **`run_classify` error fallback produces empty field_map silently** - `crates/rskim/src/cmd/search/index.rs:264` (Confidence: 65%) -- The fallback to an empty `Vec` on classification failure means the file still gets indexed with no field information, which could produce misleading search results. No test exercises a classification failure path. This is acknowledged by the debug-gate pattern but is worth a targeted test.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 3 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The test suite is well-structured with clear AAA separation, good use of tempdir for isolation, and broad coverage of the happy path. The main gaps are: (1) tests that assert exit codes without verifying actual behavioral outcomes (several tests would pass even if the feature they claim to test was broken), (2) a pair of redundant tests that inflate count without adding coverage, and (3) a test that claims to validate non-UTF-8 skipping but actually validates unsupported-extension skipping. Fixing the HIGH items (de-duplicating the redundant test, adding a cache-hit count assertion) would raise confidence in the incremental build path significantly.
