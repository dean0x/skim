//! E2E tests for untested rewrite rules, compound commands, and hook mode (#54).
//!
//! Covers rewrite rules that have unit tests but NO previous CLI-level tests:
//! - python3 -m pytest -> skim test pytest
//! - python -m pytest -> skim test pytest
//! - npx vitest -> skim test vitest
//! - npx tsc -> skim build tsc
//! - vitest (bare) -> skim test vitest
//! - tsc (bare) -> skim build tsc
//! - cargo clippy -> skim build clippy
//!
//! Also covers hook mode and three-segment compound commands.

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

// ============================================================================
// Untested rewrite rules: python pytest variants
// ============================================================================

#[test]
fn test_rewrite_python3_m_pytest() {
    skim_cmd()
        .args(["rewrite", "python3", "-m", "pytest"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test pytest"));
}

#[test]
fn test_rewrite_python3_m_pytest_with_args() {
    skim_cmd()
        .args(["rewrite", "python3", "-m", "pytest", "-v", "tests/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test pytest -v tests/"));
}

#[test]
fn test_rewrite_python_m_pytest() {
    skim_cmd()
        .args(["rewrite", "python", "-m", "pytest"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test pytest"));
}

#[test]
fn test_rewrite_python_m_pytest_with_args() {
    skim_cmd()
        .args(["rewrite", "python", "-m", "pytest", "--tb=short"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test pytest --tb=short"));
}

// ============================================================================
// Untested rewrite rules: npx variants
// ============================================================================

#[test]
fn test_rewrite_npx_vitest() {
    skim_cmd()
        .args(["rewrite", "npx", "vitest"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test vitest"));
}

#[test]
fn test_rewrite_npx_vitest_with_args() {
    skim_cmd()
        .args(["rewrite", "npx", "vitest", "--reporter=json", "--run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test vitest --reporter=json --run"));
}

#[test]
fn test_rewrite_npx_tsc() {
    skim_cmd()
        .args(["rewrite", "npx", "tsc"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build tsc"));
}

#[test]
fn test_rewrite_npx_tsc_with_args() {
    skim_cmd()
        .args(["rewrite", "npx", "tsc", "--noEmit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build tsc --noEmit"));
}

// ============================================================================
// Untested rewrite rules: bare commands
// ============================================================================

#[test]
fn test_rewrite_vitest_bare() {
    skim_cmd()
        .args(["rewrite", "vitest"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test vitest"));
}

#[test]
fn test_rewrite_vitest_bare_with_args() {
    skim_cmd()
        .args(["rewrite", "vitest", "--run", "math"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test vitest --run math"));
}

#[test]
fn test_rewrite_tsc_bare() {
    skim_cmd()
        .args(["rewrite", "tsc"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build tsc"));
}

#[test]
fn test_rewrite_tsc_bare_with_args() {
    skim_cmd()
        .args(["rewrite", "tsc", "--noEmit", "--watch"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build tsc --noEmit --watch"));
}

#[test]
fn test_rewrite_cargo_clippy() {
    skim_cmd()
        .args(["rewrite", "cargo", "clippy"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build clippy"));
}

#[test]
fn test_rewrite_cargo_clippy_with_args() {
    skim_cmd()
        .args(["rewrite", "cargo", "clippy", "--", "-W", "clippy::pedantic"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build clippy -- -W clippy::pedantic"));
}

// ============================================================================
// Three-segment compound commands
// ============================================================================

#[test]
fn test_rewrite_three_segment_compound() {
    skim_cmd()
        .args([
            "rewrite", "--suggest", "cargo", "test", "&&", "cargo", "build", "&&", "cargo",
            "clippy",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"))
        .stdout(predicate::str::contains("\"compound\":true"));
}

#[test]
fn test_rewrite_three_segment_output() {
    skim_cmd()
        .args([
            "rewrite", "cargo", "test", "&&", "cargo", "build", "&&", "cargo", "clippy",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo"))
        .stdout(predicate::str::contains("skim build cargo"))
        .stdout(predicate::str::contains("skim build clippy"));
}

// ============================================================================
// Hook mode
// ============================================================================

#[test]
fn test_rewrite_hook_cat_code_file() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "cat src/main.rs"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("skim src/main.rs --mode=pseudo"));
}

#[test]
fn test_rewrite_hook_cargo_test() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo test"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo"));
}

#[test]
fn test_rewrite_hook_passthrough_already_rewritten() {
    // Commands starting with "skim " should pass through without modification.
    // Hook mode always exits 0 (passthrough is silent success).
    let input = serde_json::json!({
        "tool_input": {
            "command": "skim test cargo"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        // No hookSpecificOutput should be emitted for passthrough
        .stdout(predicate::str::contains("hookSpecificOutput").not());
}

#[test]
fn test_rewrite_hook_passthrough_no_match() {
    // Non-matching commands pass through silently (exit 0, no output)
    let input = serde_json::json!({
        "tool_input": {
            "command": "ls -la"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("hookSpecificOutput").not());
}

#[test]
fn test_rewrite_hook_invalid_json_passthrough() {
    // Invalid JSON input should passthrough (exit 0, no output)
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin("not valid json at all\n")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn test_rewrite_hook_missing_tool_input_passthrough() {
    // JSON without tool_input.command passes through
    let input = serde_json::json!({
        "other_field": "value"
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn test_rewrite_hook_compound_cargo_test_and_build() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo test && cargo build"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo"))
        .stdout(predicate::str::contains("skim build cargo"));
}
