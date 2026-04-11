//! Integration tests for `skim rewrite` subcommand (#43).
//!
//! Tests the end-to-end CLI behavior of the rewrite engine, covering
//! standard prefix rewrites, env vars, cargo toolchain, compound commands,
//! git skip-flags, suggest mode, stdin mode, and cat/head/tail handlers.

use assert_cmd::Command;
use predicates::prelude::*;

// ============================================================================
// Standard rewrites
// ============================================================================

#[test]
fn test_rewrite_cargo_test_with_separator() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "cargo", "test", "--", "--nocapture"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo -- --nocapture"));
}

#[test]
fn test_rewrite_ls_no_match() {
    // bare `ls` without flags is not rewritten (no compression benefit)
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "ls"])
        .assert()
        .failure();
}

#[test]
fn test_rewrite_cargo_build() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "cargo", "build"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build cargo"));
}

#[test]
fn test_rewrite_go_test_with_path() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "go", "test", "./..."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test go ./..."));
}

#[test]
fn test_rewrite_pytest_with_flag() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "pytest", "-v"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test pytest -v"));
}

// ============================================================================
// Env vars
// ============================================================================

#[test]
fn test_rewrite_with_env_var() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "RUST_LOG=debug", "cargo", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("RUST_LOG=debug skim test cargo"));
}

// ============================================================================
// Cargo toolchain
// ============================================================================

#[test]
fn test_rewrite_cargo_toolchain_nightly() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "cargo", "+nightly", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo +nightly"));
}

#[test]
fn test_rewrite_env_var_with_toolchain() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "RUST_LOG=debug", "cargo", "+nightly", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "RUST_LOG=debug skim test cargo +nightly",
        ));
}

// ============================================================================
// Compound commands (#45)
// ============================================================================

#[test]
fn test_rewrite_compound_and_and() {
    // Both segments should be rewritten
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "cargo", "test", "&&", "cargo", "build"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo"))
        .stdout(predicate::str::contains("&&"))
        .stdout(predicate::str::contains("skim build cargo"));
}

#[test]
fn test_rewrite_compound_pipe() {
    // Only the first segment (output producer) should be rewritten
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("cargo test | head\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo"))
        .stdout(predicate::str::contains("|"))
        .stdout(predicate::str::contains("head"));
}

#[test]
fn test_rewrite_compound_semicolon() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "cargo", "test", ";", "echo", "done"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo"))
        .stdout(predicate::str::contains(";"))
        .stdout(predicate::str::contains("echo done"));
}

#[test]
fn test_rewrite_compound_bail_on_subshell() {
    // $( triggers bail — exit 1
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("$(command) && cargo test\n")
        .assert()
        .failure();
}

#[test]
fn test_rewrite_compound_suggest_mode() {
    // Suggest mode should include compound: true for compound commands
    Command::cargo_bin("skim")
        .unwrap()
        .args([
            "rewrite",
            "--suggest",
            "cargo",
            "test",
            "&&",
            "cargo",
            "build",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"))
        .stdout(predicate::str::contains("\"compound\":true"));
}

// ============================================================================
// Compound commands — additional coverage (#77)
// ============================================================================

#[test]
fn test_rewrite_compound_or_or() {
    // || operator should work in integration tests
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("cargo test || echo fail\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo"))
        .stdout(predicate::str::contains("||"))
        .stdout(predicate::str::contains("echo fail"));
}

#[test]
fn test_rewrite_compound_no_spaces_around_operator() {
    // Operators without surrounding spaces (e.g., cargo test&&cargo build)
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("cargo test&&cargo build\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo"))
        .stdout(predicate::str::contains("&&"))
        .stdout(predicate::str::contains("skim build cargo"));
}

#[test]
fn test_rewrite_compound_escaped_quotes() {
    // Escaped double quotes inside a quoted string should not break splitting
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("echo \"say \\\"hello\\\"\" && cargo test\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo"));
}

#[test]
fn test_rewrite_compound_mixed_pipe_and_sequential() {
    // Mixed pipe + sequential: cargo test && cargo build | head
    // The pipe causes the entire expression to go through the pipe path,
    // which only rewrites the first segment.
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("cargo test && cargo build | head\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo"));
}

#[test]
fn test_rewrite_compound_bail_on_variable_expansion() {
    // ${ triggers bail — exit 1
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("${CARGO:-cargo} test && echo done\n")
        .assert()
        .failure();
}

// ============================================================================
// Shell redirects (GRANITE #530)
// ============================================================================

#[test]
fn test_rewrite_redirect_stderr_to_stdout() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("cargo test 2>&1\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo 2>&1"));
}

#[test]
fn test_rewrite_redirect_stderr_to_stdout_pipe() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("cargo test 2>&1 | head\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo 2>&1"))
        .stdout(predicate::str::contains("|"))
        .stdout(predicate::str::contains("head"));
}

#[test]
fn test_rewrite_redirect_stderr_to_stdout_compound() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("cargo test 2>&1 && cargo build\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo 2>&1"))
        .stdout(predicate::str::contains("&&"))
        .stdout(predicate::str::contains("skim build cargo"));
}

#[test]
fn test_rewrite_redirect_stderr_to_devnull() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("cargo test 2>/dev/null\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo 2>/dev/null"));
}

#[test]
fn test_rewrite_redirect_stdout_to_file() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("cargo test > output.txt\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo > output.txt"));
}

