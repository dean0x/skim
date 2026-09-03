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
        eprintln!("SKIPPED: git not available on PATH");
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
        eprintln!("SKIPPED: git not available on PATH");
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(
        dir.path(),
        "hello.txt",
        "world",
        "feat: first commit"
    ));

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");
    assert_eq!(history.commits.len(), 1);

    let commit = &history.commits[0];
    // Hash should be 40 hex chars
    assert_eq!(
        commit.hash.len(),
        40,
        "hash should be 40 chars: {}",
        commit.hash
    );
    assert!(
        commit.hash.chars().all(|c| c.is_ascii_hexdigit()),
        "hash should be hex: {}",
        commit.hash
    );
    assert!(commit.timestamp > 0, "timestamp should be positive");
    assert!(!commit.author.is_empty(), "author should be non-empty");
    assert_eq!(commit.message, "feat: first commit");
    assert!(
        !history.metadata.is_shallow,
        "normal repo should not be shallow"
    );
}

#[test]
fn test_shallow_clone_detected() {
    if !git_available() {
        eprintln!("SKIPPED: git not available on PATH");
        return;
    }
    let Some(origin) = init_git_repo() else {
        return;
    };
    assert!(git_commit_file(origin.path(), "a.txt", "a", "first"));
    assert!(git_commit_file(origin.path(), "b.txt", "b", "second"));
    assert!(git_commit_file(origin.path(), "c.txt", "c", "third"));

    let shallow_dir = tempfile::tempdir().expect("tempdir");
    let origin_url = format!("file://{}", origin.path().display());
    let ok = Command::new("git")
        .args(["clone", "--depth", "1"])
        .arg(&origin_url)
        .arg(shallow_dir.path().join("repo"))
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        return;
    }

    let src = GixSource;
    let history = src
        .parse_history(&shallow_dir.path().join("repo"), 0)
        .expect("shallow parse");
    assert!(
        history.metadata.is_shallow,
        "shallow clone should be detected"
    );
    assert!(
        !history.commits.is_empty(),
        "shallow clone should still return available commits"
    );
}

#[test]
fn test_multiple_commits_ordering() {
    if !git_available() {
        eprintln!("SKIPPED: git not available on PATH");
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
        eprintln!("SKIPPED: git not available on PATH");
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(
        dir.path(),
        "main.rs",
        "fn main(){}",
        "add main"
    ));

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
        eprintln!("SKIPPED: git not available on PATH");
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
        eprintln!("SKIPPED: git not available on PATH");
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(dir.path(), "a.txt", "a", "old commit"));
    assert!(git_commit_file(dir.path(), "b.txt", "b", "new commit"));

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");
    assert_eq!(
        history.commits.len(),
        2,
        "lookback_days=0 should return all commits"
    );
}

#[test]
fn test_lookback_large_value_returns_recent() {
    if !git_available() {
        eprintln!("SKIPPED: git not available on PATH");
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(dir.path(), "a.txt", "a", "commit A"));
    assert!(git_commit_file(dir.path(), "b.txt", "b", "commit B"));

    let src = GixSource;
    // lookback_days=365 should include commits from the last year (both are very recent)
    let history = src.parse_history(dir.path(), 365).expect("parse_history");
    assert_eq!(
        history.commits.len(),
        2,
        "both recent commits should be within 365 days"
    );
}

#[test]
fn test_lookback_excludes_old_commits() {
    if !git_available() {
        eprintln!("SKIPPED: git not available on PATH");
        return;
    }
    let Some(dir) = init_git_repo() else { return };

    // Create a commit dated 180 days ago via GIT_AUTHOR_DATE / GIT_COMMITTER_DATE.
    let old_date = "2000-01-01T00:00:00+0000";
    std::fs::write(dir.path().join("old.txt"), "old content").unwrap();
    let staged = Command::new("git")
        .args(["add", "old.txt"])
        .current_dir(dir.path())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(staged, "git add failed");
    let committed = Command::new("git")
        .args(["commit", "-m", "old commit"])
        .env("GIT_AUTHOR_DATE", old_date)
        .env("GIT_COMMITTER_DATE", old_date)
        .current_dir(dir.path())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(committed, "git commit with old date failed");

    // Also create a recent commit so the repo isn't empty after filtering.
    assert!(git_commit_file(
        dir.path(),
        "recent.txt",
        "new",
        "recent commit"
    ));

    let src = GixSource;
    // lookback_days=30 should include only the recent commit; the 2000-dated
    // commit is ~9000 days in the past and must be excluded.
    let history = src.parse_history(dir.path(), 30).expect("parse_history");
    assert_eq!(
        history.commits.len(),
        1,
        "expected only 1 recent commit within 30-day window, got: {:?}",
        history
            .commits
            .iter()
            .map(|c| &c.message)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        history.commits[0].message, "recent commit",
        "the surviving commit should be the recent one"
    );
}

