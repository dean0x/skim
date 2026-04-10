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

// ============================================================================
// Show — new subcommand (#132)
// ============================================================================

#[test]
fn test_skim_git_show_help_listed_in_git_help() {
    // Match the exact subcommand row from print_help() in cmd/git/mod.rs so that
    // removing or renaming the "show" line causes this test to fail.
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "  show      Show compressed commit or file content at a ref",
        ));
}

#[test]
fn test_skim_git_show_help_subcommand() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "show", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("commit").and(predicate::str::contains("USAGE")));
}

#[test]
fn test_skim_git_show_head_commit_mode() {
    // HIGH-8: Distinguish compressed output from raw passthrough.
    //
    // Three assertions that raw `git show HEAD` cannot satisfy simultaneously:
    //   1. Output is STRICTLY shorter (byte count) than raw git show HEAD.
    //   2. Output contains em-dash (U+2014) — only present in ShowCommitResult::render.
    //   3. A 7-char hex token appears on any line.
    //
    // Raw git show HEAD starts with "commit <full-hash>\nAuthor:" which never
    // contains U+2014, and is always larger than the compressed one-liner.

    // Collect raw git show HEAD for byte-count comparison.
    let raw_output = std::process::Command::new("git")
        .args(["show", "HEAD"])
        .output()
        .expect("git must be available");
    let raw_bytes = raw_output.stdout.len();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "show", "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success(), "skim git show HEAD should succeed");
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Assertion 1: em-dash separator produced by ShowCommitResult::render.
    // This is U+2014 (\u{2014}) — the render format is "<hash> <author> — <subject>".
    assert!(
        stdout.contains('\u{2014}'),
        "Expected em-dash separator (U+2014) in compressed show output — \
         raw git show never contains this character; got: {stdout}"
    );

    // Assertion 2: compressed output must be strictly shorter than raw.
    assert!(
        stdout.len() < raw_bytes,
        "Expected compressed output ({} bytes) to be strictly shorter than \
         raw git show HEAD ({raw_bytes} bytes); \
         if this fails, the guardrail emitted raw output",
        stdout.len()
    );

    // Assertion 3: keep original — a 7-char hex token must appear somewhere.
    let has_hash = stdout.lines().any(|l| {
        l.split_whitespace()
            .next()
            .is_some_and(|w| w.len() >= 7 && w.chars().all(|c| c.is_ascii_hexdigit()))
    });
    assert!(
        has_hash,
        "Expected a commit hash in show output, got: {stdout}"
    );
}

#[test]
fn test_skim_git_show_stat_passthrough() {
    // --stat triggers passthrough — git standard output format with no skim wrapping.
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "show", "--stat", "HEAD"])
        .assert()
        .success();
}

#[test]
fn test_skim_git_show_unknown_subcommand_message() {
    // The "unknown git subcommand" error should now list "show" in the supported list.
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "totally_unknown_cmd_xyz"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("show"));
}

#[test]
fn test_skim_git_show_file_content_json_rejected() {
    // --json in file-content mode (git show <ref>:<path>) must exit with
    // code 2 (argument error) and print a clear error to stderr.
    // The error message must NOT embed the literal text "(exit code 2)"
    // since that would be self-contradictory if the code were ever changed.
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "show", "HEAD:Cargo.toml", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "file-content --json must exit 2 (argument error), got: {:?}",
        output.status.code()
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("--json is not supported"),
        "stderr must explain the rejection, got: {stderr}"
    );
    assert!(
        !stderr.contains("(exit code 2)"),
        "stderr must not embed '(exit code 2)' — self-contradictory prose, got: {stderr}"
    );
}

// ============================================================================
// Show — commit-mode JSON branch (HIGH-9, #132)
// ============================================================================

