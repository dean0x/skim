//! Integration tests for `skim git` subcommand (#50).
//!
//! Tests end-to-end CLI behavior for git status/diff/log compression.
//! These tests run against the real skim repository (which is a git repo),
//! so they exercise the actual git binary.

use assert_cmd::Command;
use predicates::prelude::*;

// ============================================================================
// Help
// ============================================================================

#[test]
fn test_skim_git_help() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("diff"))
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("log"));
}

#[test]
fn test_skim_git_no_args_shows_help() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("git")
        .assert()
        .success()
        .stdout(predicate::str::contains("status"));
}

// ============================================================================
// Status
// ============================================================================

#[test]
fn test_skim_git_status_in_repo() {
    // Run against the skim repo itself — should succeed
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "status"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("[status]")
                .and(predicate::str::contains("branch").or(predicate::str::contains("clean"))),
        );
}

#[test]
fn test_skim_git_status_porcelain_compresses() {
    // --porcelain is now stripped by the handler; output is still compressed.
    // The [status] prefix confirms the handler ran (not raw passthrough).
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "status", "--porcelain"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("[status]")
                .and(predicate::str::contains("branch").or(predicate::str::contains("clean"))),
        );
}

#[test]
fn test_skim_git_status_short_compresses() {
    // -s is now stripped by the handler; output is still compressed.
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "status", "-s"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("[status]")
                .and(predicate::str::contains("branch").or(predicate::str::contains("clean"))),
        );
}

// ============================================================================
// Diff
// ============================================================================

#[test]
fn test_skim_git_diff_in_repo() {
    // Clean repo has no diff — AST-aware pipeline outputs "No changes" to stderr
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "diff"])
        .assert()
        .success();
}

#[test]
fn test_skim_git_diff_name_only_passthrough() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "diff", "--name-only"])
        .assert()
        .success();
}

// ============================================================================
// Log
// ============================================================================

#[test]
fn test_skim_git_log_in_repo() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "log"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[log]").and(predicate::str::contains("commit")));
}

#[test]
fn test_skim_git_log_with_limit() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "log", "-n", "3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[log]"));
}

#[test]
fn test_skim_git_log_oneline_compresses() {
    // --oneline is now stripped by the handler; the log is still compressed.
    // The [log] prefix confirms the handler ran (not raw passthrough).
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "log", "--oneline", "-n", "3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[log]").and(predicate::str::contains("commit")));
}

// ============================================================================
// Fetch
// ============================================================================

/// Run `skim git fetch` against the skim repo. Since skim may have no
/// configured remotes or may be up-to-date, we accept either "[fetch]" output
/// or "up to date".
#[test]
fn test_skim_git_fetch_in_repo() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "fetch"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[fetch]").or(predicate::str::contains("up to date")));
}

// ============================================================================
// Error cases
// ============================================================================

#[test]
fn test_skim_git_unknown_subcommand() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "unknown_subcmd"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown git subcommand"));
}

// ============================================================================
// Step 7a: Real `git status` E2E tests — previously-skipped flags now compress
// ============================================================================

#[test]
fn test_skim_git_status_with_short_flag_compresses() {
    // -s was previously a skip flag causing passthrough. Handler now strips it
    // and runs compressed output.
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "status", "-s"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[status]"));
}

#[test]
fn test_skim_git_status_with_porcelain_flag_compresses() {
    // --porcelain was previously a skip flag. Handler now strips it.
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "status", "--porcelain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[status]"));
}

#[test]
fn test_skim_git_status_with_short_long_flag_compresses() {
    // --short was previously a skip flag. Handler now strips it.
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "status", "--short"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[status]"));
}

// ============================================================================
// Step 7b: Real `git log` E2E tests — --oneline now compresses
// ============================================================================

#[test]
fn test_skim_git_log_oneline_flag_compresses() {
    // --oneline was previously a skip flag causing passthrough. Handler now
    // strips it and runs compressed output.
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "log", "--oneline", "-5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[log]").and(predicate::str::contains("commit")));
}

#[test]
fn test_skim_git_log_contains_hashes() {
    // Compressed log output should contain commit hashes (short 7-char hex).
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "log", "-n", "1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // git log format is "%h %s (%cr) <%an>" — first word is the short hash
    assert!(
        stdout.contains("[log]"),
        "Expected [log] prefix in output, got: {stdout}"
    );
    // Verify at least one line looks like a commit (7-char hex prefix)
    let has_hash = stdout.lines().filter(|l| !l.starts_with('[')).any(|l| {
        l.split_whitespace()
            .next()
            .is_some_and(|w| w.len() >= 7 && w.chars().all(|c| c.is_ascii_hexdigit()))
    });
    assert!(
        has_hash,
        "Expected a line with a hex commit hash in output, got: {stdout}"
    );
}