// ============================================================================
// File tracking
// ============================================================================

#[test]
fn test_file_addition_appears_in_changed_files() {
    if !git_available() {
        eprintln!("SKIPPED: git not available on PATH");
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(
        dir.path(),
        "new_feature.rs",
        "pub fn feature(){}",
        "add feature"
    ));

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");
    let files: Vec<&PathBuf> = history.commits[0]
        .changed_files
        .iter()
        .map(|f| &f.path)
        .collect();
    assert!(
        files.iter().any(|p| p.as_os_str() == "new_feature.rs"),
        "expected new_feature.rs in changed_files, got: {files:?}"
    );
}

#[test]
fn test_file_deletion_appears_in_changed_files() {
    if !git_available() {
        eprintln!("SKIPPED: git not available on PATH");
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(
        dir.path(),
        "old.rs",
        "content",
        "add old.rs"
    ));
    assert!(git_delete_file(dir.path(), "old.rs", "delete old.rs"));

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");
    assert_eq!(history.commits.len(), 2);
    // The deletion commit (newest, commits[0]) should mention old.rs
    let delete_commit = &history.commits[0];
    let files: Vec<&PathBuf> = delete_commit
        .changed_files
        .iter()
        .map(|f| &f.path)
        .collect();
    assert!(
        files.iter().any(|p| p.as_os_str() == "old.rs"),
        "expected old.rs in deletion commit, got: {files:?}"
    );
}

#[test]
fn test_file_modification_appears_in_changed_files() {
    if !git_available() {
        eprintln!("SKIPPED: git not available on PATH");
        return;
    }
    let Some(dir) = init_git_repo() else { return };
    assert!(git_commit_file(dir.path(), "lib.rs", "v1", "initial"));
    assert!(git_commit_file(
        dir.path(),
        "lib.rs",
        "v2 with more content",
        "update lib.rs"
    ));

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

#[test]
fn test_file_rename_appears_in_changed_files() {
    if !git_available() {
        eprintln!("SKIPPED: git not available on PATH");
        return;
    }
    let Some(dir) = init_git_repo() else { return };

    // Create original file and commit it.
    assert!(git_commit_file(
        dir.path(),
        "original.rs",
        "pub fn hello(){}",
        "add original.rs"
    ));

    // Rename via `git mv` and commit.
    let moved = Command::new("git")
        .args(["mv", "original.rs", "renamed.rs"])
        .current_dir(dir.path())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(moved, "git mv failed");
    let committed = Command::new("git")
        .args(["commit", "-m", "rename original.rs to renamed.rs"])
        .current_dir(dir.path())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(committed, "git commit after rename failed");

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");
    assert_eq!(history.commits.len(), 2);

    // The rename commit is newest (commits[0]).
    let rename_commit = &history.commits[0];
    let files: Vec<&PathBuf> = rename_commit
        .changed_files
        .iter()
        .map(|f| &f.path)
        .collect();
    assert!(
        files.iter().any(|p| p.as_os_str() == "renamed.rs"),
        "expected renamed.rs (new path) in rename commit changed_files, got: {files:?}"
    );
}

// ============================================================================
// Metadata
// ============================================================================

#[test]
fn test_commit_count_matches_vec_len() {
    if !git_available() {
        eprintln!("SKIPPED: git not available on PATH");
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
    assert_eq!(
        restored.changed_files[0].path,
        original.changed_files[0].path
    );
    assert_eq!(restored.changed_files[0].additions, 10);
    assert_eq!(restored.changed_files[0].deletions, 3);
}

// ============================================================================
// Full-DAG walk — merge skip and budget (Test Plan items 1-9, #407)
// ============================================================================

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// Create a repo with a feature branch merged in (no-ff).
///
/// Layout (non-merge commits = 4, a.txt touched 2×, b.txt touched 2×):
///   main:    A1 (a.txt, "chore: initial a") → A2 (a.txt, "chore: update a")
///   feature: B1 (b.txt, "fix: bug one")    → B2 (b.txt, "fix: bug two")
///   HEAD:    M (merge(#1): feature → main, 2 parents)
///
/// The merge commit's subject never matches FIX_REGEX. All four non-merge
/// commits have deterministic dates so concurrent test runs are stable (PF-012).
fn init_merge_fixture() -> Option<TempDir> {
    let dir = init_git_repo()?;
    let p = dir.path();

    // Pinned base time so branches don't race (PF-012)
    const T0: i64 = 1_700_000_000;

    // A1: root commit on main
    if !git_commit_file_at(p, "a.txt", "v1", "chore: initial a", T0, T0) {
        return None;
    }
    // Branch off to feature
    if !Command::new("git")
        .args(["checkout", "-b", "feature"])
        .current_dir(p)
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return None;
    }
    // B1, B2: two fix commits on feature
    if !git_commit_file_at(p, "b.txt", "v1", "fix: bug one", T0 + 10, T0 + 10) {
        return None;
    }
    if !git_commit_file_at(p, "b.txt", "v2", "fix: bug two", T0 + 20, T0 + 20) {
        return None;
    }
    // Back to main
    if !Command::new("git")
        .args(["checkout", "main"])
        .current_dir(p)
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return None;
    }
    // A2: second commit on main
    if !git_commit_file_at(p, "a.txt", "v2", "chore: update a", T0 + 30, T0 + 30) {
        return None;
    }
    // Merge feature into main (--no-ff to guarantee a merge commit)
    let merge_ok = Command::new("git")
        .args([
            "merge",
            "--no-ff",
            "-m",
            "merge(#1): feature into main",
            "feature",
        ])
        .current_dir(p)
        .output()
        .is_ok_and(|o| o.status.success());
    if !merge_ok {
        return None;
    }

    Some(dir)
}

