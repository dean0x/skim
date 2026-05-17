//! Integration tests for the index builder pipeline (index.rs).

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::ExitCode;
use tempfile::TempDir;

use super::run;

// ============================================================================
// Helpers
// ============================================================================

/// Create a minimal project tree with a .git root and a few source files.
fn make_project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"hello\"); }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn add(a: u32, b: u32) -> u32 { a + b }\n",
    )
    .unwrap();
    fs::write(root.join("build.py"), "print('hello')\n").unwrap();

    dir
}

/// Build args for running index against `project` with `cache` as the output dir.
fn index_args(project: &Path, cache: &Path) -> Vec<String> {
    vec![
        format!("--root={}", project.display()),
        format!("--index-dir={}", cache.display()),
    ]
}

// ============================================================================
// Full build — happy path
// ============================================================================

#[test]
fn test_index_build_succeeds_with_source_files() {
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();

    let result = run(&index_args(project.path(), cache.path())).unwrap();

    assert_eq!(result, ExitCode::SUCCESS, "index build should succeed");
}

#[test]
fn test_index_writes_skidx_and_skpost() {
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();

    run(&index_args(project.path(), cache.path())).unwrap();

    assert!(
        find_file_with_ext(cache.path(), "skidx"),
        "index.skidx should exist in cache dir"
    );
    assert!(
        find_file_with_ext(cache.path(), "skpost"),
        "index.skpost should exist in cache dir"
    );
}

#[test]
fn test_index_writes_manifest_sidecar() {
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();

    run(&index_args(project.path(), cache.path())).unwrap();

    assert!(
        find_file_with_ext(cache.path(), "skfiles"),
        "index.skfiles manifest should exist in cache dir"
    );
}

// ============================================================================
// Empty directory
// ============================================================================

#[test]
fn test_index_empty_directory_returns_success() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();

    let cache = tempfile::tempdir().unwrap();

    let result = run(&index_args(root, cache.path())).unwrap();

    assert_eq!(result, ExitCode::SUCCESS, "empty dir should still succeed");
}

// ============================================================================
// Incremental build — cache hits
// ============================================================================

#[test]
fn test_index_incremental_second_build_succeeds() {
    // Smoke test: two consecutive builds on the same project both succeed.
    // (Previously misnamed "faster_or_same" — no timing assertion is made here.)
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    let r1 = run(&args).unwrap();
    let r2 = run(&args).unwrap();

    assert_eq!(r1, ExitCode::SUCCESS);
    assert_eq!(r2, ExitCode::SUCCESS);
}

#[test]
fn test_index_incremental_cache_hits_verified_via_manifest() {
    // Verify that the incremental path (SHA match → reuse field_map) produces
    // identical manifest entries across two consecutive builds on unchanged files.
    // Also asserts that Rust sources produce non-empty field_maps (classifier ran).
    use super::super::manifest::FileManifest;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    // First build — cold start, no cache.
    let r1 = run(&args).unwrap();
    assert_eq!(r1, ExitCode::SUCCESS, "first build should succeed");

    let manifest1 =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();

    // Second build — should hit the manifest cache for all unchanged files.
    let r2 = run(&args).unwrap();
    assert_eq!(r2, ExitCode::SUCCESS, "second build should succeed");

    let manifest2 =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();

    // All three source files from make_project() must be present with stable
    // SHAs — a missing entry or changed SHA would indicate the incremental path
    // failed to recognise the file as cached.
    for path in &["src/main.rs", "src/lib.rs", "build.py"] {
        let e1 = manifest1
            .lookup(path)
            .unwrap_or_else(|| panic!("first manifest should contain {path}"));
        let e2 = manifest2
            .lookup(path)
            .unwrap_or_else(|| panic!("second manifest should contain {path}"));

        assert_eq!(
            e1.sha256, e2.sha256,
            "sha256 for {path} must be identical across both builds (content unchanged)"
        );

        // The field_map must also be preserved — same encoding on both runs.
        assert_eq!(
            e1.field_map, e2.field_map,
            "field_map for {path} must be identical when served from cache"
        );
    }

    // Rust files must have a non-empty field_map — the classifier must have
    // produced output (not silently fallen back to an empty map).
    for path in &["src/main.rs", "src/lib.rs"] {
        let entry = manifest2
            .lookup(path)
            .unwrap_or_else(|| panic!("second manifest should contain {path}"));
        assert!(
            !entry.field_map.is_empty(),
            "field_map for {path} should be non-empty after classification"
        );
    }
}

#[test]
fn test_index_incremental_modified_file_reindexed() {
    use super::super::manifest::FileManifest;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    // First build — record the SHA for src/main.rs before modification.
    run(&args).unwrap();
    let manifest1 =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();
    let sha_before = manifest1
        .lookup("src/main.rs")
        .expect("first manifest must contain src/main.rs")
        .sha256
        .clone();

    // Modify the file so its SHA-256 changes.
    fs::write(
        project.path().join("src/main.rs"),
        "fn main() { eprintln!(\"modified\"); }\n",
    )
    .unwrap();

    // Second build — should detect the change and re-classify.
    let r2 = run(&args).unwrap();
    assert_eq!(
        r2,
        ExitCode::SUCCESS,
        "incremental build after modification should succeed"
    );

    // The SHA in the new manifest must differ — silent cache reuse would be wrong.
    let manifest2 =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();
    let sha_after = manifest2
        .lookup("src/main.rs")
        .expect("second manifest must contain src/main.rs")
        .sha256
        .clone();

    assert_ne!(
        sha_before, sha_after,
        "SHA for src/main.rs must change after file modification — cache reuse would be wrong"
    );
}

