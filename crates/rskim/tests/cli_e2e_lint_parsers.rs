//! E2E tests for lint parser degradation tiers (#104).
//!
//! Tests each linter at different degradation tiers via stdin piping,
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
// ESLint: Tier 1 (JSON) -- Full
// ============================================================================

#[test]
fn test_eslint_tier1_json_pass() {
    let fixture = include_str!("fixtures/cmd/lint/eslint_pass.json");
    skim_cmd()
        .args(["lint", "eslint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT OK"));
}

#[test]
fn test_eslint_tier1_json_fail() {
    let fixture = include_str!("fixtures/cmd/lint/eslint_fail.json");
    skim_cmd()
        .args(["lint", "eslint"])
        .write_stdin(fixture)
        .assert()
        .code(0) // stdin mode always exits 0
        .stdout(predicate::str::contains("LINT:"))
        .stdout(predicate::str::contains("2 errors"))
        .stdout(predicate::str::contains("3 warnings"));
}

// ============================================================================
// ESLint: Tier 2 (regex) -- Degraded
// ============================================================================

#[test]
fn test_eslint_tier2_regex_degraded() {
    let fixture = include_str!("fixtures/cmd/lint/eslint_text.txt");
    skim_cmd()
        .args(["lint", "eslint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stderr(predicate::str::contains("[warning]"));
}

// ============================================================================
// ESLint: Tier 3 (passthrough)
// ============================================================================

#[test]
fn test_eslint_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["lint", "eslint"])
        .write_stdin("random garbage not eslint output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[notice]"));
}

// ============================================================================
// ESLint: --json flag
// ============================================================================

#[test]
fn test_eslint_json_flag_full() {
    let fixture = include_str!("fixtures/cmd/lint/eslint_fail.json");
    skim_cmd()
        .args(["lint", "--json", "eslint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"tool\":\"eslint\""))
        .stdout(predicate::str::contains("\"errors\":2"));
}

// ============================================================================
// Ruff: Tier 1 (JSON) -- Full
// ============================================================================

#[test]
fn test_ruff_tier1_json_fail() {
    let fixture = include_str!("fixtures/cmd/lint/ruff_fail.json");
    skim_cmd()
        .args(["lint", "ruff"])
        .write_stdin(fixture)
        .assert()
        .code(0)
        .stdout(predicate::str::contains("LINT:"))
        .stdout(predicate::str::contains("3 errors"));
}

// ============================================================================
// Ruff: Tier 2 (regex) -- Degraded
// ============================================================================

#[test]
fn test_ruff_tier2_regex_degraded() {
    let fixture = include_str!("fixtures/cmd/lint/ruff_text.txt");
    skim_cmd()
        .args(["lint", "ruff"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stderr(predicate::str::contains("[warning]"));
}

// ============================================================================
// Ruff: Tier 3 (passthrough)
// ============================================================================

#[test]
fn test_ruff_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["lint", "ruff"])
        .write_stdin("random garbage not ruff output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[notice]"));
}

// ============================================================================
// Ruff: --json flag
// ============================================================================

#[test]
fn test_ruff_json_flag_full() {
    let fixture = include_str!("fixtures/cmd/lint/ruff_fail.json");
    skim_cmd()
        .args(["lint", "--json", "ruff"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"tool\":\"ruff\""))
        .stdout(predicate::str::contains("\"errors\":3"));
}

// ============================================================================
// mypy: Tier 1 (NDJSON) -- Full
// ============================================================================

#[test]
fn test_mypy_tier1_json_fail() {
    let fixture = include_str!("fixtures/cmd/lint/mypy_fail.json");
    skim_cmd()
        .args(["lint", "mypy"])
        .write_stdin(fixture)
        .assert()
        .code(0)
        .stdout(predicate::str::contains("LINT:"))
        .stdout(predicate::str::contains("3 errors"));
}

// ============================================================================
// mypy: Tier 2 (regex) -- Degraded
// ============================================================================

#[test]
fn test_mypy_tier2_regex_degraded() {
    let fixture = include_str!("fixtures/cmd/lint/mypy_text.txt");
    skim_cmd()
        .args(["lint", "mypy"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stderr(predicate::str::contains("[warning]"));
}

// ============================================================================
// mypy: Tier 3 (passthrough)
// ============================================================================

#[test]
fn test_mypy_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["lint", "mypy"])
        .write_stdin("random garbage not mypy output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[notice]"));
}

// ============================================================================
// mypy: --json flag
// ============================================================================

#[test]
fn test_mypy_json_flag_full() {
    let fixture = include_str!("fixtures/cmd/lint/mypy_fail.json");
    skim_cmd()
        .args(["lint", "--json", "mypy"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"tool\":\"mypy\""))
        .stdout(predicate::str::contains("\"errors\":3"));
}

// ============================================================================
// golangci-lint: Tier 1 (JSON) -- Full
// ============================================================================

#[test]
fn test_golangci_tier1_json_fail() {
    let fixture = include_str!("fixtures/cmd/lint/golangci_fail.json");
    skim_cmd()
        .args(["lint", "golangci"])
        .write_stdin(fixture)
        .assert()
        .code(0)
        .stdout(predicate::str::contains("LINT:"));
}

// ============================================================================
// golangci-lint: Tier 2 (regex) -- Degraded
// ============================================================================

#[test]
fn test_golangci_tier2_regex_degraded() {
    let fixture = include_str!("fixtures/cmd/lint/golangci_text.txt");
    skim_cmd()
        .args(["lint", "golangci"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stderr(predicate::str::contains("[warning]"));
}

// ============================================================================
// golangci-lint: Tier 3 (passthrough)
// ============================================================================

#[test]
fn test_golangci_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["lint", "golangci"])
        .write_stdin("random garbage not golangci output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[notice]"));
}

// ============================================================================
// golangci-lint: --json flag
// ============================================================================

#[test]
fn test_golangci_json_flag_full() {
    let fixture = include_str!("fixtures/cmd/lint/golangci_fail.json");
    skim_cmd()
        .args(["lint", "--json", "golangci"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"tool\":\"golangci\""));
}

// ============================================================================
// Dispatcher: help and unknown linter
// ============================================================================

#[test]
fn test_lint_help() {
    skim_cmd()
        .args(["lint", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available linters:"))
        .stdout(predicate::str::contains("eslint"))
        .stdout(predicate::str::contains("ruff"))
        .stdout(predicate::str::contains("mypy"))
        .stdout(predicate::str::contains("golangci"));
}

#[test]
fn test_lint_unknown_linter() {
    skim_cmd()
        .args(["lint", "unknown-linter"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown linter 'unknown-linter'"));
}

#[test]
fn test_lint_no_args_shows_help() {
    skim_cmd()
        .args(["lint"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available linters:"));
}
