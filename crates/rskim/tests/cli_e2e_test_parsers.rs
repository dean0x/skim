//! E2E tests for test parser degradation tiers (#54).
//!
//! Tests each parser at different degradation tiers via stdin piping,
//! verifying structured output markers and stderr diagnostics.
//!
//! Tier behavior reference (from emit_markers in output/mod.rs):
//! - Full: no stderr markers
//! - Degraded: "[warning] ..." on stderr
//! - Passthrough: "[notice] output passed through without parsing" on stderr

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

// ============================================================================
// Cargo: Tier 1 (JSON) — Full
// ============================================================================

#[test]
fn test_cargo_tier1_json_pass_structured_output() {
    let fixture = include_str!("fixtures/cmd/test/cargo_pass.json");
    skim_cmd()
        .args(["test", "cargo"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS:"))
        .stdout(predicate::str::contains("FAIL: 0"));
}

#[test]
fn test_cargo_tier1_json_fail_structured_output() {
    // Cargo test via stdin always exits 0 because run_parsed_command_with_mode
    // maps exit code from the synthetic CommandOutput, not from parsed results.
    let fixture = include_str!("fixtures/cmd/test/cargo_fail.json");
    skim_cmd()
        .args(["test", "cargo"])
        .write_stdin(fixture)
        .assert()
        .code(0)
        .stdout(predicate::str::contains("FAIL: 1"))
        .stdout(predicate::str::contains("PASS: 1"));
}

// ============================================================================
// Cargo: Tier 1 (nextest) via stdin
// ============================================================================
// NOTE: When piping nextest output via stdin (no args), `is_nextest` is false
// because the cargo parser checks args for "nextest". Without the nextest flag,
// the nextest text format falls through to passthrough (no JSON suite events,
// no `test result:` regex match). This is a known limitation of stdin-piped
// nextest output.

#[test]
fn test_cargo_nextest_pass_passthrough_via_stdin() {
    // Without "nextest" in args, nextest output hits passthrough tier
    let fixture = include_str!("fixtures/cmd/test/cargo_nextest_pass.txt");
    skim_cmd()
        .args(["test", "cargo"])
        .write_stdin(fixture)
        .assert()
        .success()
        // Content is passed through as-is
        .stdout(predicate::str::contains("PASS"))
        .stderr(predicate::str::contains("[notice]"));
}

#[test]
fn test_cargo_nextest_fail_passthrough_via_stdin() {
    // Without "nextest" in args, nextest output hits passthrough tier
    let fixture = include_str!("fixtures/cmd/test/cargo_nextest_fail.txt");
    skim_cmd()
        .args(["test", "cargo"])
        .write_stdin(fixture)
        .assert()
        // Exit code 0 from synthetic stdin exit code
        .code(0)
        .stdout(predicate::str::contains("FAIL"))
        .stderr(predicate::str::contains("[notice]"));
}

// ============================================================================
// Cargo: Tier 2 (regex) — Degraded
// ============================================================================

#[test]
fn test_cargo_tier2_regex_degraded() {
    // Plain text cargo test output triggers tier 2 regex parsing.
    // The run_parsed_command_with_mode sets exit_code to Some(0) for stdin,
    // so the process exits 0 when tests pass.
    let text_input = "test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured";
    skim_cmd()
        .args(["test", "cargo"])
        .write_stdin(text_input)
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS: 5"))
        .stderr(predicate::str::contains("[warning]"));
}

// ============================================================================
// Cargo: Tier 3 (passthrough) — Passthrough
// ============================================================================

#[test]
fn test_cargo_tier3_passthrough_garbage_input() {
    let fixture = include_str!("fixtures/cmd/test/cargo_passthrough.txt");
    skim_cmd()
        .args(["test", "cargo"])
        .write_stdin(fixture)
        .assert()
        // Passthrough preserves raw content on stdout
        .stdout(predicate::str::contains("This is not cargo test output"))
        // Passthrough emits [notice] on stderr
        .stderr(predicate::str::contains("[notice]"));
}

#[test]
fn test_cargo_passthrough_preserves_raw_content() {
    let garbage = "completely unparseable output\nno json, no regex match\n";
    skim_cmd()
        .args(["test", "cargo"])
        .write_stdin(garbage)
        .assert()
        .stdout(predicate::str::contains("completely unparseable output"))
        .stderr(predicate::str::contains("[notice]"));
}

// ============================================================================
// Vitest: Tier 1 (JSON) — Full
// ============================================================================

#[test]
fn test_vitest_tier1_json_pass() {
    let fixture = include_str!("fixtures/vitest/vitest_pass.json");
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS: 3"))
        .stdout(predicate::str::contains("FAIL: 0"));
}

#[test]
fn test_vitest_tier1_json_fail_with_detail() {
    let fixture = include_str!("fixtures/vitest/vitest_fail.json");
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin(fixture)
        .assert()
        .code(predicate::ne(0))
        .stdout(predicate::str::contains("FAIL: 1"))
        .stdout(predicate::str::contains("PASS: 1"));
}

#[test]
fn test_vitest_tier1_pnpm_prefix() {
    let fixture = include_str!("fixtures/vitest/vitest_pnpm_prefix.json");
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS: 2"));
}

// ============================================================================
// Vitest: Tier 2 (regex) — Degraded
// ============================================================================

#[test]
fn test_vitest_tier2_regex_pipe_format() {
    // Pipe-format summary triggers tier 2 regex
    let input = "Tests  3 passed | 0 failed | 3 total\n";
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS: 3"))
        .stderr(predicate::str::contains("[warning]"));
}

#[test]
fn test_vitest_tier2_regex_fail_fixture() {
    let fixture = include_str!("fixtures/cmd/test/vitest_regex_fail.txt");
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin(fixture)
        .assert()
        .code(predicate::ne(0))
        .stdout(predicate::str::contains("FAIL: 1"))
        .stderr(predicate::str::contains("[warning]"));
}

// ============================================================================
// Vitest: Tier 3 (passthrough) — Passthrough
// ============================================================================

#[test]
fn test_vitest_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin("random garbage not vitest output\n")
        .assert()
        .stderr(predicate::str::contains("[notice]"));
}

// ============================================================================
// Pytest: Tier 1 (text state machine) — Full
// ============================================================================

#[test]
fn test_pytest_tier1_pass() {
    let fixture = include_str!("fixtures/cmd/test/pytest_pass.txt");
    skim_cmd()
        .args(["test", "pytest"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS: 5"))
        .stdout(predicate::str::contains("FAIL: 0"));
}

#[test]
fn test_pytest_tier1_fail_with_detail() {
    let fixture = include_str!("fixtures/cmd/test/pytest_fail.txt");
    skim_cmd()
        .args(["test", "pytest"])
        .write_stdin(fixture)
        .assert()
        .code(predicate::ne(0))
        .stdout(predicate::str::contains("FAIL: 1"))
        .stdout(predicate::str::contains("PASS: 2"));
}

#[test]
fn test_pytest_tier1_mixed() {
    let fixture = include_str!("fixtures/cmd/test/pytest_mixed.txt");
    skim_cmd()
        .args(["test", "pytest"])
        .write_stdin(fixture)
        .assert()
        .code(predicate::ne(0))
        .stdout(predicate::str::contains("PASS: 4"))
        .stdout(predicate::str::contains("FAIL: 1"))
        .stdout(predicate::str::contains("SKIP: 1"));
}

// ============================================================================
// Pytest: Tier 2 (passthrough) — Passthrough
// ============================================================================
// NOTE: Pytest has only 2 tiers: tier 1 (text state machine) and tier 2
// (passthrough). There is no regex degradation tier for pytest.

#[test]
fn test_pytest_passthrough_garbage() {
    skim_cmd()
        .args(["test", "pytest"])
        .write_stdin("random garbage not pytest output\n")
        .assert()
        .stderr(predicate::str::contains("[notice]"));
}

#[test]
fn test_pytest_passthrough_preserves_raw() {
    let garbage = "some unrecognized tool output\nline 2\nline 3\n";
    skim_cmd()
        .args(["test", "pytest"])
        .write_stdin(garbage)
        .assert()
        .stdout(predicate::str::contains("some unrecognized tool output"));
}
