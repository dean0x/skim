//! Integration tests for `temporal::git_parser::parse_history`.
//!
//! Each test builds a real git repo in a `TempDir` using the git CLI, then
//! calls `parse_history` and asserts on the returned `CommitInfo` slice.
//!
//! No internal state is probed — all assertions go through the public API.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod temporal_test_helpers;
use temporal_test_helpers::{build_fixture_repo, run_git, run_git_with_env, FixtureCommit};

use std::path::PathBuf;

use rskim_search::temporal::parse_history;
use tempfile::TempDir;

// ============================================================================
// Helpers
// ============================================================================

/// Seconds per day — used for computing timestamp overrides.
const DAY_SECS: i64 = 86_400;

/// Return the current UTC time as Unix epoch seconds.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time before Unix epoch")
        .as_secs() as i64
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn parse_empty_repo_returns_empty_vec() {
    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);

    let result = parse_history(dir.path(), 365).expect("parse_history failed");
    assert!(
        result.is_empty(),
        "expected empty vec for repo with no commits"
    );
}

#[test]
fn parse_single_commit_captures_file() {
    let dir = TempDir::new().expect("tempdir");
    build_fixture_repo(
        dir.path(),
        &[FixtureCommit {
            message: "add foo",
            changes: vec![("foo.rs", "fn main() {}")],
            timestamp_override: None,
        }],
    );

    let result = parse_history(dir.path(), 365).expect("parse_history failed");
    assert_eq!(result.len(), 1, "expected 1 commit");

    let commit = &result[0];
    assert_eq!(commit.message, "add foo");
    assert!(!commit.is_fix);
    assert_eq!(commit.changed_files, vec![PathBuf::from("foo.rs")]);
}

#[test]
fn parse_multiple_commits_reverse_chronological() {
    let dir = TempDir::new().expect("tempdir");
    build_fixture_repo(
        dir.path(),
        &[
            FixtureCommit {
                message: "first commit",
                changes: vec![("a.rs", "a")],
                timestamp_override: None,
            },
            FixtureCommit {
                message: "second commit",
                changes: vec![("b.rs", "b")],
                timestamp_override: None,
            },
            FixtureCommit {
                message: "third commit",
                changes: vec![("c.rs", "c")],
                timestamp_override: None,
            },
        ],
    );

    let result = parse_history(dir.path(), 365).expect("parse_history failed");
    assert_eq!(result.len(), 3, "expected 3 commits");

    // Results are in reverse-chronological order (newest first).
    assert_eq!(result[0].message, "third commit");
    assert_eq!(result[1].message, "second commit");
    assert_eq!(result[2].message, "first commit");
}

#[test]
fn parse_fix_commit_message_detected() {
    let dir = TempDir::new().expect("tempdir");
    build_fixture_repo(
        dir.path(),
        &[FixtureCommit {
            message: "fix: login bug",
            changes: vec![("auth.rs", "// fixed")],
            timestamp_override: None,
        }],
    );

    let result = parse_history(dir.path(), 365).expect("parse_history failed");
    assert_eq!(result.len(), 1);
    assert!(
        result[0].is_fix,
        "expected is_fix=true for 'fix: login bug'"
    );
}

#[test]
fn parse_non_fix_message() {
    let dir = TempDir::new().expect("tempdir");
    build_fixture_repo(
        dir.path(),
        &[FixtureCommit {
            message: "add feature",
            changes: vec![("feature.rs", "// new")],
            timestamp_override: None,
        }],
    );

    let result = parse_history(dir.path(), 365).expect("parse_history failed");
    assert_eq!(result.len(), 1);
    assert!(!result[0].is_fix, "expected is_fix=false for 'add feature'");
}

#[test]
fn parse_word_boundary_prefix() {
    let dir = TempDir::new().expect("tempdir");
    build_fixture_repo(
        dir.path(),
        &[FixtureCommit {
            message: "prefixing types for clarity",
            changes: vec![("types.rs", "// typed")],
            timestamp_override: None,
        }],
    );

    let result = parse_history(dir.path(), 365).expect("parse_history failed");
    assert_eq!(result.len(), 1);
    // "fix" appears inside "prefixing" — must NOT match (word-boundary check).
    assert!(
        !result[0].is_fix,
        "expected is_fix=false: 'fix' is embedded in 'prefixing'"
    );
}

#[test]
fn parse_word_boundary_bugatti() {
    let dir = TempDir::new().expect("tempdir");
    build_fixture_repo(
        dir.path(),
        &[FixtureCommit {
            message: "rename bugatti to car",
            changes: vec![("car.rs", "// renamed")],
            timestamp_override: None,
        }],
    );

    let result = parse_history(dir.path(), 365).expect("parse_history failed");
    assert_eq!(result.len(), 1);
    // "bug" appears inside "bugatti" — must NOT match (word-boundary check).
    assert!(
        !result[0].is_fix,
        "expected is_fix=false: 'bug' is embedded in 'bugatti'"
    );
}