#[test]
fn test_index_force_flag_ignores_manifest() {
    use super::super::types::IndexConfig;
    use super::build_index;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    // First build to populate the manifest (creates cache entries for all files).
    run(&args).unwrap();

    // Force rebuild via build_index directly so we can inspect IndexResult.
    // cache_hits must be zero — --force means the manifest is intentionally ignored.
    let config = IndexConfig {
        root: project.path().to_path_buf(),
        max_files: None,
        force: true,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };
    let result = build_index(&config).expect("--force rebuild should not fail");

    assert_eq!(
        result.cache_hits, 0,
        "--force must produce zero cache hits (manifest was ignored); got {}",
        result.cache_hits
    );
    assert!(
        result.file_count > 0,
        "--force rebuild should index at least one file"
    );
}

// ============================================================================
// Incremental build — cache hit count (direct build_index)
// ============================================================================

#[test]
fn test_index_incremental_cache_hits_count() {
    use super::super::types::IndexConfig;
    use super::build_index;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();

    let config = IndexConfig {
        root: project.path().to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    // Cold start — no manifest exists.
    let result1 = build_index(&config).expect("first build should succeed");
    assert!(result1.file_count > 0, "first build should index files");
    assert_eq!(result1.cache_hits, 0, "cold start must have zero cache hits");

    // Incremental — all files unchanged, all should be cache hits.
    let result2 = build_index(&config).expect("second build should succeed");
    assert!(result2.cache_hits > 0, "incremental build must have cache hits");
    assert_eq!(
        result2.cache_hits, result2.file_count,
        "all {} files should be cache hits; got {}",
        result2.file_count, result2.cache_hits
    );
}

// ============================================================================
// Mixed languages
// ============================================================================

#[test]
fn test_index_mixed_languages() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();

    fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.join("script.py"), "def hello(): pass\n").unwrap();
    fs::write(root.join("app.ts"), "export function greet(): void {}\n").unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();

    let cache = tempfile::tempdir().unwrap();
    let result = run(&index_args(root, cache.path())).unwrap();

    assert_eq!(
        result,
        ExitCode::SUCCESS,
        "mixed language build should succeed"
    );
}

// ============================================================================
// --max-files integration
// ============================================================================

#[test]
fn test_index_max_files_limits_manifest_entries() {
    // Create 10 source files, index with --max-files=2, and verify that the
    // manifest contains at most 2 entries.  This exercises the full CLI flag
    // path end-to-end (clap parse → walk cap → manifest write).
    use super::super::manifest::FileManifest;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();

    for i in 0..10 {
        fs::write(
            root.join(format!("file_{i:02}.rs")),
            format!("fn f{i}() {{}}\n"),
        )
        .unwrap();
    }

    let cache = tempfile::tempdir().unwrap();
    let mut args = index_args(root, cache.path());
    args.push("--max-files=2".to_string());

    let result = run(&args).unwrap();
    assert_eq!(result, ExitCode::SUCCESS, "--max-files=2 build should succeed");

    let manifest = FileManifest::load(root.to_path_buf(), cache.path().to_path_buf()).unwrap();

    // Count entries by checking all possible file names.
    let entry_count = (0..10)
        .filter(|i| manifest.lookup(&format!("file_{i:02}.rs")).is_some())
        .count();

    assert_eq!(
        entry_count, 2,
        "only 2 files should be indexed when --max-files=2, got {entry_count}"
    );
}

// ============================================================================
// Help flag
// ============================================================================

#[test]
fn test_index_help_returns_success() {
    let result = run(&["--help".to_string()]).unwrap();
    assert_eq!(result, ExitCode::SUCCESS);
}

#[test]
fn test_index_short_help_returns_success() {
    let result = run(&["-h".to_string()]).unwrap();
    assert_eq!(result, ExitCode::SUCCESS);
}

// ============================================================================
// Argument validation
// ============================================================================

#[test]
fn test_index_max_files_zero_is_rejected() {
    // --max-files=0 must produce an error, not a silently empty index.
    let result = run(&["--max-files=0".to_string()]);
    assert!(result.is_err(), "--max-files=0 should return an error");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("≥ 1") || msg.contains("positive"),
        "error message should mention the constraint, got: {msg}"
    );
}

#[test]
fn test_index_unknown_flag_is_rejected() {
    let result = run(&["--unknown-flag".to_string()]);
    assert!(result.is_err(), "unknown flags should return an error");
}

// ============================================================================
// Private helpers
// ============================================================================

/// Search for a file with the given extension in `dir`, up to `max_depth`
/// levels deep. `max_depth = 0` checks only direct children of `dir`.
/// Bounded to prevent infinite recursion on symlink loops.
fn find_file_with_ext_depth(dir: &Path, ext: &str, max_depth: usize) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if max_depth > 0 && find_file_with_ext_depth(&path, ext, max_depth - 1) {
                return true;
            }
        } else if path.extension().is_some_and(|e| e == ext) {
            return true;
        }
    }
    false
}

fn find_file_with_ext(dir: &Path, ext: &str) -> bool {
    find_file_with_ext_depth(dir, ext, 5)
}
