//! Tests for the file walker (walk.rs).

#![allow(clippy::unwrap_used)]

use super::{discover_project_root, walk_and_read};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

// ============================================================================
// Helpers
// ============================================================================

/// Create a temporary directory tree with some Rust source files.
///
/// Layout:
/// ```
/// root/
///   .git/              <- marks project root
///   src/
///     main.rs          <- valid Rust source
///     lib.rs           <- valid Rust source
///   build.py           <- Python source
///   README.md          <- Markdown
///   data.json          <- JSON
///   binary.bin         <- non-UTF8 bytes
///   huge.rs            <- a file whose content we can later check
/// ```
fn make_sample_tree() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Simulate git root
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn add(a: u32, b: u32) -> u32 { a + b }\n").unwrap();
    fs::write(root.join("build.py"), "print('hello')\n").unwrap();
    fs::write(root.join("README.md"), "# Hello\n").unwrap();
    fs::write(root.join("data.json"), "{\"key\": 1}\n").unwrap();

    // Non-UTF8 file: will be skipped
    fs::write(root.join("binary.bin"), b"\xFF\xFE invalid utf8 \x80").unwrap();

    dir
}

// ============================================================================
// discover_project_root
// ============================================================================

#[test]
fn test_discover_project_root_finds_git_root() {
    let dir = make_sample_tree();
    let root = dir.path();

    // Start from src/ — should walk up to find .git
    let src = root.join("src");
    let found = discover_project_root(&src).unwrap();
    assert_eq!(found, root.canonicalize().unwrap());
}

#[test]
fn test_discover_project_root_at_git_dir() {
    let dir = make_sample_tree();
    let root = dir.path();

    // Start at root itself
    let found = discover_project_root(root).unwrap();
    assert_eq!(found, root.canonicalize().unwrap());
}

#[test]
fn test_discover_project_root_no_git_returns_cwd() {
    // No .git anywhere — should fall back to the provided directory
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let found = discover_project_root(root).unwrap();
    // Falls back to the canonical form of the provided path
    assert_eq!(found, root.canonicalize().unwrap());
}

// ============================================================================
// walk_and_read
// ============================================================================

#[test]
fn test_walk_finds_rust_and_python_files() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (files, skipped) = walk_and_read(&root, 50_000).unwrap();

    // At minimum, main.rs, lib.rs, and build.py should be found
    let paths: Vec<PathBuf> = files.iter().map(|f| f.rel_path.clone()).collect();
    assert!(
        paths.iter().any(|p| p.ends_with("main.rs")),
        "main.rs not found in {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("lib.rs")),
        "lib.rs not found in {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("build.py")),
        "build.py not found in {paths:?}"
    );

    // binary.bin has non-UTF8 content → should be skipped
    let _ = skipped; // some files will be skipped (binary, unsupported, etc.)
}

#[test]
fn test_walk_skips_non_utf8_files() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (files, _skipped) = walk_and_read(&root, 50_000).unwrap();
    let paths: Vec<PathBuf> = files.iter().map(|f| f.rel_path.clone()).collect();

    // binary.bin must not appear
    assert!(
        !paths.iter().any(|p| p.extension().is_some_and(|e| e == "bin")),
        "binary.bin should be skipped, paths: {paths:?}"
    );
}

#[test]
fn test_walk_sha256_is_64_hex_chars() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (files, _) = walk_and_read(&root, 50_000).unwrap();
    for f in &files {
        assert_eq!(
            f.sha256.len(),
            64,
            "sha256 of {} should be 64 hex chars, got {}",
            f.rel_path.display(),
            f.sha256.len()
        );
        assert!(
            f.sha256.chars().all(|c| c.is_ascii_hexdigit()),
            "sha256 of {} contains non-hex chars: {}",
            f.rel_path.display(),
            f.sha256
        );
    }
}

#[test]
fn test_walk_sha256_is_deterministic() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (files1, _) = walk_and_read(&root, 50_000).unwrap();
    let (files2, _) = walk_and_read(&root, 50_000).unwrap();

    // Same root → same results in the same order
    assert_eq!(files1.len(), files2.len());
    for (f1, f2) in files1.iter().zip(files2.iter()) {
        assert_eq!(f1.rel_path, f2.rel_path);
        assert_eq!(f1.sha256, f2.sha256);
    }
}

#[test]
fn test_walk_sha256_changes_when_content_changes() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (files_before, _) = walk_and_read(&root, 50_000).unwrap();
    let main_before = files_before
        .iter()
        .find(|f| f.rel_path.ends_with("main.rs"))
        .unwrap();
    let sha_before = main_before.sha256.clone();

    // Modify the file
    fs::write(root.join("src/main.rs"), "fn main() { println!(\"hello\"); }\n").unwrap();

    let (files_after, _) = walk_and_read(&root, 50_000).unwrap();
    let main_after = files_after
        .iter()
        .find(|f| f.rel_path.ends_with("main.rs"))
        .unwrap();

    assert_ne!(sha_before, main_after.sha256, "SHA should change after file modification");
}

#[test]
fn test_walk_respects_max_files_cap() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();
    // Create 10 files
    for i in 0..10 {
        fs::write(root.join(format!("file_{i}.rs")), format!("fn f{i}() {{}}\n")).unwrap();
    }

    // Cap at 3
    let (files, _skipped) = walk_and_read(&root.canonicalize().unwrap(), 3).unwrap();
    assert_eq!(files.len(), 3, "walker should stop at max_files=3, got {}", files.len());
}

#[test]
fn test_walk_skips_files_over_5mb() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();

    // Create a file over 5 MB
    let big = vec![b'a'; 6 * 1024 * 1024];
    fs::write(root.join("big.rs"), &big).unwrap();
    // Create a small file too
    fs::write(root.join("small.rs"), "fn f() {}\n").unwrap();

    let (files, _skipped) = walk_and_read(&root.canonicalize().unwrap(), 50_000).unwrap();
    let paths: Vec<PathBuf> = files.iter().map(|f| f.rel_path.clone()).collect();

    assert!(
        !paths.iter().any(|p| p.ends_with("big.rs")),
        "big.rs (>5MB) should be skipped, paths: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("small.rs")),
        "small.rs should be included, paths: {paths:?}"
    );
}

#[test]
fn test_walk_empty_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();

    let (files, _skipped) = walk_and_read(&root, 50_000).unwrap();
    assert!(files.is_empty(), "empty dir should yield no files");
}

#[test]
fn test_walk_returns_relative_paths() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (files, _) = walk_and_read(&root, 50_000).unwrap();
    for f in &files {
        assert!(
            f.rel_path.is_relative(),
            "expected relative path, got: {}",
            f.rel_path.display()
        );
    }
}

#[test]
fn test_walk_skips_git_directory() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (files, _) = walk_and_read(&root, 50_000).unwrap();
    let paths: Vec<PathBuf> = files.iter().map(|f| f.rel_path.clone()).collect();

    // No file under .git/ should appear
    for p in &paths {
        assert!(
            !p.starts_with(".git"),
            ".git files should be excluded, found: {}",
            p.display()
        );
    }
}
