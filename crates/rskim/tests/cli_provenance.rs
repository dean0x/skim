//! Integration tests for B4 (skim doctor) and B5 (provenance / debug startup) features.

use predicates::prelude::*;
use tempfile::TempDir;

mod common;

fn skim_cmd() -> assert_cmd::Command {
    common::skim()
}

// ============================================================================
// B4: skim doctor
// ============================================================================

#[test]
fn test_doctor_runs_without_crash() {
    // Smoke test: `skim doctor` must exit with code 0 or 1 (healthy or drift),
    // never with a Rust panic or unhandled error.
    let status = skim_cmd()
        .args(["doctor"])
        .assert()
        .stderr(predicate::str::is_empty()); // no error output on stderr

    // Accept exit code 0 (healthy) or 1 (drift) — both are valid outcomes.
    let output = status.get_output();
    assert!(
        output.status.code() == Some(0) || output.status.code() == Some(1),
        "skim doctor must exit with code 0 or 1, got: {:?}",
        output.status.code()
    );
}

#[test]
fn test_doctor_help() {
    skim_cmd()
        .args(["doctor", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim doctor"))
        .stdout(predicate::str::contains("Exit codes"));
}

#[test]
fn test_doctor_stdout_contains_sections() {
    // Doctor output must include all expected section headings.
    let assert = skim_cmd().args(["doctor"]).assert();

    let output = assert.get_output();
    let stdout = std::str::from_utf8(&output.stdout).unwrap_or("");

    assert!(
        stdout.contains("Running binary"),
        "missing 'Running binary' section"
    );
    assert!(stdout.contains("$PATH"), "missing '$PATH' section");
    assert!(stdout.contains("Hooks"), "missing 'Hooks' section");
    assert!(stdout.contains("Wrappers"), "missing 'Wrappers' section");
    assert!(
        stdout.contains("Cache & Analytics"),
        "missing 'Cache & Analytics' section"
    );
    assert!(stdout.contains("Staleness"), "missing 'Staleness' section");
    assert!(stdout.contains("Status:"), "missing 'Status:' summary line");
}

#[test]
fn test_doctor_commit_flag_prints_commit() {
    // B4: `--commit` is a hidden early-exit flag used by doctor to identify
    // binaries on $PATH. It must print the compiled SKIM_GIT_COMMIT and exit 0.
    skim_cmd()
        .arg("--commit")
        .assert()
        .success()
        // Output is either a short hex SHA or "unknown" (tarball build).
        // Either way it must be non-empty and on a single line.
        .stdout(predicate::str::is_match(r"^[0-9a-f]+|unknown\n?$").unwrap());
}

// ============================================================================
// B5a: SKIM_DEBUG in hook mode must not emit to stderr
// ============================================================================

/// B5a regression: when `SKIM_DEBUG=1` is set and the binary runs in hook
/// mode (`--hook` in argv), the structured startup line must go to hook.log,
/// NOT to stderr.
///
/// Claude Code treats any stderr output on exit 0 as an error (GRANITE #361
/// Bug 3). A globally-set `SKIM_DEBUG` would break every hook invocation
/// without this guard.
#[test]
fn test_b5a_debug_hook_mode_zero_stderr() {
    let tmp = TempDir::new().unwrap();

    // Minimal valid Claude Code PreToolUse JSON — triggers the rewrite path
    // and exits without error.
    let hook_input = r#"{"tool":"Bash","toolInput":{"command":"echo hello"}}"#;

    skim_cmd()
        .env("SKIM_DEBUG", "1")
        .env("SKIM_CACHE_DIR", tmp.path()) // isolate hook.log from real cache
        .args(["rewrite", "--hook", "--agent", "claude-code"])
        .write_stdin(hook_input)
        .assert()
        // Exit 0 expected (rewrite recognised and returned updatedInput).
        // What matters for GRANITE #361 is: zero stderr bytes.
        .stderr(predicate::str::is_empty());
}
