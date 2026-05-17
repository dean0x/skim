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
    fs::write(
        root.join("src/lib.rs"),
        "pub fn add(a: u32, b: u32) -> u32 { a + b }\n",
    )
    .unwrap();
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
    // Use a supported extension (.rs) so that Language::from_path returns Some,
    // which lets classify_entry proceed past the unsupported-language gate and
    // exercise the actual non-UTF-8 detection code path (open_and_read returns
    // ReadOutcome::NonUtf8 → SkipReason::NonUtf8).  A .bin file would be
    // rejected earlier by the unsupported-language check, never reaching the
    // UTF-8 read; that code path is already covered by test_walk_finds_rust_and_python_files.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    // Valid UTF-8 Rust source — should be accepted.
    fs::write(root.join("valid.rs"), "fn main() {}\n").unwrap();
    // Invalid UTF-8 content with a supported .rs extension — must be skipped
    // via the non-UTF-8 code path, not the unsupported-language gate.
    fs::write(root.join("invalid_utf8.rs"), b"\xFF\xFE not valid utf8 \x80").unwrap();

    let root = root.canonicalize().unwrap();
    let (files, skipped) = walk_and_read(&root, 50_000).unwrap();
    let paths: Vec<PathBuf> = files.iter().map(|f| f.rel_path.clone()).collect();

    assert!(
        !paths.iter().any(|p| p.ends_with("invalid_utf8.rs")),
        "invalid_utf8.rs should be skipped, paths: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("valid.rs")),
        "valid.rs should be accepted, paths: {paths:?}"
    );
    // Confirm the skip was recorded with the correct reason.
    let has_non_utf8 = skipped.iter().any(|r| {
        matches!(
            r,
            super::super::types::SkipReason::NonUtf8(path)
            if path.ends_with("invalid_utf8.rs")
        )
    });
    assert!(
        has_non_utf8,
        "skipped list should contain NonUtf8 for invalid_utf8.rs, got: {skipped:?}"
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

    let (mut files1, _) = walk_and_read(&root, 50_000).unwrap();
    let (mut files2, _) = walk_and_read(&root, 50_000).unwrap();

    // Sort by rel_path before comparing so the assertion is order-independent.
    // walk_and_read documents lexicographic ordering via sort_by_file_path, but
    // sorting here makes the test robust if that contract ever changes: a broken
    // sort would surface as a mismatched SHA rather than a flaky zip mismatch.
    files1.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    files2.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

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
    fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"hello\"); }\n",
    )
    .unwrap();

    let (files_after, _) = walk_and_read(&root, 50_000).unwrap();
    let main_after = files_after
        .iter()
        .find(|f| f.rel_path.ends_with("main.rs"))
        .unwrap();

    assert_ne!(
        sha_before, main_after.sha256,
        "SHA should change after file modification"
    );
}

#[test]
fn test_walk_respects_max_files_cap() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();
    // Create 10 files
    for i in 0..10 {
        fs::write(
            root.join(format!("file_{i}.rs")),
            format!("fn f{i}() {{}}\n"),
        )
        .unwrap();
    }

    // Cap at 3
    let (files, _skipped) = walk_and_read(&root.canonicalize().unwrap(), 3).unwrap();
    assert_eq!(
        files.len(),
        3,
        "walker should stop at max_files=3, got {}",
        files.len()
    );
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

    let (files, skipped) = walk_and_read(&root.canonicalize().unwrap(), 50_000).unwrap();
    let paths: Vec<PathBuf> = files.iter().map(|f| f.rel_path.clone()).collect();

    assert!(
        !paths.iter().any(|p| p.ends_with("big.rs")),
        "big.rs (>5MB) should be skipped, paths: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("small.rs")),
        "small.rs should be included, paths: {paths:?}"
    );

    // The oversized file must appear in the skipped list with TooLarge reason.
    let has_too_large = skipped.iter().any(|r| {
        matches!(
            r,
            super::super::types::SkipReason::TooLarge { path, .. }
            if path.ends_with("big.rs")
        )
    });
    assert!(
        has_too_large,
        "skipped list should contain TooLarge for big.rs, got: {skipped:?}"
    );
}

/// Verify that `SkipReason::TooLarge` carries the correct path and a size
/// that reflects the actual file size (not a sentinel).
///
/// The walker has two size checks:
///   1. A fast pre-screen on `DirEntry` metadata before opening the file.
///   2. A second check inside `open_and_read` on the opened file handle
///      (guards against TOCTOU growth between pre-screen and read).
///
/// Both paths produce `SkipReason::TooLarge { size }` where `size` is the
/// real byte count from the metadata.  This test exercises case 1 (the
/// pre-screen), which is the reliable codepath in a non-concurrent test.
#[test]
fn test_walk_too_large_skip_reason_contains_path_and_size() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();

    // Write a file that exceeds the 5 MiB limit by 1 byte.
    let over_limit: u64 = 5 * 1024 * 1024 + 1;
    let big = vec![b'x'; over_limit as usize];
    fs::write(root.join("over_limit.rs"), &big).unwrap();

    let (files, skipped) = walk_and_read(&root.canonicalize().unwrap(), 50_000).unwrap();

    // The file must not appear in the accepted list.
    assert!(
        files.iter().all(|f| !f.rel_path.ends_with("over_limit.rs")),
        "over_limit.rs should not be indexed"
    );

    // The skipped list must contain a TooLarge entry with the correct path
    // and a size field that reflects the actual file size.
    let too_large_entry = skipped
        .iter()
        .find(|r| {
            matches!(
                r,
                super::super::types::SkipReason::TooLarge { path, .. }
                if path.ends_with("over_limit.rs")
            )
        })
        .unwrap_or_else(|| {
            panic!("skipped list should contain TooLarge for over_limit.rs, got: {skipped:?}")
        });

    if let super::super::types::SkipReason::TooLarge { size, .. } = too_large_entry {
        assert!(
            *size > 5 * 1024 * 1024,
            "TooLarge size should exceed 5 MiB, got {size}"
        );
    }
}

#[test]
fn test_walk_skips_minified_js_file() {
    // is_minified() fires when average bytes-per-line in the first 8 KB exceeds
    // 500.  A single-line .js file with 10 000 bytes has no newlines at all,
    // so the average is 8192 (the full probe length) — well above the threshold.
    // The file must NOT appear in the walk results.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();

    // Write a "minified" JS file: one very long line, no newlines.
    let content = "x".repeat(10_000);
    fs::write(root.join("bundle.js"), &content).unwrap();

    // Write a normal JS file alongside it to confirm the walker still works.
    fs::write(
        root.join("normal.js"),
        "function greet() {\n  return 'hello';\n}\n",
    )
    .unwrap();

    let root_canonical = root.canonicalize().unwrap();
    let (files, _skipped) = walk_and_read(&root_canonical, 50_000).unwrap();
    let paths: Vec<PathBuf> = files.iter().map(|f| f.rel_path.clone()).collect();

    assert!(
        !paths.iter().any(|p| p.ends_with("bundle.js")),
        "minified bundle.js should be skipped, paths: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("normal.js")),
        "normal.js should be included, paths: {paths:?}"
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
