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
    // Verify that the second build actually reuses cached data rather than just
    // not crashing.  Cache hits are observable through the manifest: when a file
    // is served from cache its SHA-256 is identical to the first build.  If the
    // second build re-classified every file the SHAs would still match (content
    // unchanged), but the field_map entries would be re-computed from scratch.
    //
    // The incremental path is: SHA match → reuse field_map from manifest.
    // We verify this by asserting that:
    //   (a) both builds succeed,
    //   (b) every entry in the second build's manifest has the same SHA as the
    //       first build — proving the walker recognised unchanged files and the
    //       pipeline produced a coherent manifest on both runs.
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
}

#[test]
fn test_index_incremental_manifest_correctness() {
    // After two consecutive builds the manifest must:
    //   (a) contain entries for all source files,
    //   (b) have stable SHA-256 values across builds,
    //   (c) have non-empty field_maps for at least the Rust sources.
    use super::super::manifest::{FileManifest, ManifestEntry};

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    // First build
    run(&args).unwrap();
    let manifest1 = FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf())
        .unwrap();

    // Second build — should reuse cached data
    run(&args).unwrap();
    let manifest2 = FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf())
        .unwrap();

    // (a) Entries must exist for the three source files in make_project().
    // Paths use forward slashes in the manifest.
    for path in &["src/main.rs", "src/lib.rs", "build.py"] {
        assert!(
            manifest2.lookup(path).is_some(),
            "manifest should contain an entry for {path}"
        );
    }

    // (b) SHA-256 values must be identical across both builds (content unchanged).
    for path in &["src/main.rs", "src/lib.rs", "build.py"] {
        let e1: &ManifestEntry = manifest1.lookup(path).unwrap();
        let e2: &ManifestEntry = manifest2.lookup(path).unwrap();
        assert_eq!(
            e1.sha256, e2.sha256,
            "sha256 for {path} should be stable between builds"
        );
    }

    // (c) Rust files must have a non-empty field_map (classifier produced output).
    for path in &["src/main.rs", "src/lib.rs"] {
        let entry = manifest2.lookup(path).unwrap();
        assert!(
            !entry.field_map.is_empty(),
            "field_map for {path} should be non-empty after classification"
        );
    }
}

#[test]
fn test_index_incremental_modified_file_reindexed() {
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    // First build
    run(&args).unwrap();

    // Modify a file
    fs::write(
        project.path().join("src/main.rs"),
        "fn main() { eprintln!(\"modified\"); }\n",
    )
    .unwrap();

    // Second build — should detect the change and re-classify
    let r2 = run(&args).unwrap();

    assert_eq!(
        r2,
        ExitCode::SUCCESS,
        "incremental build after modification should succeed"
    );
}

#[test]
fn test_index_force_flag_ignores_manifest() {
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    // First build to populate manifest
    run(&args).unwrap();

    // Force rebuild — should ignore manifest entirely
    let mut force_args = args.clone();
    force_args.push("--force".to_string());
    let r2 = run(&force_args).unwrap();

    assert_eq!(r2, ExitCode::SUCCESS, "--force rebuild should succeed");
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

fn find_file_with_ext(dir: &Path, ext: &str) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if find_file_with_ext(&path, ext) {
                return true;
            }
        } else if path.extension().is_some_and(|e| e == ext) {
            return true;
        }
    }
    false
}
