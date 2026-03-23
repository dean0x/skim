//! E2E tests for build parsers (#54).
//!
//! Tests the build subcommand's CLI behavior for cargo build and clippy.
//!
//! NOTE: Build parsers do NOT support stdin piping — they always execute the
//! real build command. These tests verify real build execution behavior and
//! exit code semantics. TSC tests are skipped because `tsc` may not be
//! installed in the test environment.

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

// ============================================================================
// Cargo build: real execution
// ============================================================================

#[test]
fn test_build_cargo_success_exit_code() {
    // Running `skim build cargo` on the skim repo itself should succeed
    // (already compiled artifacts are cached).
    skim_cmd()
        .args(["build", "cargo"])
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success()
        .stdout(predicate::str::contains("BUILD OK"));
}

#[test]
fn test_build_cargo_structured_output() {
    // Verify the output includes build result markers
    skim_cmd()
        .args(["build", "cargo"])
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success()
        .stdout(predicate::str::contains("BUILD OK"));
}

// ============================================================================
// Clippy: real execution
// ============================================================================

#[test]
fn test_build_clippy_success_exit_code() {
    // Running `skim build clippy` on the skim repo should succeed
    // (clean code, no warnings that trigger failure).
    skim_cmd()
        .args(["build", "clippy"])
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
}

// ============================================================================
// Build error handling
// ============================================================================

#[test]
fn test_build_unknown_tool_exit_code() {
    skim_cmd()
        .args(["build", "webpack"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown build tool"));
}

#[test]
fn test_build_missing_tool_exit_code() {
    skim_cmd()
        .arg("build")
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing required argument"));
}
