//! Tests for the staleness detection module (staleness.rs).

#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::Write as _;
use std::path::PathBuf;

use tempfile::tempdir;

use super::{StalenessCheck, check_staleness, read_git_head, resolve_git_dir};

// ============================================================================
// Helpers
// ============================================================================

/// Create a minimal git repo structure in `dir` with the given HEAD content.
fn create_fake_git_repo(dir: &std::path::Path, head_content: &str) {
    let git_dir = dir.join(".git");
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(git_dir.join("HEAD"), head_content).unwrap();
}

/// Write a packed-refs file for the git repo in `dir`.
fn write_packed_refs(dir: &std::path::Path, content: &str) {
    let git_dir = dir.join(".git");
    fs::write(git_dir.join("packed-refs"), content).unwrap();
}

/// Create a ref file with SHA under `.git/refs/`.
fn create_ref_file(dir: &std::path::Path, ref_path: &str, sha: &str) {
    let git_dir = dir.join(".git");
    let full_path = git_dir.join(ref_path);
    fs::create_dir_all(full_path.parent().unwrap()).unwrap();
    fs::write(&full_path, format!("{sha}\n")).unwrap();
}

/// Write a manifest with the given git_head into `cache_dir`.
fn write_manifest_with_head(
    root: &std::path::Path,
    cache_dir: &std::path::Path,
    git_head: Option<&str>,
) {
    use crate::cmd::search::manifest::FileManifest;

    let mut manifest = FileManifest::new(root.to_path_buf(), cache_dir.to_path_buf());
    manifest.set_git_head(git_head.map(str::to_string));
    manifest.save().unwrap();
}

// ============================================================================
// resolve_git_dir
// ============================================================================

#[test]
fn test_resolve_git_dir_returns_git_dir_when_directory() {
    let dir = tempdir().unwrap();
    let git_path = dir.path().join(".git");
    fs::create_dir_all(&git_path).unwrap();

    let result = resolve_git_dir(dir.path());
    assert!(result.is_some(), "should resolve git dir when .git is a directory");
    assert_eq!(result.unwrap(), git_path);
}

#[test]
fn test_resolve_git_dir_returns_none_when_no_git() {
    let dir = tempdir().unwrap();
    // No .git at all
    assert!(
        resolve_git_dir(dir.path()).is_none(),
        "should return None when no .git present"
    );
}

#[test]
fn test_resolve_git_dir_follows_gitdir_file_for_worktree() {
    let dir = tempdir().unwrap();
    let worktree_dir = dir.path().join("worktree");
    fs::create_dir_all(&worktree_dir).unwrap();

    // Create the actual git dir that the .git file points to
    let actual_git_dir = dir.path().join("actual_git");
    fs::create_dir_all(&actual_git_dir).unwrap();

    // Write .git file (worktree style)
    let git_file_path = worktree_dir.join(".git");
    fs::write(
        &git_file_path,
        format!("gitdir: {}\n", actual_git_dir.display()),
    )
    .unwrap();

    let result = resolve_git_dir(&worktree_dir);
    assert!(result.is_some(), "should follow gitdir: pointer");
    assert_eq!(result.unwrap(), actual_git_dir);
}

// ============================================================================
// read_git_head
// ============================================================================

#[test]
fn test_read_git_head_returns_none_when_no_git() {
    let dir = tempdir().unwrap();
    assert!(
        read_git_head(dir.path()).is_none(),
        "should return None when no .git directory"
    );
}

#[test]
fn test_read_git_head_detached_head_raw_sha() {
    let dir = tempdir().unwrap();
    let sha = "abcdef1234567890abcdef1234567890abcdef12";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    let result = read_git_head(dir.path());
    assert_eq!(result.as_deref(), Some(sha));
}

#[test]
fn test_read_git_head_follows_symbolic_ref_to_loose_ref() {
    let dir = tempdir().unwrap();
    let sha = "deadbeef12345678deadbeef12345678deadbeef";
    create_fake_git_repo(dir.path(), "ref: refs/heads/main\n");
    create_ref_file(dir.path(), "refs/heads/main", sha);

    let result = read_git_head(dir.path());
    assert_eq!(result.as_deref(), Some(sha));
}