/// Add a file and commit with pinned `author_ts` and `committer_ts` timestamps
/// (Unix seconds). Both dates are set independently; pass equal values for a
/// uniform timestamp.
fn git_commit_file_at(
    dir: &Path,
    filename: &str,
    content: &str,
    message: &str,
    author_ts: i64,
    committer_ts: i64,
) -> bool {
    // Format as git's expected "seconds timezone" string
    let author_date = format!("{} +0000", author_ts);
    let committer_date = format!("{} +0000", committer_ts);
    std::fs::write(dir.join(filename), content).is_ok()
        && Command::new("git")
            .args(["add", filename])
            .current_dir(dir)
            .output()
            .is_ok_and(|o| o.status.success())
        && Command::new("git")
            .args(["commit", "-m", message])
            .env("GIT_AUTHOR_DATE", &author_date)
            .env("GIT_COMMITTER_DATE", &committer_date)
            .current_dir(dir)
            .output()
            .is_ok_and(|o| o.status.success())
}

/// Run `git rev-list --count --no-merges HEAD` and return the count.
fn git_rev_list_count_no_merges(dir: &Path) -> Option<usize> {
    Command::new("git")
        .args(["rev-list", "--count", "--no-merges", "HEAD"])
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
}

/// Run `git rev-list --count --no-merges --full-history HEAD -- <path>`.
fn git_rev_list_count_no_merges_for_path(dir: &Path, path: &str) -> Option<usize> {
    Command::new("git")
        .args([
            "rev-list",
            "--count",
            "--no-merges",
            "--full-history",
            "HEAD",
            "--",
            path,
        ])
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
}

/// Run `git rev-parse HEAD` and return the full SHA.
fn git_rev_parse_head(dir: &Path) -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
}

// ---------------------------------------------------------------------------
// T-1: total equals git rev-list --count --no-merges
// ---------------------------------------------------------------------------

/// AC-1: parse_history must return exactly git rev-list --count --no-merges HEAD
/// commits on the merge fixture (derived in-test, never hardcoded — ADR-003).
#[test]
fn test_merge_repo_total_equals_git_rev_list_no_merges() {
    if !git_available() {
        eprintln!("SKIPPED: git not available");
        return;
    }
    let Some(dir) = init_merge_fixture() else {
        return;
    };
    let Some(expected) = git_rev_list_count_no_merges(dir.path()) else {
        eprintln!("SKIPPED: git rev-list failed");
        return;
    };

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");

    assert_eq!(
        history.commits.len(),
        expected,
        "parse_history must return exactly git rev-list --count --no-merges HEAD ({expected}) commits"
    );
    assert_eq!(history.metadata.commit_count, expected);
}

// ---------------------------------------------------------------------------
// T-2: branch commit subjects appear verbatim
// ---------------------------------------------------------------------------

