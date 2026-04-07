//! E2E exit code verification for all parsers (#54).
//!
//! Systematic per-parser exit code tests via stdin piping.
//! Validates that each parser produces the correct exit code when
//! processing fixture data.
//!
//! ## Exit code semantics by parser
//!
//! - **cargo test**: Uses `run_parsed_command_with_mode()` which maps exit code
//!   from `output.exit_code` (not from parsed results). When stdin is piped,
//!   `exit_code` is always `Some(0)`, so cargo test via stdin always exits 0
//!   regardless of test results. This is the designed behavior — the exit code
//!   reflects the transport, not the content.
//!
//! - **pytest/vitest**: Have their own `run()` implementations that infer exit
//!   code from parsed results when `exit_code` is `None`. Failures in parsed
//!   content produce non-zero exit codes.
//!
//! - **go test**: Does NOT support stdin (always runs `go test`).
//! - **build parsers**: Do NOT support stdin (always run the real command).

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

// ============================================================================
// Cargo test exit codes
// ============================================================================

#[test]
fn test_exit_code_cargo_pass_json() {
    let fixture = include_str!("fixtures/cmd/test/cargo_pass.json");
    skim_cmd()
        .args(["test", "cargo"])
        .write_stdin(fixture)
        .assert()
        .code(0);
}

#[test]
fn test_exit_code_cargo_fail_json() {
    // Cargo test via stdin always exits 0 because run_parsed_command_with_mode
    // maps exit code from the synthetic CommandOutput (exit_code: Some(0)),
    // not from the parsed test results.
    let fixture = include_str!("fixtures/cmd/test/cargo_fail.json");
    skim_cmd()
        .args(["test", "cargo"])
        .write_stdin(fixture)
        .assert()
        .code(0)
        // Verify the output correctly shows failures even though exit code is 0
        .stdout(predicate::str::contains("FAIL: 1"));
}

#[test]
fn test_exit_code_cargo_nextest_pass() {
    let fixture = include_str!("fixtures/cmd/test/cargo_nextest_pass.txt");
    skim_cmd()
        .args(["test", "cargo"])
        .write_stdin(fixture)
        .assert()
        .code(0);
}

#[test]
fn test_exit_code_cargo_nextest_fail_via_stdin() {
    // When piped via stdin (no args), `is_nextest` is false because the cargo
    // parser checks args for "nextest". Without the nextest flag, the text
    // falls through to passthrough (no JSON suite events, no `test result:`
    // regex match). Exit code is 0 from synthetic stdin exit code.
    let fixture = include_str!("fixtures/cmd/test/cargo_nextest_fail.txt");
    skim_cmd()
        .args(["--debug", "test", "cargo"])
        .write_stdin(fixture)
        .assert()
        .code(0)
        .stderr(predicate::str::contains("[skim:notice]"));
}

#[test]
fn test_exit_code_cargo_passthrough_garbage() {
    // Passthrough with stdin: exit_code is Some(0) from synthetic CommandOutput,
    // so the process exits 0.
    let fixture = include_str!("fixtures/cmd/test/cargo_passthrough.txt");
    skim_cmd()
        .args(["--debug", "test", "cargo"])
        .write_stdin(fixture)
        .assert()
        .code(0)
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// Vitest exit codes
// ============================================================================

#[test]
fn test_exit_code_vitest_pass_json() {
    let fixture = include_str!("fixtures/vitest/vitest_pass.json");
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin(fixture)
        .assert()
        .code(0);
}

#[test]
fn test_exit_code_vitest_fail_json() {
    // Vitest infers exit code from parsed results (fail > 0 => FAILURE)
    let fixture = include_str!("fixtures/vitest/vitest_fail.json");
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin(fixture)
        .assert()
        .code(predicate::ne(0));
}

#[test]
fn test_exit_code_vitest_passthrough_garbage() {
    // Vitest passthrough always returns ExitCode::FAILURE
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin("completely unparseable garbage text\n")
        .assert()
        .code(predicate::ne(0));
}

// ============================================================================
// Pytest exit codes
// ============================================================================

#[test]
fn test_exit_code_pytest_pass() {
    let fixture = include_str!("fixtures/cmd/test/pytest_pass.txt");
    skim_cmd()
        .args(["test", "pytest"])
        .write_stdin(fixture)
        .assert()
        .code(0);
}

#[test]
fn test_exit_code_pytest_fail() {
    // Pytest infers exit code from parsed results (fail > 0 => FAILURE)
    let fixture = include_str!("fixtures/cmd/test/pytest_fail.txt");
    skim_cmd()
        .args(["test", "pytest"])
        .write_stdin(fixture)
        .assert()
        .code(predicate::ne(0));
}

#[test]
fn test_exit_code_pytest_all_fail() {
    let fixture = include_str!("fixtures/cmd/test/pytest_all_fail.txt");
    skim_cmd()
        .args(["test", "pytest"])
        .write_stdin(fixture)
        .assert()
        .code(predicate::ne(0));
}

#[test]
fn test_exit_code_pytest_passthrough_garbage() {
    // Pytest passthrough: exit_code is None (stdin) so it infers FAILURE
    skim_cmd()
        .args(["test", "pytest"])
        .write_stdin("random garbage not pytest output\n")
        .assert()
        .code(predicate::ne(0));
}
