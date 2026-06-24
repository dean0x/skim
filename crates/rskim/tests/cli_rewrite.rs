//! Integration tests for `skim rewrite` subcommand (#43).
//!
//! Tests the end-to-end CLI behavior of the rewrite engine, covering
//! standard prefix rewrites, env vars, cargo toolchain, compound commands,
//! git skip-flags, suggest mode, stdin mode, and cat/head/tail handlers.

use assert_cmd::Command;
use predicates::prelude::*;
mod common;

// ============================================================================
// Standard rewrites
// ============================================================================

#[test]
fn test_rewrite_cargo_test_with_separator() {
    common::skim()
        .args(["rewrite", "cargo", "test", "--", "--nocapture"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test -- --nocapture"));
}

#[test]
fn test_rewrite_ls_no_match() {
    // NOTE: bare `ls` now matches the catch-all rule (B.1) added in v2.5.1 and
    // IS rewritten to `skim ls` (v2.8.0 flat dispatch: was `skim file ls`).  Updated from the original no-match expectation.
    common::skim()
        .args(["rewrite", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim"));
}

#[test]
fn test_rewrite_cargo_build() {
    common::skim()
        .args(["rewrite", "cargo", "build"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo build"));
}

#[test]
fn test_rewrite_go_test_with_path() {
    common::skim()
        .args(["rewrite", "go", "test", "./..."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim go test ./..."));
}

#[test]
fn test_rewrite_pytest_with_flag() {
    common::skim()
        .args(["rewrite", "pytest", "-v"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pytest -v"));
}

// ============================================================================
// Env vars
// ============================================================================

#[test]
fn test_rewrite_with_env_var() {
    common::skim()
        .args(["rewrite", "RUST_LOG=debug", "cargo", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("RUST_LOG=debug skim cargo test"));
}

// ============================================================================
// Cargo toolchain
// ============================================================================

#[test]
fn test_rewrite_cargo_toolchain_nightly() {
    common::skim()
        .args(["rewrite", "cargo", "+nightly", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test +nightly"));
}

#[test]
fn test_rewrite_env_var_with_toolchain() {
    common::skim()
        .args(["rewrite", "RUST_LOG=debug", "cargo", "+nightly", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "RUST_LOG=debug skim cargo test +nightly",
        ));
}

// ============================================================================
// Compound commands (#45)
// ============================================================================

#[test]
fn test_rewrite_compound_and_and() {
    // Both segments should be rewritten
    common::skim()
        .args(["rewrite", "cargo", "test", "&&", "cargo", "build"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test"))
        .stdout(predicate::str::contains("&&"))
        .stdout(predicate::str::contains("skim cargo build"));
}

#[test]
fn test_rewrite_compound_pipe_never_rewritten() {
    // #317 (user-approved): pipe expressions are never rewritten — exit 1.
    common::skim()
        .arg("rewrite")
        .write_stdin("cargo test | head\n")
        .assert()
        .failure()
        .stdout(predicate::str::is_empty());
}

#[test]
fn test_rewrite_compound_semicolon() {
    common::skim()
        .args(["rewrite", "cargo", "test", ";", "echo", "done"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test"))
        .stdout(predicate::str::contains(";"))
        .stdout(predicate::str::contains("echo done"));
}

#[test]
fn test_rewrite_compound_bail_on_subshell() {
    // $( triggers bail — exit 1
    common::skim()
        .arg("rewrite")
        .write_stdin("$(command) && cargo test\n")
        .assert()
        .failure();
}

#[test]
fn test_rewrite_compound_suggest_mode() {
    // Suggest mode should include compound: true for compound commands
    common::skim()
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
    common::skim()
        .arg("rewrite")
        .write_stdin("cargo test || echo fail\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test"))
        .stdout(predicate::str::contains("||"))
        .stdout(predicate::str::contains("echo fail"));
}

#[test]
fn test_rewrite_compound_no_spaces_around_operator() {
    // Operators without surrounding spaces (e.g., cargo test&&cargo build)
    common::skim()
        .arg("rewrite")
        .write_stdin("cargo test&&cargo build\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test"))
        .stdout(predicate::str::contains("&&"))
        .stdout(predicate::str::contains("skim cargo build"));
}

#[test]
fn test_rewrite_compound_escaped_quotes() {
    // Escaped double quotes inside a quoted string should not break splitting
    common::skim()
        .arg("rewrite")
        .write_stdin("echo \"say \\\"hello\\\"\" && cargo test\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test"));
}

#[test]
fn test_rewrite_compound_mixed_pipe_and_sequential() {
    // Mixed pipe + sequential: ANY top-level pipe makes the whole expression
    // pass through untouched (#317) — exit 1.
    common::skim()
        .arg("rewrite")
        .write_stdin("cargo test && cargo build | head\n")
        .assert()
        .failure()
        .stdout(predicate::str::is_empty());
}

#[test]
fn test_rewrite_compound_bail_on_variable_expansion() {
    // ${ triggers bail — exit 1
    common::skim()
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
    common::skim()
        .arg("rewrite")
        .write_stdin("cargo test 2>&1\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test 2>&1"));
}

#[test]
fn test_rewrite_redirect_stderr_to_stdout_pipe() {
    // #317: pipes never rewrite — redirects in the producer are preserved
    // implicitly because the ORIGINAL command runs unchanged.
    common::skim()
        .arg("rewrite")
        .write_stdin("cargo test 2>&1 | head\n")
        .assert()
        .failure()
        .stdout(predicate::str::is_empty());
}

#[test]
fn test_rewrite_redirect_stderr_to_stdout_compound() {
    common::skim()
        .arg("rewrite")
        .write_stdin("cargo test 2>&1 && cargo build\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test 2>&1"))
        .stdout(predicate::str::contains("&&"))
        .stdout(predicate::str::contains("skim cargo build"));
}

#[test]
fn test_rewrite_redirect_stderr_to_devnull() {
    common::skim()
        .arg("rewrite")
        .write_stdin("cargo test 2>/dev/null\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test 2>/dev/null"));
}

#[test]
fn test_rewrite_redirect_stdout_to_file() {
    common::skim()
        .arg("rewrite")
        .write_stdin("cargo test > output.txt\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test > output.txt"));
}

#[test]
fn test_rewrite_redirect_both_to_file() {
    common::skim()
        .arg("rewrite")
        .write_stdin("cargo test &> output.txt\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test &> output.txt"));
}

#[test]
fn test_rewrite_redirect_git_with_skip_flags() {
    // Redirect must not trigger skip_if_flag_prefix (--porcelain, --stat, etc.)
    common::skim()
        .arg("rewrite")
        .write_stdin("git status 2>&1\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git status 2>&1"));
}

// ============================================================================
// Git with skip flags (AD-RW-4: --stat and --format/--pretty removed from skip list)
// ============================================================================

/// `git log --format=...` now rewrites (AD-RW-4: --format removed from skip list).
/// The log handler detects --format via user_has_flag and passthroughs to git.
#[test]
fn test_rewrite_git_log_format_rewrites() {
    common::skim()
        .args(["rewrite", "git", "log", "--format=%H"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git log --format=%H"));
}

#[test]
fn test_rewrite_git_status_success() {
    common::skim()
        .args(["rewrite", "git", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git status"));
}

/// `git diff --stat` now rewrites (AD-RW-4: --stat removed from skip list).
/// The diff handler detects --stat via user_has_flag and passthroughs to git.
#[test]
fn test_rewrite_git_diff_stat_rewrites() {
    common::skim()
        .args(["rewrite", "git", "diff", "--stat"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git diff --stat"));
}

/// `git diff --staged` rewrites after engine strict-match fix (AD-RW-1).
#[test]
fn test_rewrite_git_diff_staged_rewrites() {
    common::skim()
        .args(["rewrite", "git", "diff", "--staged"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git diff --staged"));
}

/// `git diff --name-only` rewrites (AD-RW-4: --name-only removed from skip list).
#[test]
fn test_rewrite_git_diff_name_only_rewrites() {
    common::skim()
        .args(["rewrite", "git", "diff", "--name-only"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git diff --name-only"));
}

/// `git show HEAD` rewrites (new rule, AD-GIT-5).
#[test]
fn test_rewrite_git_show_rewrites() {
    common::skim()
        .args(["rewrite", "git", "show", "HEAD"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git show HEAD"));
}

/// `git show HEAD:src/main.rs` rewrites (new rule, AD-GIT-5).
#[test]
fn test_rewrite_git_show_file_content_rewrites() {
    common::skim()
        .args(["rewrite", "git", "show", "HEAD:src/main.rs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git show HEAD:src/main.rs"));
}

/// `git worktree list` is AlreadyCompact (AD-RW-2/AD-RW-3): exits 0 and prints original.
#[test]
fn test_rewrite_git_worktree_list_already_compact() {
    common::skim()
        .args(["rewrite", "git", "worktree", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("git worktree list"));
}

/// `git worktree list --porcelain` is also AlreadyCompact (prefix match).
#[test]
fn test_rewrite_git_worktree_list_porcelain_already_compact() {
    common::skim()
        .args(["rewrite", "git", "worktree", "list", "--porcelain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("git worktree list --porcelain"));
}

/// Compound: `git worktree list && git show HEAD` → ack segment passes through,
/// show segment is rewritten (AD-RW-2 compound behavior uses original try_rewrite_compound).
#[test]
fn test_rewrite_compound_worktree_list_and_git_show() {
    common::skim()
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
    common::skim()
        .args(["rewrite", "--suggest", "cargo", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"))
        .stdout(predicate::str::contains("\"category\":\"test\""));
}

#[test]
fn test_suggest_mode_no_match() {
    // NOTE: bare `ls` now matches the catch-all rule (B.1, v2.5.1) — use `echo`
    // as a stable non-rewritable example.
    common::skim()
        .args(["rewrite", "--suggest", "echo", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

// ============================================================================
// Stdin mode
// ============================================================================

#[test]
fn test_rewrite_stdin_cargo_test() {
    common::skim()
        .arg("rewrite")
        .write_stdin("cargo test\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test"));
}

// ============================================================================
// cat / head / tail
// ============================================================================

#[test]
fn test_rewrite_cat_code_file() {
    common::skim()
        .args(["rewrite", "cat", "src/main.rs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim src/main.rs --mode=pseudo"));
}

#[test]
fn test_rewrite_cat_squeeze_blanks() {
    common::skim()
        .args(["rewrite", "cat", "-s", "file.ts"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--mode=pseudo"));
}

#[test]
fn test_rewrite_cat_line_numbers_rejected() {
    common::skim()
        .args(["rewrite", "cat", "-n", "file.ts"])
        .assert()
        .failure();
}

#[test]
fn test_rewrite_head_with_count() {
    common::skim()
        .args(["rewrite", "head", "-20", "file.ts"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--max-lines"))
        .stdout(predicate::str::contains("20"));
}

#[test]
fn test_rewrite_head_n_space() {
    common::skim()
        .args(["rewrite", "head", "-n", "50", "file.py"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--max-lines"))
        .stdout(predicate::str::contains("50"));
}

#[test]
fn test_rewrite_tail_with_count() {
    common::skim()
        .args(["rewrite", "tail", "-20", "file.rs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--last-lines"))
        .stdout(predicate::str::contains("20"));
}

#[test]
fn test_rewrite_tail_non_code_rejected() {
    common::skim()
        .args(["rewrite", "tail", "-20", "data.csv"])
        .assert()
        .failure();
}

#[test]
fn test_rewrite_cat_non_code_rejected() {
    common::skim()
        .args(["rewrite", "cat", "data.csv"])
        .assert()
        .failure();
}

// ============================================================================
// Nextest
// ============================================================================

#[test]
fn test_rewrite_nextest() {
    common::skim()
        .args(["rewrite", "cargo", "nextest", "run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo nextest"));
}

// ============================================================================
// Suggest mode + stdin
// ============================================================================

#[test]
fn test_suggest_mode_stdin_match() {
    common::skim()
        .args(["rewrite", "--suggest"])
        .write_stdin("cargo test\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"));
}

#[test]
fn test_suggest_mode_stdin_no_match() {
    // NOTE: bare `ls` now matches the catch-all rule (B.1, v2.5.1) — use `echo`
    // as a stable non-rewritable example.
    common::skim()
        .args(["rewrite", "--suggest"])
        .write_stdin("echo hello\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

// ============================================================================
// Help
// ============================================================================

#[test]
fn test_rewrite_help() {
    common::skim()
        .args(["rewrite", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim rewrite"))
        .stdout(predicate::str::contains("--suggest"));
}

#[test]
fn test_rewrite_short_help() {
    common::skim()
        .args(["rewrite", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim rewrite"));
}

// ============================================================================
// Task 1 regression: find/rg pipe-source exclusion (AD-RW-2)
// ============================================================================

/// `find . | head` must NOT be rewritten on the pipe source — raw output is
/// consumed by `head` and rewriting would break the pipeline.  (AD-RW-2)
#[test]
fn test_find_pipe_not_rewritten() {
    common::skim()
        .arg("rewrite")
        .write_stdin("find . -name foo | head\n")
        .assert()
        .failure(); // exit 1 = Unhandled (pipe source excluded)
}

/// `rg pattern | head` must NOT be rewritten on the pipe source. (AD-RW-2)
#[test]
fn test_rg_pipe_not_rewritten() {
    common::skim()
        .arg("rewrite")
        .write_stdin("rg pattern | head\n")
        .assert()
        .failure(); // exit 1 = Unhandled (pipe source excluded)
}

/// Standalone `find . -name foo` (no pipe) SHOULD still be rewritten.
#[test]
fn test_find_standalone_rewritten() {
    common::skim()
        .arg("rewrite")
        .write_stdin("find . -name foo\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim find"));
}

/// Standalone `rg pattern` (no pipe) SHOULD still be rewritten.
#[test]
fn test_rg_standalone_rewritten() {
    common::skim()
        .arg("rewrite")
        .write_stdin("rg pattern\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim rg"));
}

/// `find . || echo fail` — `||` is NOT a pipe, so `find` IS rewritten.
/// Pipe-source exclusion only applies to `|`, not `||` or `&&`.
#[test]
fn test_find_or_chain_still_rewritten() {
    common::skim()
        .arg("rewrite")
        .write_stdin("find . || echo fail\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim find"));
}

// ============================================================================
// Task 6a: compress-or-skip negative tests (AD-RW-2)
// ============================================================================

/// `ls --help` must NOT be rewritten — informational invocations pass through.
#[test]
fn test_rewrite_ls_help_passthrough() {
    common::skim()
        .arg("rewrite")
        .write_stdin("ls --help\n")
        .assert()
        .failure(); // skip_if_flag_prefix fires
}

/// `grep --version` must NOT be rewritten.
#[test]
fn test_rewrite_grep_version_passthrough() {
    common::skim()
        .arg("rewrite")
        .write_stdin("grep --version\n")
        .assert()
        .failure(); // skip_if_flag_prefix fires
}

/// `ls | head` — catch-all ls rule is excluded on pipe source (AD-RW-2).
#[test]
fn test_rewrite_ls_pipe_excluded() {
    common::skim()
        .arg("rewrite")
        .write_stdin("ls | head\n")
        .assert()
        .failure(); // pipe-source excluded
}

/// Bare `ls` (renamed from test_rewrite_ls_no_match) — catch-all matches and rewrites.
#[test]
fn test_rewrite_ls_catch_all_matches() {
    // NOTE: bare `ls` matches the catch-all rule (B.1) added in v2.5.1 and
    // IS rewritten to `skim ls` when NOT on the source side of a pipe (v2.8.0 flat dispatch).
    common::skim()
        .args(["rewrite", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim ls"));
}