/// AC-4: branch commit messages ("fix: bug one", "fix: bug two") must appear
/// verbatim in the returned CommitInfo.message, so is_fix_commit can classify
/// them correctly rather than receiving merge subjects.
#[test]
fn test_branch_commits_present_with_real_subjects() {
    if !git_available() {
        eprintln!("SKIPPED: git not available");
        return;
    }
    let Some(dir) = init_merge_fixture() else {
        return;
    };

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");

    let messages: Vec<&str> = history.commits.iter().map(|c| c.message.as_str()).collect();
    assert!(
        messages.contains(&"fix: bug one"),
        "expected \"fix: bug one\" in messages: {messages:?}"
    );
    assert!(
        messages.contains(&"fix: bug two"),
        "expected \"fix: bug two\" in messages: {messages:?}"
    );
}

// ---------------------------------------------------------------------------
// T-3: merge commit absent from history
// ---------------------------------------------------------------------------

/// AC-3: no CommitInfo.hash must equal the merge commit's SHA (HEAD on the fixture).
#[test]
fn test_merge_commit_absent_from_history() {
    if !git_available() {
        eprintln!("SKIPPED: git not available");
        return;
    }
    let Some(dir) = init_merge_fixture() else {
        return;
    };
    let Some(head_sha) = git_rev_parse_head(dir.path()) else {
        eprintln!("SKIPPED: git rev-parse HEAD failed");
        return;
    };

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");

    let merge_in_result = history.commits.iter().any(|c| c.hash == head_sha);
    assert!(
        !merge_in_result,
        "merge commit {head_sha} must NOT appear in parse_history results"
    );
}

// ---------------------------------------------------------------------------
// T-4: per-file counts match git rev-list --no-merges
// ---------------------------------------------------------------------------

/// AC-2: for each path the skim touch count must equal
/// git rev-list --count --no-merges --full-history HEAD -- <path> (ADR-003).
#[test]
fn test_per_file_counts_match_git_log_no_merges() {
    if !git_available() {
        eprintln!("SKIPPED: git not available");
        return;
    }
    let Some(dir) = init_merge_fixture() else {
        return;
    };

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse_history");

    for path in &["a.txt", "b.txt"] {
        let Some(expected) = git_rev_list_count_no_merges_for_path(dir.path(), path) else {
            eprintln!("SKIPPED: git rev-list for {path} failed");
            continue;
        };
        let skim_count = history
            .commits
            .iter()
            .filter(|c| {
                c.changed_files
                    .iter()
                    .any(|f| f.path.to_str() == Some(path))
            })
            .count();
        assert_eq!(
            skim_count, expected,
            "skim touch count for {path} ({skim_count}) != git --no-merges count ({expected})"
        );
    }
}

// ---------------------------------------------------------------------------
// T-5: octopus merge is skipped
// ---------------------------------------------------------------------------

/// AC-3 (octopus): a 3-parent octopus merge must not appear in the history.
/// All branch commits must be present.
#[test]
fn test_octopus_merge_is_skipped() {
    if !git_available() {
        eprintln!("SKIPPED: git not available");
        return;
    }
    let Some(dir) = init_git_repo() else {
        return;
    };
    let p = dir.path();
    const T0: i64 = 1_700_100_000;

    // Root commit on main
    if !git_commit_file_at(p, "shared.txt", "v1", "chore: init", T0, T0) {
        return;
    }

    // Branch b1
    if !Command::new("git")
        .args(["checkout", "-b", "b1"])
        .current_dir(p)
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    if !git_commit_file_at(p, "b1.txt", "x", "feat: b1 work", T0 + 10, T0 + 10) {
        return;
    }

    // Branch b2 from main
    if !Command::new("git")
        .args(["checkout", "main"])
        .current_dir(p)
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    if !Command::new("git")
        .args(["checkout", "-b", "b2"])
        .current_dir(p)
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    if !git_commit_file_at(p, "b2.txt", "y", "feat: b2 work", T0 + 20, T0 + 20) {
        return;
    }

    // Back to main, octopus merge of both branches
    if !Command::new("git")
        .args(["checkout", "main"])
        .current_dir(p)
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    let octopus_ok = Command::new("git")
        .args([
            "merge",
            "--no-ff",
            "-m",
            "merge(octopus): b1 b2",
            "b1",
            "b2",
        ])
        .current_dir(p)
        .output()
        .is_ok_and(|o| o.status.success());
    if !octopus_ok {
        eprintln!("SKIPPED: octopus merge failed (git may not support it)");
        return;
    }

    let Some(head_sha) = git_rev_parse_head(p) else {
        return;
    };

    let src = GixSource;
    let history = src.parse_history(p, 0).expect("parse_history");

    // Octopus merge must be absent
    assert!(
        !history.commits.iter().any(|c| c.hash == head_sha),
        "octopus merge {head_sha} must NOT appear in parse_history"
    );

    // All branch commits must be present
    let messages: Vec<&str> = history.commits.iter().map(|c| c.message.as_str()).collect();
    assert!(
        messages.contains(&"feat: b1 work"),
        "branch b1 commit must be present: {messages:?}"
    );
    assert!(
        messages.contains(&"feat: b2 work"),
        "branch b2 commit must be present: {messages:?}"
    );
}

