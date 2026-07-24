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

/// `skim grep -h pattern <file>` — `-h` means no-filename, not help.
/// The dispatcher must NOT print help; it must pass through to grep.
///
/// This exercises the HANDLER surface (direct `skim grep` dispatch, per PF-004),
/// not the rewrite engine.  The two surfaces share per-tool handlers but have
/// different dispatch front-ends; `-h` flag preservation is tested here on the
/// handler surface only.
///
/// The original test used `/dev/null` (no matches → empty stdout), which cannot
/// distinguish "reached grep" from "errored before grep."  This version uses a
/// real temp file with known content so we can make a positive assertion that
/// grep actually ran and returned a match.
#[test]
fn test_grep_h_flag_not_intercepted_as_help() {
    let dir = TempDir::new().unwrap();
    let f = dir.path().join("needle.txt");
    std::fs::write(&f, "findme line\n").unwrap();

    let output = skim_cmd()
        .args(["grep", "-h", "findme", f.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Positive: exit 0 confirms skim reached the grep handler and grep matched.
    assert!(
        output.status.success(),
        "skim grep -h must exit 0 (grep ran and found a match); stderr: {stderr}"
    );

    // Positive: matched content in stdout proves grep actually ran against the file
    // and `-h` was passed through unchanged (not swallowed as help).
    assert!(
        stdout.contains("findme"),
        "grep -h output must include the matched line, proving grep ran; stdout: {stdout}"
    );

    // Negative: must not show skim's fileops help text.
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
    let output = skim_cmd().args(["ls", "-lh", "/tmp"]).output().unwrap();
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
/// line numbers.  This test exercises the HANDLER surface (direct `skim grep -hn`
/// dispatch), not the rewrite engine (PF-004 two-surfaces distinction).  The
/// fileops dispatcher must NOT intercept `-h` as help; `-h` must reach the grep
/// handler unchanged.
///
/// Fix 3: grep now passes through native output byte-faithfully.  With `-h`, grep
/// emits `lineno:content` (no file prefix), so each match appears on its own line
/// with only the line number — no `(no filename)` label from the old grouped renderer.
#[test]
fn test_grep_h_multifile_produces_no_filename_label() {
    let dir = TempDir::new().unwrap();
    let f1 = dir.path().join("a.txt");
    let f2 = dir.path().join("b.txt");
    std::fs::write(&f1, "hello world\n").unwrap();
    std::fs::write(&f2, "hello again\n").unwrap();

    let output = skim_cmd()
        .args([
            "grep",
            "-hn",
            "hello",
            f1.to_str().unwrap(),
            f2.to_str().unwrap(),
        ])
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
    // Fix 3: native passthrough — no (no filename) label from old grouped renderer.
    // With -h, grep omits file prefixes; native output is `lineno:content`.
    assert!(
        !stdout.contains("(no filename)"),
        "Fix 3: native passthrough must not inject (no filename) label; got: {stdout}"
    );
    // Both matches must appear (line count == match count — no header/footer).
    assert!(
        stdout.contains("hello world"),
        "first match must appear: {stdout}"
    );
    assert!(
        stdout.contains("hello again"),
        "second match must appear: {stdout}"
    );
}