#[test]
fn parse_lookback_excludes_old_commits() {
    let dir = TempDir::new().expect("tempdir");
    let old_ts = now_secs() - 400 * DAY_SECS; // 400 days ago

    build_fixture_repo(
        dir.path(),
        &[FixtureCommit {
            message: "ancient commit",
            changes: vec![("old.rs", "// old")],
            timestamp_override: Some(old_ts),
        }],
    );

    // lookback_days=365 → cutoff is ~365 days ago, so 400-day-old commit is excluded.
    let result = parse_history(dir.path(), 365).expect("parse_history failed");
    assert!(
        result.is_empty(),
        "expected old commit to be excluded by lookback filter"
    );
}

#[test]
fn parse_lookback_includes_recent_commits() {
    let dir = TempDir::new().expect("tempdir");
    let recent_ts = now_secs() - 5 * DAY_SECS; // 5 days ago

    build_fixture_repo(
        dir.path(),
        &[FixtureCommit {
            message: "recent commit",
            changes: vec![("new.rs", "// new")],
            timestamp_override: Some(recent_ts),
        }],
    );

    let result = parse_history(dir.path(), 365).expect("parse_history failed");
    assert_eq!(result.len(), 1, "expected recent commit to be included");
}

#[test]
fn parse_merge_commit_first_parent_only() {
    let dir = TempDir::new().expect("tempdir");
    let p = dir.path();

    // Initialize repo on main branch.
    run_git(p, &["init", "-q", "-b", "main"]);
    run_git(p, &["config", "user.name", "Test"]);
    run_git(p, &["config", "user.email", "test@example.com"]);

    // First commit on main.
    std::fs::write(p.join("main.rs"), "// main").expect("write");
    run_git(p, &["add", "main.rs"]);
    run_git(p, &["commit", "-q", "-m", "init: main"]);

    // Create a feature branch.
    run_git(p, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(p.join("feature.rs"), "// feature").expect("write");
    run_git(p, &["add", "feature.rs"]);
    run_git(p, &["commit", "-q", "-m", "feat: add feature.rs"]);

    // Switch back to main, add another file, then merge feature.
    run_git(p, &["checkout", "-q", "main"]);
    std::fs::write(p.join("mainline.rs"), "// mainline").expect("write");
    run_git(p, &["add", "mainline.rs"]);
    run_git(p, &["commit", "-q", "-m", "chore: mainline file"]);

    // Merge feature into main (creates a merge commit).
    run_git_with_env(
        p,
        &[
            "merge",
            "--no-ff",
            "-m",
            "merge: feature into main",
            "feature",
        ],
        &[],
    );

    // With first-parent walk, the merge commit's diff is only against its
    // first parent (the "chore: mainline file" commit). That means only
    // feature.rs should appear as changed in the merge commit entry — not
    // mainline.rs (which changed in the prior commit).
    let result = parse_history(p, 365).expect("parse_history failed");

    // Walk should be: merge commit, mainline commit, init commit — 3 total.
    assert_eq!(result.len(), 3, "expected 3 commits on first-parent chain");

    let merge = &result[0];
    assert_eq!(merge.message, "merge: feature into main");
    // The merge commit diff (first-parent only) should include feature.rs.
    assert!(
        merge.changed_files.contains(&PathBuf::from("feature.rs")),
        "merge commit should show feature.rs as changed (brought in from feature branch)"
    );
    // mainline.rs should NOT appear in the merge commit diff.
    assert!(
        !merge.changed_files.contains(&PathBuf::from("mainline.rs")),
        "mainline.rs should not appear in merge commit diff (it was in a prior commit)"
    );
}

#[test]
fn parse_non_git_dir_returns_error() {
    let dir = TempDir::new().expect("tempdir");
    // No `git init` — not a git repo.

    let result = parse_history(dir.path(), 365);
    assert!(result.is_err(), "expected error for non-git directory");

    let err = result.expect_err("should be an error");
    let err_str = err.to_string();
    assert!(
        err_str.contains("Git error"),
        "expected GitError, got: {err_str}"
    );
}

#[test]
fn parse_commit_touching_two_files() {
    let dir = TempDir::new().expect("tempdir");
    build_fixture_repo(
        dir.path(),
        &[FixtureCommit {
            message: "add two files",
            changes: vec![("alpha.rs", "// alpha"), ("beta.rs", "// beta")],
            timestamp_override: None,
        }],
    );

    let result = parse_history(dir.path(), 365).expect("parse_history failed");
    assert_eq!(result.len(), 1);

    let mut changed = result[0].changed_files.clone();
    changed.sort();
    assert_eq!(
        changed,
        vec![PathBuf::from("alpha.rs"), PathBuf::from("beta.rs")],
        "expected both files in changed_files"
    );
}