#[test]
fn test_read_git_head_falls_back_to_packed_refs() {
    let dir = tempdir().unwrap();
    let sha = "cafebabe12345678cafebabe12345678cafebabe";
    create_fake_git_repo(dir.path(), "ref: refs/heads/feature\n");
    // No loose ref file — only packed-refs
    write_packed_refs(
        dir.path(),
        &format!("# pack-refs with: peeled fully-peeled sorted\n{sha} refs/heads/feature\n"),
    );

    let result = read_git_head(dir.path());
    assert_eq!(result.as_deref(), Some(sha));
}

#[test]
fn test_read_git_head_loose_ref_takes_priority_over_packed() {
    let dir = tempdir().unwrap();
    let loose_sha = "1111111111111111111111111111111111111111";
    let packed_sha = "2222222222222222222222222222222222222222";
    create_fake_git_repo(dir.path(), "ref: refs/heads/main\n");
    create_ref_file(dir.path(), "refs/heads/main", loose_sha);
    write_packed_refs(
        dir.path(),
        &format!("{packed_sha} refs/heads/main\n"),
    );

    let result = read_git_head(dir.path());
    assert_eq!(
        result.as_deref(),
        Some(loose_sha),
        "loose ref should take priority over packed-refs"
    );
}

// ============================================================================
// check_staleness
// ============================================================================

#[test]
fn test_check_staleness_no_index_returns_no_index() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_fake_git_repo(dir.path(), "ref: refs/heads/main\n");

    let result = check_staleness(&cache_dir, dir.path());
    assert!(
        matches!(result, StalenessCheck::NoIndex),
        "no index.skidx → NoIndex, got {result:?}"
    );
}

#[test]
fn test_check_staleness_no_stored_head() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let sha = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    // Write manifest without git_head and create fake index file
    write_manifest_with_head(dir.path(), &cache_dir, None);
    // Create a stub index file so NoIndex branch is not triggered
    fs::write(cache_dir.join("index.skidx"), b"stub").unwrap();

    let result = check_staleness(&cache_dir, dir.path());
    assert!(
        matches!(result, StalenessCheck::NoStoredHead),
        "manifest without git_head → NoStoredHead, got {result:?}"
    );
}

#[test]
fn test_check_staleness_current_when_heads_match() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let sha = "bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    write_manifest_with_head(dir.path(), &cache_dir, Some(sha));
    fs::write(cache_dir.join("index.skidx"), b"stub").unwrap();

    let result = check_staleness(&cache_dir, dir.path());
    assert!(
        matches!(result, StalenessCheck::Current),
        "matching HEADs → Current, got {result:?}"
    );
}

#[test]
fn test_check_staleness_head_changed() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let stored_sha = "cccc3333cccc3333cccc3333cccc3333cccc3333";
    let current_sha = "dddd4444dddd4444dddd4444dddd4444dddd4444";
    create_fake_git_repo(dir.path(), &format!("{current_sha}\n"));

    write_manifest_with_head(dir.path(), &cache_dir, Some(stored_sha));
    fs::write(cache_dir.join("index.skidx"), b"stub").unwrap();

    let result = check_staleness(&cache_dir, dir.path());
    match result {
        StalenessCheck::HeadChanged { stored, current } => {
            assert_eq!(stored, stored_sha);
            assert_eq!(current, current_sha);
        }
        other => panic!("expected HeadChanged, got {other:?}"),
    }
}

#[test]
fn test_check_staleness_non_git_project_current_with_no_stored() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    // No .git directory — non-git project
    write_manifest_with_head(dir.path(), &cache_dir, None);
    fs::write(cache_dir.join("index.skidx"), b"stub").unwrap();

    let result = check_staleness(&cache_dir, dir.path());
    // Non-git project: no current HEAD, no stored HEAD → NoStoredHead
    assert!(
        matches!(result, StalenessCheck::NoStoredHead),
        "non-git project → NoStoredHead, got {result:?}"
    );
}
