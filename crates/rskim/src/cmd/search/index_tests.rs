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
fn test_index_incremental_second_build_faster_or_same() {
    // Smoke test: two consecutive builds both succeed.
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    let r1 = run(&args).unwrap();
    let r2 = run(&args).unwrap();

    assert_eq!(r1, ExitCode::SUCCESS);
    assert_eq!(r2, ExitCode::SUCCESS);
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
