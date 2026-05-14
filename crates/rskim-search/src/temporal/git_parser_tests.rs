//! Tests for [`GixSource`] — written test-first (RED phase before implementation).
//!
//! Each group tests one behavioural contract of `parse_history`. Tests create
//! temporary git repositories via `gix::init` and the `git` CLI helper so we can
//! exercise the real parser against real git objects without requiring an external
//! git binary for most tests (using gix init + commit directly).

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use crate::temporal::{GixSource, is_fix_commit};
use crate::types::{SearchError, TemporalSource};

// ============================================================================
// Test infrastructure
// ============================================================================

/// Create a minimal git repo via the `git` CLI.
///
/// Returns `None` when git isn't available (CI environments without git).
/// Tests that require git skip themselves gracefully.
fn init_git_repo() -> Option<TempDir> {
    let dir = tempfile::tempdir().ok()?;

    // Try `git init -b main` first (git ≥2.28); fall back to plain `git init`.
    let init_ok = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
        || Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

    if !init_ok {
        return None;
    }

    // Configure identity so commits work
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir.path())
        .output()
        .ok()?;
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(dir.path())
        .output()
        .ok()?;
    Some(dir)
}

/// Check git is available for tests that require it.
fn git_available() -> bool {
    Command::new("git")
        .args(["--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Add a file and commit it in `dir`.
fn git_commit_file(dir: &Path, filename: &str, content: &str, message: &str) -> bool {
    std::fs::write(dir.join(filename), content).is_ok()
        && Command::new("git")
            .args(["add", filename])
            .current_dir(dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        && Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

/// Delete a file and commit the deletion.
fn git_delete_file(dir: &Path, filename: &str, message: &str) -> bool {
    Command::new("git")
        .args(["rm", filename])
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
        && Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

// ============================================================================
// Repository opening
// ============================================================================

#[test]
fn test_empty_repo_returns_ok_empty() {
    if !git_available() {
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    let src = GixSource;
    let result = src.parse_history(dir.path(), 90);
    let history = result.expect("empty repo should succeed");
    assert!(
        history.commits.is_empty(),
        "expected no commits in empty repo, got {}",
        history.commits.len()
    );
    assert_eq!(history.metadata.commit_count, 0);
}

#[test]
fn test_nonexistent_path_returns_git_error() {
    let src = GixSource;
    let result = src.parse_history(Path::new("/nonexistent/__no_such_path__"), 90);
    assert!(
        matches!(result, Err(SearchError::Git(_))),
        "expected Git error for nonexistent path, got: {result:?}"
    );
}

#[test]
fn test_not_a_git_repo_returns_git_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = GixSource;
    let result = src.parse_history(dir.path(), 90);
    assert!(
        matches!(result, Err(SearchError::Git(_))),
        "expected Git error for non-repo dir, got: {result:?}"
    );
}

// ============================================================================
// Commit parsing
// ============================================================================

#[test]
fn test_single_commit_fields() {
    if !git_available() {
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(dir.path(), "hello.txt", "world", "feat: first commit"));

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");
    assert_eq!(history.commits.len(), 1);

    let commit = &history.commits[0];
    // Hash should be 40 hex chars
    assert_eq!(commit.hash.len(), 40, "hash should be 40 chars: {}", commit.hash);
    assert!(
        commit.hash.chars().all(|c| c.is_ascii_hexdigit()),
        "hash should be hex: {}",
        commit.hash
    );
    assert!(commit.timestamp > 0, "timestamp should be positive");
    assert!(!commit.author.is_empty(), "author should be non-empty");
    assert_eq!(commit.message, "feat: first commit");
}

#[test]
fn test_multiple_commits_ordering() {
    if !git_available() {
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(dir.path(), "a.txt", "a", "commit one"));
    assert!(git_commit_file(dir.path(), "b.txt", "b", "commit two"));
    assert!(git_commit_file(dir.path(), "c.txt", "c", "commit three"));

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");
    assert_eq!(history.commits.len(), 3);
    // Newest first: commits[0] should be "commit three"
    assert_eq!(history.commits[0].message, "commit three");
    assert_eq!(history.commits[2].message, "commit one");
    // Timestamps should be non-increasing (newest first ordering)
    for window in history.commits.windows(2) {
        assert!(
            window[0].timestamp >= window[1].timestamp,
            "commits should be ordered newest first: {} < {}",
            window[0].timestamp,
            window[1].timestamp
        );
    }
}

#[test]
fn test_root_commit_includes_changed_files() {
    if !git_available() {
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(dir.path(), "main.rs", "fn main(){}", "add main"));

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");
    assert_eq!(history.commits.len(), 1);
    // Root commit diffed against empty tree — should contain main.rs
    let files: Vec<&PathBuf> = history.commits[0]
        .changed_files
        .iter()
        .map(|f| &f.path)
        .collect();
    assert!(
        files.iter().any(|p| p.as_os_str() == "main.rs"),
        "expected main.rs in changed_files, got: {files:?}"
    );
}

#[test]
fn test_commit_with_multiple_files() {
    if !git_available() {
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    // Create 3 files and commit them together
    std::fs::write(dir.path().join("a.rs"), "a").unwrap();
    std::fs::write(dir.path().join("b.rs"), "b").unwrap();
    std::fs::write(dir.path().join("c.rs"), "c").unwrap();
    assert!(
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    );
    assert!(
        Command::new("git")
            .args(["commit", "-m", "add three files"])
            .current_dir(dir.path())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    );

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");
    assert_eq!(history.commits.len(), 1);
    assert_eq!(
        history.commits[0].changed_files.len(),
        3,
        "expected 3 changed files, got: {:?}",
        history.commits[0].changed_files
    );
}

// ============================================================================
// Lookback filtering
// ============================================================================

#[test]
fn test_lookback_zero_returns_all_history() {
    if !git_available() {
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(dir.path(), "a.txt", "a", "old commit"));
    assert!(git_commit_file(dir.path(), "b.txt", "b", "new commit"));

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");
    assert_eq!(history.commits.len(), 2, "lookback_days=0 should return all commits");
}

#[test]
fn test_lookback_large_value_returns_recent() {
    if !git_available() {
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(dir.path(), "a.txt", "a", "commit A"));
    assert!(git_commit_file(dir.path(), "b.txt", "b", "commit B"));

    let src = GixSource;
    // lookback_days=365 should include commits from the last year (both are very recent)
    let history = src.parse_history(dir.path(), 365).expect("parse_history");
    assert_eq!(history.commits.len(), 2, "both recent commits should be within 365 days");
}

// ============================================================================
// File tracking
// ============================================================================

#[test]
fn test_file_addition_appears_in_changed_files() {
    if !git_available() {
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(dir.path(), "new_feature.rs", "pub fn feature(){}", "add feature"));

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");
    let files: Vec<&PathBuf> = history.commits[0].changed_files.iter().map(|f| &f.path).collect();
    assert!(
        files.iter().any(|p| p.as_os_str() == "new_feature.rs"),
        "expected new_feature.rs in changed_files, got: {files:?}"
    );
}

#[test]
fn test_file_deletion_appears_in_changed_files() {
    if !git_available() {
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(dir.path(), "old.rs", "content", "add old.rs"));
    assert!(git_delete_file(dir.path(), "old.rs", "delete old.rs"));

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");
    assert_eq!(history.commits.len(), 2);
    // The deletion commit (newest, commits[0]) should mention old.rs
    let delete_commit = &history.commits[0];
    let files: Vec<&PathBuf> = delete_commit.changed_files.iter().map(|f| &f.path).collect();
    assert!(
        files.iter().any(|p| p.as_os_str() == "old.rs"),
        "expected old.rs in deletion commit, got: {files:?}"
    );
}

#[test]
fn test_file_modification_appears_in_changed_files() {
    if !git_available() {
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(dir.path(), "lib.rs", "v1", "initial"));
    assert!(git_commit_file(dir.path(), "lib.rs", "v2 with more content", "update lib.rs"));

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");
    assert_eq!(history.commits.len(), 2);
    let mod_commit = &history.commits[0]; // newest = modification commit
    let files: Vec<&PathBuf> = mod_commit.changed_files.iter().map(|f| &f.path).collect();
    assert!(
        files.iter().any(|p| p.as_os_str() == "lib.rs"),
        "expected lib.rs in modification commit, got: {files:?}"
    );
}

// ============================================================================
// Metadata
// ============================================================================

#[test]
fn test_commit_count_matches_vec_len() {
    if !git_available() {
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(dir.path(), "a.txt", "a", "one"));
    assert!(git_commit_file(dir.path(), "b.txt", "b", "two"));
    assert!(git_commit_file(dir.path(), "c.txt", "c", "three"));

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");
    assert_eq!(
        history.metadata.commit_count,
        history.commits.len(),
        "metadata.commit_count must equal commits.len()"
    );
    assert_eq!(history.metadata.commit_count, 3);
}

// ============================================================================
// Fix classification
// ============================================================================

#[test]
fn test_is_fix_commit_matches_fix() {
    assert!(is_fix_commit("fix: null pointer dereference"));
    assert!(is_fix_commit("Fix typo in README"));
    assert!(is_fix_commit("FIX: urgent security issue"));
}

#[test]
fn test_is_fix_commit_matches_bug() {
    assert!(is_fix_commit("bug: crash on empty input"));
    assert!(is_fix_commit("BUG: wrong calculation"));
}

#[test]
fn test_is_fix_commit_matches_hotfix() {
    assert!(is_fix_commit("hotfix: production outage"));
    assert!(is_fix_commit("HOTFIX: urgent"));
}

#[test]
fn test_is_fix_commit_matches_patch() {
    assert!(is_fix_commit("patch: minor adjustment"));
    assert!(is_fix_commit("PATCH: something"));
}

#[test]
fn test_is_fix_commit_matches_revert() {
    assert!(is_fix_commit("revert: bad change"));
    assert!(is_fix_commit("Revert \"some feature\""));
    assert!(is_fix_commit("REVERT: rollback"));
}

#[test]
fn test_is_fix_commit_case_insensitive() {
    assert!(is_fix_commit("FIX: something"));
    assert!(is_fix_commit("Bug report addressed"));
    assert!(is_fix_commit("hoTFiX applied"));
}

#[test]
fn test_is_fix_commit_word_boundary() {
    // "prefix" and "suffix" should not match "fix" due to word boundary
    assert!(!is_fix_commit("prefix the thing"));
    assert!(!is_fix_commit("bugfix: this is a compound"));
    assert!(!is_fix_commit("hotfixing something"));
}

#[test]
fn test_is_fix_commit_no_match() {
    assert!(!is_fix_commit("add new feature"));
    assert!(!is_fix_commit("refactor: improve readability"));
    assert!(!is_fix_commit("feat: initial implementation"));
    assert!(!is_fix_commit("chore: update dependencies"));
}

// ============================================================================
// Trait & type safety
// ============================================================================

#[test]
fn test_temporal_source_is_object_safe() {
    // This test exists to ensure the trait compiles as a trait object.
    // If TemporalSource is not object-safe, this won't compile.
    fn accepts_trait_object(_: &dyn TemporalSource) {}
    let src = GixSource;
    accepts_trait_object(&src);
}

#[test]
fn test_gix_source_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<GixSource>();
}

#[test]
fn test_commit_info_serialization_roundtrip() {
    use crate::types::{CommitInfo, FileChangeInfo};

    let original = CommitInfo {
        hash: "a".repeat(40),
        timestamp: 1_700_000_000,
        author: "Alice".to_string(),
        message: "feat: something".to_string(),
        changed_files: vec![FileChangeInfo {
            path: PathBuf::from("src/main.rs"),
            additions: 10,
            deletions: 3,
        }],
    };

    let json = serde_json::to_string(&original).expect("serialize");
    let restored: CommitInfo = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(restored.hash, original.hash);
    assert_eq!(restored.timestamp, original.timestamp);
    assert_eq!(restored.author, original.author);
    assert_eq!(restored.message, original.message);
    assert_eq!(restored.changed_files.len(), 1);
    assert_eq!(restored.changed_files[0].path, original.changed_files[0].path);
    assert_eq!(restored.changed_files[0].additions, 10);
    assert_eq!(restored.changed_files[0].deletions, 3);
}