#[test]
fn test_rewrite_redirect_both_to_file() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("cargo test &> output.txt\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo &> output.txt"));
}

#[test]
fn test_rewrite_redirect_git_with_skip_flags() {
    // Redirect must not trigger skip_if_flag_prefix (--porcelain, --stat, etc.)
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("git status 2>&1\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git status 2>&1"));
}

// ============================================================================
// Git with skip flags (AD-4: --stat and --format/--pretty removed from skip list)
// ============================================================================

/// `git log --format=...` now rewrites (AD-4: --format removed from skip list).
/// The log handler detects --format via user_has_flag and passthroughs to git.
#[test]
fn test_rewrite_git_log_format_rewrites() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "git", "log", "--format=%H"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git log --format=%H"));
}

#[test]
fn test_rewrite_git_status_success() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "git", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git status"));
}

/// `git diff --stat` now rewrites (AD-4: --stat removed from skip list).
/// The diff handler detects --stat via user_has_flag and passthroughs to git.
#[test]
fn test_rewrite_git_diff_stat_rewrites() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "git", "diff", "--stat"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git diff --stat"));
}

/// `git diff --staged` rewrites after engine strict-match fix (AD-1).
#[test]
fn test_rewrite_git_diff_staged_rewrites() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "git", "diff", "--staged"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git diff --staged"));
}

/// `git diff --name-only` rewrites (AD-4: --name-only removed from skip list).
#[test]
fn test_rewrite_git_diff_name_only_rewrites() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "git", "diff", "--name-only"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git diff --name-only"));
}

/// `git show HEAD` rewrites (new rule, AD-5).
#[test]
fn test_rewrite_git_show_rewrites() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "git", "show", "HEAD"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git show HEAD"));
}

/// `git show HEAD:src/main.rs` rewrites (new rule, AD-5).
#[test]
fn test_rewrite_git_show_file_content_rewrites() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "git", "show", "HEAD:src/main.rs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git show HEAD:src/main.rs"));
}

/// `git worktree list` is AlreadyCompact (AD-2/AD-3): exits 0 and prints original.
#[test]
fn test_rewrite_git_worktree_list_already_compact() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "git", "worktree", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("git worktree list"));
}

/// `git worktree list --porcelain` is also AlreadyCompact (prefix match).
#[test]
fn test_rewrite_git_worktree_list_porcelain_already_compact() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "git", "worktree", "list", "--porcelain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("git worktree list --porcelain"));
}

/// Compound: `git worktree list && git show HEAD` → ack segment passes through,
/// show segment is rewritten (AD-2 compound behavior uses original try_rewrite_compound).
#[test]
fn test_rewrite_compound_worktree_list_and_git_show() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("git worktree list && git show HEAD\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git show HEAD"));
}

// ============================================================================
// Suggest mode
// ============================================================================

#[test]
fn test_suggest_mode_match() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "--suggest", "cargo", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"))
        .stdout(predicate::str::contains("\"category\":\"test\""));
}

#[test]
fn test_suggest_mode_no_match() {
    // bare `ls` has no rewrite rule — confirm suggest emits match:false
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "--suggest", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

// ============================================================================
// Stdin mode
// ============================================================================

#[test]
fn test_rewrite_stdin_cargo_test() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("rewrite")
        .write_stdin("cargo test\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo"));
}

// ============================================================================
// cat / head / tail
// ============================================================================

#[test]
fn test_rewrite_cat_code_file() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "cat", "src/main.rs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim src/main.rs --mode=pseudo"));
}

#[test]
fn test_rewrite_cat_squeeze_blanks() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "cat", "-s", "file.ts"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--mode=pseudo"));
}

#[test]
fn test_rewrite_cat_line_numbers_rejected() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "cat", "-n", "file.ts"])
        .assert()
        .failure();
}

#[test]
fn test_rewrite_head_with_count() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "head", "-20", "file.ts"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--max-lines"))
        .stdout(predicate::str::contains("20"));
}

#[test]
fn test_rewrite_head_n_space() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "head", "-n", "50", "file.py"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--max-lines"))
        .stdout(predicate::str::contains("50"));
}

#[test]
fn test_rewrite_tail_with_count() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "tail", "-20", "file.rs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--last-lines"))
        .stdout(predicate::str::contains("20"));
}

#[test]
fn test_rewrite_tail_non_code_rejected() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "tail", "-20", "data.csv"])
        .assert()
        .failure();
}

#[test]
fn test_rewrite_cat_non_code_rejected() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "cat", "data.csv"])
        .assert()
        .failure();
}

// ============================================================================
// Nextest
// ============================================================================

#[test]
fn test_rewrite_nextest() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "cargo", "nextest", "run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo"));
}

// ============================================================================
// Suggest mode + stdin
// ============================================================================

#[test]
fn test_suggest_mode_stdin_match() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "--suggest"])
        .write_stdin("cargo test\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"));
}

#[test]
fn test_suggest_mode_stdin_no_match() {
    // bare `ls` has no rewrite rule — confirm suggest emits match:false via stdin
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "--suggest"])
        .write_stdin("ls\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

// ============================================================================
// Help
// ============================================================================

#[test]
fn test_rewrite_help() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim rewrite"))
        .stdout(predicate::str::contains("--suggest"));
}

#[test]
fn test_rewrite_short_help() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim rewrite"));
}
