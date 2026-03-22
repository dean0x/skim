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
fn test_skim_git_status_porcelain_passthrough() {
    // --porcelain triggers passthrough — output should NOT contain [status] prefix
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "status", "--porcelain"])
        .assert()
        .success();
    // Passthrough output is raw git output; we just verify it doesn't crash
}

#[test]
fn test_skim_git_status_short_passthrough() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "status", "-s"])
        .assert()
        .success();
}

// ============================================================================
// Diff
// ============================================================================

#[test]
fn test_skim_git_diff_in_repo() {
    // Clean repo may have no diff — that's fine, should still succeed
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "diff"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[diff]"));
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
fn test_skim_git_log_oneline_passthrough() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "log", "--oneline", "-n", "3"])
        .assert()
        .success();
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
