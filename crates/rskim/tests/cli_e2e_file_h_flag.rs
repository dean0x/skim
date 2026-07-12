//! E2E tests verifying that `-h` is NOT intercepted as `--help` in the fileops
//! dispatcher, mirroring the `db/mod.rs` precedent where `-h` is a legitimate
//! tool-level flag (grep -h = no-filename; ls/du/df/tree -h = human-readable).
//!
//! `skim <tool> --help` continues to show skim's fileops help (keep-green contract).

use predicates::prelude::*;
use tempfile::TempDir;
mod common;

fn skim_cmd() -> assert_cmd::Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd
}

// ============================================================================
// grep -h must NOT trigger help text
// ============================================================================

/// `skim grep -h pattern /dev/null` — `-h` means no-filename, not help.
/// The dispatcher must NOT print help; it must pass through to grep.
#[test]
fn test_grep_h_flag_not_intercepted_as_help() {
    let output = skim_cmd()
        .args(["grep", "-h", "pattern", "/dev/null"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains("Available tools:"),
        "grep -h must not trigger fileops help text; stdout: {stdout}, stderr: {stderr}"
    );
    assert!(
        !stdout.contains("Run file operation tools"),
        "grep -h must not trigger fileops help text; stdout: {stdout}"
    );
}

// ============================================================================
// ls -lh must NOT trigger help text
// ============================================================================

/// `skim ls -lh /tmp` — `-h` is the human-readable flag, not help.
#[test]
fn test_ls_lh_not_intercepted_as_help() {
    let output = skim_cmd()
        .args(["ls", "-lh", "/tmp"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Available tools:"),
        "ls -lh must not trigger fileops help text; got stdout: {stdout}"
    );
    assert!(
        !stdout.contains("Run file operation tools"),
        "ls -lh must not trigger fileops help text; got stdout: {stdout}"
    );
}

// ============================================================================
// --help still triggers help (keep-green contract)
// ============================================================================

/// `skim grep --help` must still show the fileops help text.
#[test]
fn test_file_long_help_still_triggers_help() {
    skim_cmd()
        .args(["grep", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available tools:"));
}

// ============================================================================
// grep -h multifile — structured output, no help dump
// ============================================================================

/// `skim grep -hn hello <f1> <f2>` — `-h` suppresses filename prefix, `-n` adds
/// line numbers.  The rewrite rule rewrites this to `skim grep -hn …` and the
/// dispatcher must NOT intercept `-h` as help.  The output should contain the
/// matched content and the `(no filename)` fallback label.
#[test]
fn test_grep_h_multifile_produces_no_filename_label() {
    let dir = TempDir::new().unwrap();
    let f1 = dir.path().join("a.txt");
    let f2 = dir.path().join("b.txt");
    std::fs::write(&f1, "hello world\n").unwrap();
    std::fs::write(&f2, "hello again\n").unwrap();

    let output = skim_cmd()
        .args(["grep", "-hn", "hello", f1.to_str().unwrap(), f2.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Must NOT be help text
    assert!(
        !stdout.contains("Available tools:"),
        "grep -hn must not trigger help text; got: {stdout}"
    );
    // Should contain the matched content
    assert!(
        stdout.contains("hello"),
        "grep -hn output must contain matched content; got: {stdout}"
    );
    // When grep -h suppresses filenames, lines without the `file:lineno:` prefix
    // fall into the `(no filename)` bucket in the structured parser.
    assert!(
        stdout.contains("(no filename)"),
        "grep -hn output should contain '(no filename)' label; got: {stdout}"
    );
}
