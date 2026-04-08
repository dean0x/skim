//! Shared helpers for Wave 2 temporal layer integration tests.
//!
//! Creates deterministic git repos for testing git_parser, cochange, scoring.
//! Uses `std::process::Command` + git CLI (test-only dependency).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, dead_code)]

use std::path::Path;
use std::process::Command;

// ============================================================================
// Time helpers
// ============================================================================

/// Return a Unix epoch timestamp `days_ago` days before now (as seconds).
///
/// Used to seed `FixtureCommit::timestamp_override` so tests exercise
/// temporal windows (30d / 90d hotspot, lookback filter) deterministically.
pub fn recent_ts(days_ago: i64) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_secs() as i64;
    now - days_ago * 86_400
}

// ============================================================================
// Standard fixture repos
// ============================================================================

/// Build a 4-commit fixture repo where `a.rs` and `b.rs` always change together.
///
/// Commits are dated 20, 15, 10, and 5 days ago (all within the 90-day
/// hotspot window). Commit 3 has a `fix:` prefix so both files accumulate
/// risk data. This is the canonical co-change scenario used across storage
/// and acceptance tests.
pub fn build_cochange_fixture(dir: &Path) {
    build_fixture_repo(
        dir,
        &[
            FixtureCommit {
                message: "feat: add a and b",
                changes: vec![("a.rs", "fn a() {}"), ("b.rs", "fn b() {}")],
                timestamp_override: Some(recent_ts(20)),
            },
            FixtureCommit {
                message: "refactor: update a and b",
                changes: vec![("a.rs", "fn a() { 1 }"), ("b.rs", "fn b() { 2 }")],
                timestamp_override: Some(recent_ts(15)),
            },
            FixtureCommit {
                message: "fix: bug in a and b",
                changes: vec![("a.rs", "fn a() { 2 }"), ("b.rs", "fn b() { 3 }")],
                timestamp_override: Some(recent_ts(10)),
            },
            FixtureCommit {
                message: "chore: cleanup a and b",
                changes: vec![("a.rs", "fn a() { 3 }"), ("b.rs", "fn b() { 4 }")],
                timestamp_override: Some(recent_ts(5)),
            },
        ],
    );
}

/// A commit to apply when building a fixture git repo.
pub struct FixtureCommit<'a> {
    /// The commit message.
    pub message: &'a str,
    /// Files to write before committing. Each entry is `(repo-relative path, content)`.
    pub changes: Vec<(&'a str, &'a str)>,
    /// Optional Unix epoch seconds to use as both author and committer date.
    /// If `None`, the current time is used (git default).
    pub timestamp_override: Option<i64>,
}

/// Run a `git` subprocess command in `dir`, panicking on failure.
///
/// Tests-only: `.expect()` is permitted in integration tests per clippy lint
/// config (tests are not library code).
pub fn run_git(dir: &Path, args: &[&str]) {
    run_git_with_env(dir, args, &[]);
}

/// Run a `git` subprocess command in `dir` with additional environment variables,
/// panicking on failure.
pub fn run_git_with_env(dir: &Path, args: &[&str], env_pairs: &[(&str, &str)]) {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com");

    for (k, v) in env_pairs {
        cmd.env(k, v);
    }

    let output = cmd.output().expect("failed to run git");
    if !output.status.success() {
        panic!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Initialize a git repo in `dir` and apply the given fixture commits.
///
/// Each commit writes the specified files and commits with the given message.
/// If `FixtureCommit::timestamp_override` is set, both `GIT_AUTHOR_DATE` and
/// `GIT_COMMITTER_DATE` are set to that Unix epoch timestamp so the commit has
/// a deterministic (and possibly past) date.
pub fn build_fixture_repo(dir: &Path, commits: &[FixtureCommit<'_>]) {
    run_git(dir, &["init", "-q", "-b", "main"]);
    run_git(dir, &["config", "user.name", "Test"]);
    run_git(dir, &["config", "user.email", "test@example.com"]);

    for commit in commits {
        for (path, content) in &commit.changes {
            let full = dir.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).expect("create dir");
            }
            std::fs::write(&full, content).expect("write fixture file");
            run_git(dir, &["add", path]);
        }

        let date_env: Option<String> = commit.timestamp_override.map(|ts| format!("{ts} +0000"));

        match &date_env {
            Some(date) => run_git_with_env(
                dir,
                &["commit", "-q", "-m", commit.message],
                &[
                    ("GIT_AUTHOR_DATE", date.as_str()),
                    ("GIT_COMMITTER_DATE", date.as_str()),
                ],
            ),
            None => run_git(dir, &["commit", "-q", "-m", commit.message]),
        }
    }
}
