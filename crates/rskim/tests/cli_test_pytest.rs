//! Integration tests for `skim test pytest` subcommand (#47).
//!
//! Tests end-to-end CLI behavior: help output, piped fixture parsing,
//! and (optionally) real pytest execution when available.

use assert_cmd::Command;
use predicates::prelude::*;
use std::process;

// ============================================================================
// Help and subcommand routing
// ============================================================================

#[test]
fn test_skim_test_help_mentions_pytest() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["test", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pytest"));
}

#[test]
fn test_skim_test_pytest_help() {
    // `skim test pytest` with no stdin and no args should attempt to run pytest.
    // Since we can't guarantee pytest is installed, we just check that
    // the subcommand routing works (doesn't say "not yet implemented").
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["test", "pytest", "--help"])
        .output()
        .unwrap();

    // If pytest is installed, it shows pytest help.
    // If not, we get an error about pytest not being found.
    // Either way, we should NOT see the "not yet implemented" message.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("not yet implemented"),
        "skim test pytest should be implemented, got: {combined}"
    );
}

// ============================================================================
// Piped fixture tests
// ============================================================================

#[test]
fn test_piped_all_pass() {
    let fixture = include_str!("fixtures/cmd/test/pytest_pass.txt");
    Command::cargo_bin("skim")
        .unwrap()
        .args(["test", "pytest"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS: 5"))
        .stdout(predicate::str::contains("FAIL: 0"));
}

#[test]
fn test_piped_with_failures() {
    let fixture = include_str!("fixtures/cmd/test/pytest_fail.txt");
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["test", "pytest"])
        .write_stdin(fixture)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should show 2 passed, 1 failed
    assert!(
        stdout.contains("PASS: 2"),
        "expected PASS: 2 in output, got: {stdout}"
    );
    assert!(
        stdout.contains("FAIL: 1"),
        "expected FAIL: 1 in output, got: {stdout}"
    );

    // Should include the failure detail
    assert!(
        stdout.contains("test_divide") || stdout.contains("test_math"),
        "expected failure test name in output, got: {stdout}"
    );
}

#[test]
fn test_piped_mixed() {
    let fixture = include_str!("fixtures/cmd/test/pytest_mixed.txt");
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["test", "pytest"])
        .write_stdin(fixture)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("PASS: 4"),
        "expected PASS: 4, got: {stdout}"
    );
    assert!(
        stdout.contains("FAIL: 1"),
        "expected FAIL: 1, got: {stdout}"
    );
    assert!(
        stdout.contains("SKIP: 1"),
        "expected SKIP: 1, got: {stdout}"
    );
}

#[test]
fn test_piped_all_failures() {
    let fixture = include_str!("fixtures/cmd/test/pytest_all_fail.txt");
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["test", "pytest"])
        .write_stdin(fixture)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("PASS: 0"),
        "expected PASS: 0 in output, got: {stdout}"
    );
    assert!(
        stdout.contains("FAIL: 3"),
        "expected FAIL: 3 in output, got: {stdout}"
    );
}

#[test]
fn test_piped_passthrough_for_garbage() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["test", "pytest"])
        .write_stdin("this is not pytest output\n")
        .assert()
        // Passthrough: should still output something (the raw input)
        .stdout(predicate::str::contains("this is not pytest output"));
}

// ============================================================================
// Passthrough mode (SKIM_PASSTHROUGH=1)
// ============================================================================

/// When SKIM_PASSTHROUGH=1 and input is piped, raw output is forwarded unchanged
/// (no compression header, no PASS:/FAIL: counts).
#[test]
fn test_piped_passthrough_mode_skips_compression() {
    let fixture = include_str!("fixtures/cmd/test/pytest_fail.txt");
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["test", "pytest"])
        .env("SKIM_PASSTHROUGH", "1")
        .write_stdin(fixture)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Must forward the raw pytest output (contains the original summary line)
    assert!(
        stdout.contains("passed") || stdout.contains("failed"),
        "passthrough should forward raw pytest output, got: {stdout}"
    );

    // Must NOT contain the compressed format headers
    assert!(
        !stdout.contains("PASS:") && !stdout.contains("FAIL:"),
        "passthrough must not emit compressed PASS:/FAIL: counts, got: {stdout}"
    );
}

// ============================================================================
// Failure context
// ============================================================================

/// When pytest reports failures, the compressed output must include a raw
/// failure context tail so the agent can diagnose failures without re-running.
#[test]
fn test_failure_context_appended_on_failures() {
    let fixture = include_str!("fixtures/cmd/test/pytest_fail.txt");
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["test", "pytest"])
        .write_stdin(fixture)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Compressed header must be present
    assert!(
        stdout.contains("FAIL: 1"),
        "expected FAIL: 1 in compressed output, got: {stdout}"
    );

    // Failure context separator must appear
    assert!(
        stdout.contains("--- failure context"),
        "expected failure context block in output, got: {stdout}"
    );
}

/// When all tests pass there must be no failure context tail.
#[test]
fn test_failure_context_absent_on_all_pass() {
    let fixture = include_str!("fixtures/cmd/test/pytest_pass.txt");
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["test", "pytest"])
        .write_stdin(fixture)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !stdout.contains("--- failure context"),
        "failure context must not appear when all tests pass, got: {stdout}"
    );
}

// ============================================================================
// Unknown runner
// ============================================================================

#[test]
fn test_unknown_runner() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["test", "unknown_runner_xyz"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown runner"));
}

// ============================================================================
// Real pytest execution (gated on pytest availability)
// ============================================================================

#[test]
fn test_real_pytest_if_available() {
    // Skip if pytest is not installed
    if process::Command::new("pytest")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }

    // Run a trivial passing test
    let dir = tempfile::TempDir::new().unwrap();
    let test_file = dir.path().join("test_trivial.py");
    std::fs::write(
        &test_file,
        "def test_one():\n    assert 1 + 1 == 2\n\ndef test_two():\n    assert True\n",
    )
    .unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["test", "pytest", test_file.to_str().unwrap()])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("PASS: 2"),
        "expected PASS: 2 from real pytest, got: {stdout}"
    );
    assert!(
        stdout.contains("FAIL: 0"),
        "expected FAIL: 0 from real pytest, got: {stdout}"
    );
}
