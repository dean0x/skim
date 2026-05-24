# Testing Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**Incremental build test does not verify cache hits actually occurred** - `index_tests.rs:112-124`
**Confidence**: 90%
- Problem: `test_index_incremental_second_build_faster_or_same` only asserts that both builds return `ExitCode::SUCCESS`. The test name claims to verify incremental behavior ("faster or same"), but there is no assertion on cache hits, file counts, or timing. The `run()` function returns `ExitCode` but the underlying `IndexResult` (which contains `cache_hits`, `file_count`, `skipped`) is discarded before reaching the test. This means the incremental build path is not actually verified -- the second build could re-classify every file from scratch and this test would still pass.
- Fix: Either expose `build_index` (or a test-only wrapper) that returns `IndexResult`, or inspect the manifest file after both builds to verify entries match. At minimum, verify the manifest file's entry count matches the expected file count after both builds:
```rust
#[test]
fn test_index_incremental_second_build_has_cache_hits() {
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    run(&args).unwrap(); // first build
    run(&args).unwrap(); // second build -- should have cache hits

    // Verify manifest exists and has entries for all 3 source files
    let manifest_path = find_file_path_with_ext(cache.path(), "skfiles").unwrap();
    let contents = fs::read_to_string(manifest_path).unwrap();
    let lines: Vec<&str> = contents.lines().collect();
    // 1 header + 3 entries (main.rs, lib.rs, build.py)
    assert!(lines.len() >= 4, "manifest should have header + 3 entries");
}
```

**Minified file detection has no direct test** - `walk.rs:217-228`, `walk_tests.rs`
**Confidence**: 85%
- Problem: The `is_minified` function is a non-trivial heuristic (probe first 8KB, count newlines, check average line length > 500 bytes), but there is no test for it in `walk_tests.rs`. The minification check is a documented skip condition for tree-sitter languages and is part of the walk pipeline, yet the entire skip reason is untested. A regression here would silently index minified bundles, degrading index quality.
- Fix: Add a test that creates a minified-style file and verifies it is skipped:
```rust
#[test]
fn test_walk_skips_minified_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();

    // Create a "minified" JS file: one long line > 500 avg bytes/line
    let minified = "var a=1;".repeat(200); // 1600 bytes, 0 newlines
    fs::write(root.join("bundle.js"), &minified).unwrap();
    // Normal file for comparison
    fs::write(root.join("normal.rs"), "fn f() {}\n").unwrap();

    let (files, _skipped) = walk_and_read(&root.canonicalize().unwrap(), 50_000).unwrap();
    let paths: Vec<PathBuf> = files.iter().map(|f| f.rel_path.clone()).collect();

    assert!(!paths.iter().any(|p| p.ends_with("bundle.js")),
        "minified file should be skipped");
    assert!(paths.iter().any(|p| p.ends_with("normal.rs")),
        "normal file should be included");
}
```

### MEDIUM

**Argument parsing error paths are untested** - `index.rs:83-127`
**Confidence**: 85%
- Problem: `parse_args` handles several error conditions -- unknown arguments (`anyhow::bail!`), missing flag values (`{flag} requires a value`), and invalid `--max-files` values (`--max-files requires a positive integer`). None of these error branches have tests. The `run()` function is tested only with valid inputs or `--help`.
- Fix: Add tests for invalid argument handling:
```rust
#[test]
fn test_index_unknown_arg_returns_error() {
    let result = run(&["--bogus".to_string()]);
    assert!(result.is_err() || result.unwrap() != ExitCode::SUCCESS);
}

#[test]
fn test_index_max_files_non_numeric_returns_error() {
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = vec![
        format!("--root={}", project.path().display()),
        format!("--index-dir={}", cache.path().display()),
        "--max-files=abc".to_string(),
    ];
    assert!(run(&args).is_err());
}
```

**Duplicated encode/decode helpers between manifest.rs and manifest_tests.rs** - `manifest_tests.rs:32-46`
**Confidence**: 82%
- Problem: `manifest_tests.rs` defines its own `encode_field_map` and `decode_field_map` functions (lines 32-46) that duplicate the implementations in `manifest.rs` (lines 243-264). The tests use these local copies instead of the production code. This means the tests for `test_field_map_encoding_roundtrip` and `test_field_map_unknown_discriminant_filtered` are testing the test-local copies, not the actual production functions. If the production `encode_field_map`/`decode_field_map` had a bug, these tests would not catch it.
- Fix: Use `super::encode_field_map` and `super::decode_field_map` directly in the test file instead of re-implementing them:
```rust
// Remove the local encode_field_map and decode_field_map functions
// and import from the parent module:
use super::{FileManifest, ManifestEntry, encode_field_map, decode_field_map};
```