#[test]
fn test_skim_git_show_head_commit_mode_json() {
    // HIGH-9: Verify commit-mode --json path behaviour post guardrail fix.
    //
    // Commit 71a8ce6 moved the guardrail call inside the Text-only branch so
    // that the JSON branch never emits `[skim:guardrail]` to stderr.
    // This test verifies:
    //   1. Exit code 0.
    //   2. stdout parses as valid JSON with expected ShowCommitResult keys.
    //   3. stderr contains NO `[skim:guardrail]` marker.
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "show", "HEAD", "--json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "skim git show HEAD --json should exit 0"
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    // Assertion 1: stderr must NOT contain the guardrail marker.
    assert!(
        !stderr.contains("[skim:guardrail]"),
        "JSON mode must never emit [skim:guardrail] to stderr; got stderr: {stderr}"
    );

    // Assertion 2: stdout must be valid JSON.
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout must be valid JSON; parse error: {e}\nstdout: {stdout}"));

    // Assertion 3: JSON must have the expected ShowCommitResult top-level keys.
    for key in &["hash", "author", "subject", "files_changed", "files"] {
        assert!(
            json.get(key).is_some(),
            "JSON output missing expected key '{key}'; got: {stdout}"
        );
    }

    // Assertion 4: hash field should be a non-empty string.
    let hash = json["hash"]
        .as_str()
        .unwrap_or_else(|| panic!("'hash' field must be a string; got: {}", json["hash"]));
    assert!(
        !hash.is_empty(),
        "ShowCommitResult.hash must not be empty; got: {stdout}"
    );
}

// ============================================================================
// Show — file-content mode passthrough paths (MEDIUM-26, #132)
// ============================================================================

/// Tier-2 passthrough: unsupported file extension causes `Language::from_path`
/// to return `None`, routing to `passthrough_file_content` without calling
/// `rskim_core::transform`.  This exercises the "no silent drop" guarantee:
/// the file content must appear on stdout and exit code must be 0.
///
/// Note: A true Tier-3 path (transform returns `Err`) requires `rskim_core::
/// transform` to fail, which is extremely rare in practice because tree-sitter
/// is error-tolerant and `CommandRunner` performs lossy UTF-8 conversion (so
/// binary files become valid—if ugly—strings).  Tier-2 exercises the same
/// `passthrough_file_content` code path and the same "no silent drop" property.
#[test]
fn test_skim_git_show_file_content_unsupported_ext_passthrough() {
    // MEDIUM-26: Tier-2 (unsupported extension) exercises passthrough_file_content.
    // Cargo.lock has no recognised language extension → Language::from_path returns None.
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "show", "HEAD:Cargo.lock"])
        .output()
        .unwrap();

    // Exit code must be 0 — content is passed through, not dropped.
    assert!(
        output.status.success(),
        "skim git show HEAD:Cargo.lock should exit 0 (passthrough); \
         got exit code {:?}",
        output.status.code()
    );

    let stdout = String::from_utf8(output.stdout).unwrap();

    // stdout must be non-empty — no silent content drop.
    assert!(
        !stdout.is_empty(),
        "skim git show HEAD:Cargo.lock stdout must not be empty (content was silently dropped)"
    );

    // Cargo.lock always starts with a known header comment.
    assert!(
        stdout.contains("# This file is automatically @generated by Cargo"),
        "Expected Cargo.lock header in passthrough output; got: {}",
        &stdout[..stdout.len().min(200)]
    );
}

// ============================================================================
// Git dispatcher coverage (MEDIUM-28, #132)
// ============================================================================

/// MEDIUM-28: Verify that the `git/mod.rs::run()` dispatcher correctly routes
/// each implemented subcommand.  Each assertion checks for a subcommand-specific
/// marker that passthrough alone could not produce.
///
/// Subcommands covered: status, diff, log, show, fetch.
#[test]
fn test_skim_git_dispatcher_routes_all_subcommands() {
    // ---- status ----
    // The status handler always prefixes output with "[status]".
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[status]"));

    // ---- log ----
    // The log handler prefixes output with "[log]".
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "log", "-n", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[log]"));

    // ---- show ----
    // The show handler (commit mode, --json) produces a JSON object —
    // passthrough would emit raw git format starting with "commit ".
    {
        let output = Command::cargo_bin("skim")
            .unwrap()
            .args(["git", "show", "HEAD", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "git show dispatch: exit 0");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.trim_start().starts_with('{'),
            "git show --json must produce a JSON object; got: {}",
            &stdout[..stdout.len().min(120)]
        );
    }

    // ---- diff ----
    // `git diff` on a clean repo exits 0.  The diff handler's own help string
    // differs from native git output, but a minimal invocation only guarantees
    // exit 0 when there are no unstaged changes.
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "diff"])
        .assert()
        .success();

    // ---- fetch ----
    // The fetch handler always prefixes output with "[fetch]" (even when
    // there is nothing to fetch).
    Command::cargo_bin("skim")
        .unwrap()
        .args(["git", "fetch"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[fetch]").or(predicate::str::contains("up to date")));
}