// ---------------------------------------------------------------------------
// T-6: cutoff uses committer time, not author time (AD-407-3)
// ---------------------------------------------------------------------------

/// AC-7: a commit with old GIT_AUTHOR_DATE but recent GIT_COMMITTER_DATE must
/// be included when lookback_days covers the committer date, and excluded when
/// both dates are old. This verifies the manual author-date guard was removed.
#[test]
fn test_cutoff_uses_committer_time_not_author_time() {
    if !git_available() {
        eprintln!("SKIPPED: git not available");
        return;
    }
    let Some(dir) = init_git_repo() else {
        return;
    };
    let p = dir.path();

    // "Rebased" commit: very old author date, but committer date = now
    let ancient_author_ts: i64 = 946_684_800; // 2000-01-01 UTC
    // committer date = 5 days ago (well within a 30-day window)
    let recent_committer_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        - 5 * 86_400;

    if !git_commit_file_at(
        p,
        "rebased.txt",
        "content",
        "feat: rebased commit",
        ancient_author_ts,
        recent_committer_ts,
    ) {
        return;
    }

    // With 30-day window: committer date is recent → commit MUST be returned
    let src = GixSource;
    let history_30d = src.parse_history(p, 30).expect("parse_history 30d");
    assert!(
        !history_30d.commits.is_empty(),
        "commit with old author date but recent committer date must be returned with lookback_days=30"
    );
    assert!(
        history_30d
            .commits
            .iter()
            .any(|c| c.message == "feat: rebased commit"),
        "rebased commit must appear in 30-day window"
    );

    // Now create a second repo where BOTH dates are ancient
    let Some(dir2) = init_git_repo() else {
        return;
    };
    if !git_commit_file_at(
        dir2.path(),
        "old.txt",
        "content",
        "chore: ancient commit",
        ancient_author_ts,
        ancient_author_ts, // committer date also old
    ) {
        return;
    }

    let history_both_old = src
        .parse_history(dir2.path(), 30)
        .expect("parse_history both old");
    assert!(
        history_both_old.commits.is_empty(),
        "commit with old author AND committer dates must be excluded with lookback_days=30; \
         got: {:?}",
        history_both_old
            .commits
            .iter()
            .map(|c| &c.message)
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// T-7: shallow clone whose HEAD is a merge yields no commits
// ---------------------------------------------------------------------------

/// After #407's merge skip, a --depth 1 clone whose HEAD is a merge commit
/// yields 0 CommitInfos because the only visited commit is a merge.
#[test]
fn test_shallow_clone_with_merge_head_yields_no_commits() {
    if !git_available() {
        eprintln!("SKIPPED: git not available");
        return;
    }
    let Some(origin) = init_merge_fixture() else {
        return;
    };

    let shallow_dir = tempfile::tempdir().expect("tempdir");
    let clone_target = shallow_dir.path().join("repo");
    let origin_url = format!("file://{}", origin.path().display());

    let clone_ok = Command::new("git")
        .args(["clone", "--depth", "1", &origin_url])
        .arg(&clone_target)
        .output()
        .is_ok_and(|o| o.status.success());
    if !clone_ok {
        eprintln!("SKIPPED: git clone --depth 1 failed");
        return;
    }

    let src = GixSource;
    let history = src
        .parse_history(&clone_target, 0)
        .expect("parse_history shallow");

    assert!(
        history.metadata.is_shallow,
        "clone must be detected as shallow"
    );
    assert_eq!(
        history.commits.len(),
        0,
        "shallow clone whose HEAD is a merge must yield 0 CommitInfos after merge skip"
    );
}

// ---------------------------------------------------------------------------
// T-8: ordering contract — sort by author time, not committer time (AC-6)
// ---------------------------------------------------------------------------

/// AC-6: parse_history must stably sort by CommitInfo.timestamp (author time)
/// descending, so commits with old author dates but recent committer dates
/// sort LAST, not first (stable sort preserves traversal order for ties).
#[test]
fn test_ordering_contract_committer_time_not_author_time() {
    if !git_available() {
        eprintln!("SKIPPED: git not available");
        return;
    }
    let Some(dir) = init_git_repo() else {
        return;
    };
    let p = dir.path();

    // Commit A: newer AUTHOR date, older COMMITTER date
    // Commit B: older AUTHOR date, newer COMMITTER date
    // After stable sort by author time descending: A must come first.
    const T_OLD_AUTHOR: i64 = 1_600_000_000;
    const T_NEW_AUTHOR: i64 = 1_700_000_000;
    const T_OLD_COMMITTER: i64 = 1_600_000_100;
    const T_NEW_COMMITTER: i64 = 1_700_000_100;

    // Commit A first so it's visited second by gix (gix visits newest committer first)
    if !git_commit_file_at(
        p,
        "a.txt",
        "a",
        "feat: commit A (old committer, new author)",
        T_NEW_AUTHOR,
        T_OLD_COMMITTER,
    ) {
        return;
    }
    // Commit B: old author, new committer → gix visits this FIRST (newer committer)
    if !git_commit_file_at(
        p,
        "b.txt",
        "b",
        "feat: commit B (new committer, old author)",
        T_OLD_AUTHOR,
        T_NEW_COMMITTER,
    ) {
        return;
    }

    let src = GixSource;
    let history = src.parse_history(p, 0).expect("parse_history");
    assert_eq!(history.commits.len(), 2, "expected 2 commits");

    // After stable sort by author time (CommitInfo.timestamp) descending:
    // commits[0] must be A (newer author time T_NEW_AUTHOR)
    // commits[1] must be B (older author time T_OLD_AUTHOR)
    assert_eq!(
        history.commits[0].message,
        "feat: commit A (old committer, new author)",
        "commits[0] must be A (newer author time); actual ordering: {:?}",
        history
            .commits
            .iter()
            .map(|c| &c.message)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        history.commits[1].message, "feat: commit B (new committer, old author)",
        "commits[1] must be B (older author time)"
    );
    assert!(
        history.commits[0].timestamp > history.commits[1].timestamp,
        "timestamps must be non-increasing (newest first): {} < {}",
        history.commits[0].timestamp,
        history.commits[1].timestamp
    );
}

// ---------------------------------------------------------------------------
// T-9: walk budget bounds (AC-8)
// ---------------------------------------------------------------------------

/// AC-8: WalkBudget::charge_retain and charge_visit must trip at their
/// respective caps. Both bounds are driven directly without constructing a
/// large repository (unit-testable by design).
#[test]
fn test_walk_budget_bounds() {
    use super::{MAX_COMMITS, MAX_VISITED_COMMITS, WalkBudget};

    // Compile-time guard is already in production code via `const _: () = assert!(...)`.
    // Verify the values in a const block so a regression is caught at compile time here too.
    const { assert!(MAX_VISITED_COMMITS >= MAX_COMMITS) };
    assert_eq!(
        MAX_VISITED_COMMITS,
        4 * MAX_COMMITS,
        "MAX_VISITED_COMMITS must be 4× MAX_COMMITS per the plan decision"
    );

    // --- Retain bound ---
    // charge_retain() should return false for the first MAX_COMMITS calls,
    // then true on the (MAX_COMMITS+1)th call.
    {
        let mut budget = WalkBudget::new();
        for i in 0..MAX_COMMITS {
            assert!(
                !budget.charge_retain(),
                "charge_retain must return false on call {i} (below cap)"
            );
        }
        assert!(
            budget.charge_retain(),
            "charge_retain must return true after {MAX_COMMITS} charges (cap reached)"
        );
    }

    // --- Visit bound ---
    // charge_visit() increments first, then checks. It fires when visited
    // strictly exceeds MAX_VISITED_COMMITS, i.e. on the (MAX_VISITED_COMMITS+1)th call.
    {
        let mut budget = WalkBudget::new();
        for i in 1..=MAX_VISITED_COMMITS {
            assert!(
                !budget.charge_visit(),
                "charge_visit must return false on call {i} (below cap)"
            );
        }
        assert!(
            budget.charge_visit(),
            "charge_visit must return true on call {} (cap exceeded)",
            MAX_VISITED_COMMITS + 1
        );
    }
}
