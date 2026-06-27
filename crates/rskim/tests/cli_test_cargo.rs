//! Integration tests for `skim cargo test` subcommand (#46).
//!
//! v2.8.0: Flat dispatch — `skim cargo test` replaces `skim test cargo`.
//!
//! Tests the end-to-end cargo test parser via the CLI binary.

use predicates::prelude::*;
mod common;

// ============================================================================
// Real cargo test execution
// ============================================================================

#[test]
fn test_skim_test_cargo_in_this_repo() {
    // Run `skim cargo test -p rskim-core` on skim's own repo.
    // This executes a real `cargo test` and parses the output.
    // We use -p rskim-core to limit scope and speed up the test.
    let assert = common::skim()
        .args(["cargo", "test", "-p", "rskim-core"])
        .env_remove("SKIM_PASSTHROUGH")
        .timeout(std::time::Duration::from_secs(120))
        .assert();

    // Should produce structured output with pass count (tier 2 regex)
    assert.stdout(predicate::str::contains("pass:"));
}

// ============================================================================
// Help text
// ============================================================================

#[test]
fn test_skim_cargo_help() {
    // v2.8.0: `skim cargo --help` — "test" is no longer a subcommand.
    common::skim()
        .args(["cargo", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo"));
}

// ============================================================================
// Unknown runner
// ============================================================================

// v2.8.0: "test" is no longer a subcommand. Unknown tool names are unknown
// subcommands at the dispatch level. This test is removed.

// ============================================================================
// Piped stdin parsing
// ============================================================================

#[test]
fn test_skim_test_cargo_stdin_json() {
    // Pipe cargo JSON output via stdin
    let json_input = r#"{"type":"suite","event":"started","test_count":2}
{"type":"test","event":"ok","name":"test_a","exec_time":0.001}
{"type":"test","event":"ok","name":"test_b","exec_time":0.002}
{"type":"suite","event":"ok","passed":2,"failed":0,"ignored":0,"measured":0,"filtered_out":0,"exec_time":0.003}
"#;

    common::skim()
        .args(["cargo", "test"])
        // Remove SKIM_PASSTHROUGH so compression is not bypassed inside the child process.
        .env_remove("SKIM_PASSTHROUGH")
        .write_stdin(json_input)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 2"))
        .stdout(predicate::str::contains("fail: 0"));
}

#[test]
fn test_skim_test_cargo_stdin_plain_text() {
    // Pipe plain text cargo test output (tier 2 regex)
    let text_input = "running 5 tests\n\
        test test_one ... ok\n\
        test test_two ... ok\n\
        test test_three ... ok\n\
        test test_four ... ok\n\
        test test_five ... ok\n\n\
        test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";

    common::skim()
        .args(["cargo", "test"])
        // Remove SKIM_PASSTHROUGH so compression is not bypassed inside the child process.
        .env_remove("SKIM_PASSTHROUGH")
        .write_stdin(text_input)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 5"));
}

// ============================================================================
// `skim cargo nextest` — dispatch correctness (issue: missing coverage)
// ============================================================================

/// `skim cargo nextest` must not produce an "unknown subcommand" error.
///
/// The nextest dispatch path keeps the "nextest" token in args (unlike the "test"
/// arm which strips it), routing through a distinct code path in `dispatch_cargo`.
///
/// Design note: `skim cargo nextest` always spawns the real cargo binary because
/// `should_read_stdin` returns false when args are non-empty (the "nextest" token
/// is passed through as a runner arg). Piped stdin is therefore not available for
/// this subcommand. The help text confirms nextest is listed as supported.
#[test]
fn test_skim_cargo_nextest_is_listed_in_help_as_supported() {
    common::skim()
        .args(["cargo", "--help"])
        .assert()
        .success()
        // nextest must appear in the help text as a supported subcommand
        .stdout(predicate::str::contains("nextest"))
        // must not have an unknown-subcommand error
        .stderr(predicate::str::contains("unknown subcommand").not());
}

/// Piping nextest-style output to `skim cargo test` (without the nextest token)
/// exercises the passthrough tier because `is_nextest=false` (no "nextest" in args).
///
/// NOTE: This is the nearest proxy for "pipe nextest output and get output".
/// The nextest format does not match the JSON or `test result:` regex tiers,
/// so it passes through unchanged.  The test verifies the dispatch succeeds
/// and the content is forwarded rather than producing an error.
#[test]
fn test_skim_cargo_nextest_output_piped_via_test_arm_passes_through() {
    let nextest_pass = include_str!("fixtures/cmd/test/cargo_nextest_pass.txt");

    common::skim()
        .args(["cargo", "test"])
        .write_stdin(nextest_pass)
        .assert()
        .success()
        // nextest content is passed through (passthrough tier — PASS token present)
        .stdout(predicate::str::contains("PASS"));
}

// ============================================================================
// `skim cargo t` and `skim cargo b` short aliases (issue: zero coverage)
// ============================================================================

/// `skim cargo t` is an alias for `skim cargo test`.
/// Pipe a minimal JSON test fixture and verify the alias dispatches correctly
/// and produces compressed output (pass count present).
#[test]
fn test_skim_cargo_t_alias_stdin_json() {
    let json_input = r#"{"type":"suite","event":"started","test_count":1}
{"type":"test","event":"ok","name":"alias_test","exec_time":0.001}
{"type":"suite","event":"ok","passed":1,"failed":0,"ignored":0,"measured":0,"filtered_out":0,"exec_time":0.001}
"#;

    common::skim()
        .args(["cargo", "t"])
        // Remove SKIM_PASSTHROUGH so compression is not bypassed inside the child process.
        .env_remove("SKIM_PASSTHROUGH")
        .write_stdin(json_input)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 1"))
        .stdout(predicate::str::contains("fail: 0"));
}

/// `skim cargo b` is an alias for `skim cargo build`.
///
/// Build commands always spawn the real executable (no stdin path), so the
/// alias is verified by running real `cargo build` on this repo.
/// Since the repo is already built, incremental compilation is fast.
#[test]
fn test_skim_cargo_b_alias_dispatches_to_build() {
    common::skim()
        .args(["cargo", "b"])
        // Must not produce an error about unknown subcommand
        .assert()
        .success()
        .stderr(predicate::str::contains("unknown subcommand").not())
        // Must also not show "missing subcommand" (the alias was recognised)
        .stderr(predicate::str::contains("missing subcommand").not());
}

// ============================================================================
// Unknown cargo subcommand — error path coverage
// ============================================================================

/// `skim cargo unknownthing` must fail with an "unknown subcommand" error
/// on stderr. This covers the `unknown` arm in `dispatch_cargo`.
#[test]
fn test_skim_cargo_unknown_subcommand_errors() {
    common::skim()
        .args(["cargo", "unknownthing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown subcommand"));
}

// ============================================================================
// Error-path E2E coverage — go, npm, pnpm, pip unknown/missing subcommands
// ============================================================================

/// `skim go unknownthing` must fail with an "unknown subcommand" error.
/// Covers the `unknown` arm in `dispatch_go`.
#[test]
fn test_skim_go_unknown_subcommand_errors() {
    common::skim()
        .args(["go", "unknownthing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown subcommand"));
}

/// `skim npm unknownthing` must fail with an "unknown subcommand" error.
/// Covers the `other` arm in `pkg::npm::run`.
#[test]
fn test_skim_npm_unknown_subcommand_errors() {
    common::skim()
        .args(["npm", "unknownthing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown subcommand"));
}

/// `skim pnpm unknownthing` must fail with an "unknown subcommand" error.
/// Covers the `other` arm in `pkg::pnpm::run`.
#[test]
fn test_skim_pnpm_unknown_subcommand_errors() {
    common::skim()
        .args(["pnpm", "unknownthing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown subcommand"));
}

/// `skim pip unknownthing` must fail with an "unknown subcommand" error.
/// Covers the `other` arm in `pkg::pip::run`.
#[test]
fn test_skim_pip_unknown_subcommand_errors() {
    common::skim()
        .args(["pip", "unknownthing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown subcommand"));
}