**`test_index_force_flag_ignores_manifest` does not verify force actually bypassed cache** - `index_tests.rs:152-167`
**Confidence**: 80%
- Problem: The test only asserts `ExitCode::SUCCESS` after a `--force` rebuild. It does not verify that the manifest was actually ignored (e.g., all files were re-classified with zero cache hits). Without such a check, this test passes identically to a normal incremental build.
- Fix: Similar to the incremental test, this requires access to `IndexResult` or an observable side effect. A pragmatic approach is to corrupt the manifest between builds and verify the force build still succeeds (proving it ignored the corrupt manifest):
```rust
#[test]
fn test_index_force_ignores_corrupt_manifest() {
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    run(&args).unwrap(); // first build

    // Corrupt the manifest
    let manifest = find_file_path_with_ext(cache.path(), "skfiles").unwrap();
    fs::write(&manifest, "corrupt data").unwrap();

    // Force rebuild should succeed despite corrupt manifest
    let mut force_args = args.clone();
    force_args.push("--force".to_string());
    let r = run(&force_args).unwrap();
    assert_eq!(r, ExitCode::SUCCESS);
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**No test for `--max-files` flag integration** - `index.rs:97-101`
**Confidence**: 82%
- Problem: The `--max-files` flag is parsed in `parse_args` and passed through to `walk_and_read`, but no integration test verifies this end-to-end. The `walk_tests.rs` tests verify `walk_and_read` with a direct `max_files` parameter, but the CLI argument parsing path (`--max-files=N`) is never exercised.
- Fix: Add an integration test:
```rust
#[test]
fn test_index_max_files_flag_limits_indexed_count() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    for i in 0..10 {
        fs::write(root.join(format!("f{i}.rs")), format!("fn f{i}() {{}}\n")).unwrap();
    }
    let cache = tempfile::tempdir().unwrap();
    let args = vec![
        format!("--root={}", root.display()),
        format!("--index-dir={}", cache.path().display()),
        "--max-files=2".to_string(),
    ];
    let r = run(&args).unwrap();
    assert_eq!(r, ExitCode::SUCCESS);
}
```

**No test for file deletion between incremental builds** - `index.rs:158-244`
**Confidence**: 80%
- Problem: The incremental build test covers file modification but not file deletion. If a file is indexed in build 1 then deleted before build 2, the manifest should no longer contain that file's entry. This is an important behavioral edge case for incremental builds.
- Fix:
```rust
#[test]
fn test_index_incremental_deleted_file_removed_from_manifest() {
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    run(&args).unwrap(); // build 1 with 3 files
    fs::remove_file(project.path().join("build.py")).unwrap();
    run(&args).unwrap(); // build 2 with 2 files

    // Manifest should only have 2 entries (build.py removed)
    let manifest_path = find_file_path_with_ext(cache.path(), "skfiles").unwrap();
    let contents = fs::read_to_string(manifest_path).unwrap();
    let entry_lines = contents.lines().skip(1).count(); // skip header
    assert_eq!(entry_lines, 2, "deleted file should not appear in manifest");
}
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`test_walk_skips_non_utf8_files` asserts on wrong property** - `walk_tests.rs:128-133` (Confidence: 70%) -- The test checks that no file with `.bin` extension is in the results, but `binary.bin` would already be skipped by the `UnsupportedLanguage` filter (`.bin` is not a recognized language extension). The test is not actually exercising the non-UTF8 skip path. A more robust test would use a recognized extension (e.g., `.rs`) with non-UTF8 content.

- **No version mismatch test for manifest format** - `manifest.rs:139-141` (Confidence: 65%) -- The `load()` function returns an empty manifest when `header.version != FORMAT_VERSION`, but no test writes a manifest with a future version number to verify this branch is exercised.

- **`discover_project_root` loop has no explicit bound** - `walk.rs:56-64` (Confidence: 60%) -- The loop walks up the filesystem to the root. While this is naturally bounded by the filesystem depth, the project's CLAUDE.md reliability rules state "every loop must have a fixed upper bound." This is borderline since the OS enforces a depth limit.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The test suite has good structural coverage across walk, manifest, and index modules (38 tests, all passing). Test organization is clean with co-located test files and clear AAA structure. However, the critical gap is that the incremental build and force-rebuild tests only verify `ExitCode::SUCCESS` without asserting that the underlying behavior (cache hits, manifest correctness, force bypass) actually occurred. The duplicated encode/decode helpers in test code also mean the production codec functions are not directly tested. Fixing the two HIGH-severity issues (incremental build observability and minification skip coverage) would meaningfully improve confidence in the pipeline's correctness.
