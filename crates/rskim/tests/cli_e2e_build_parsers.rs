//! E2E tests for build parsers (flat dispatch).
//!
//! v2.8.0: `skim build cargo` → `skim cargo build`
//!
//! Tests the cargo/clippy/make dispatch CLI behavior.
//!
//! NOTE: Build parsers do NOT support stdin piping — they always execute the
//! real build command. These tests verify real build execution behavior and
//! exit code semantics. TSC tests are skipped because `tsc` may not be
//! installed in the test environment.

use assert_cmd::Command;
use predicates::prelude::*;
use std::process::Command as StdCommand;
use tempfile::TempDir;

fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

// ============================================================================
// Cargo build: real execution
// ============================================================================

#[test]
fn test_build_cargo_success_exit_code() {
    // Running `skim cargo build` on the skim repo itself should succeed
    // (already compiled artifacts are cached) and produce structured output.
    skim_cmd()
        .args(["cargo", "build"])
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success()
        .stdout(predicate::str::contains("OK warnings:"));
}

// ============================================================================
// Clippy: real execution
// ============================================================================

#[test]
fn test_build_clippy_success_exit_code() {
    // Running `skim cargo clippy` on the skim repo should succeed
    // (clean code, no warnings that trigger failure).
    skim_cmd()
        .args(["cargo", "clippy"])
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
}

// ============================================================================
// Build error handling
// ============================================================================

#[test]
fn test_cargo_unknown_subcmd_exit_code() {
    skim_cmd()
        .args(["cargo", "webpack"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown subcommand"));
}

#[test]
fn test_cargo_no_subcmd_shows_help() {
    skim_cmd()
        .arg("cargo")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo"));
}

// ============================================================================
// Make: dispatch + help
// ============================================================================

#[test]
fn test_build_make_dispatches_through_build_module() {
    // `skim make --help` is intercepted before spawning the real `make` binary,
    // so this test is portable even on systems without `make` installed.
    // The guard below documents that intent and protects against future changes
    // that might remove the --help short-circuit.
    if StdCommand::new("make").arg("--version").output().is_err() {
        eprintln!("skipping: make not installed");
        return;
    }
    skim_cmd().args(["make", "--help"]).assert().success();
}

#[test]
fn test_build_make_real_execution_success() {
    // Verify that `skim make <target>` actually invokes the make parser — not
    // just the --help short-circuit in build::run. The cargo equivalent
    // (test_build_cargo_success_exit_code) executes a real build; this test
    // mirrors that pattern for make.
    //
    // A trivial Makefile with a silent no-op recipe produces empty
    // stdout+stderr and exits 0. The make parser's empty-output early return
    // fires, yielding ParseResult::Full(success=true), which renders as
    // "OK warnings: 0 errors: 0" on stdout.
    if StdCommand::new("make").arg("--version").output().is_err() {
        eprintln!("skipping: make not installed");
        return;
    }

    let dir = TempDir::new().expect("failed to create temp dir");
    std::fs::write(dir.path().join("Makefile"), "all:\n\t@:\n").expect("failed to write Makefile");

    skim_cmd()
        .args(["make", "all"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("OK warnings:"));
}
