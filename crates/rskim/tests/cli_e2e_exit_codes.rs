//! E2E exit code verification for all parsers (#54).
//!
//! Systematic per-parser exit code tests via stdin piping.
//! Validates that each parser produces the correct exit code when
//! processing fixture data.
//!
//! ## Exit code semantics by parser
//!
//! - **cargo test**: Uses `run_parsed_command_with_exit()` (#317): the final
//!   exit is `max(child_exit, derived)`. On the stdin path the child exit is
//!   the fabricated `Some(0)`, but a parsed `fail > 0` derives exit 1 — a
//!   piped failing run no longer exits 0. Passthrough-tier stdin (unparseable
//!   content) still exits 0 because nothing was derived.
//!
//! - **pytest/vitest**: Have their own `run()` implementations that infer exit
//!   code from parsed results when `exit_code` is `None`. Failures in parsed
//!   content produce non-zero exit codes.
//!
//! - **go test**: Does NOT support stdin (always runs `go test`).
//! - **build parsers**: Do NOT support stdin (always run the real command).

use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

// ============================================================================
// Cargo test exit codes
// ============================================================================

#[test]
fn test_exit_code_cargo_pass_json() {
    let fixture = include_str!("fixtures/cmd/test/cargo_pass.json");
    skim_cmd()
        .args(["cargo", "test"])
        .write_stdin(fixture)
        .assert()
        .code(0);
}

#[test]
fn test_exit_code_cargo_fail_json() {
    // #317: a piped failing run must exit non-zero — the parser derives
    // exit 1 from fail > 0 even though the stdin transport fabricates exit 0.
    let fixture = include_str!("fixtures/cmd/test/cargo_fail.json");
    skim_cmd()
        .args(["cargo", "test"])
        .write_stdin(fixture)
        .assert()
        .code(1)
        .stdout(predicate::str::contains("fail: 1"));
}

#[test]
fn test_exit_code_cargo_stable_panic_via_stdin() {
    // The exact #317 Addendum-2 repro: stable-toolchain output with a panic.
    // Both the panic diagnostic AND a failure exit code must survive.
    let fixture = include_str!("fixtures/cmd/test/cargo_panic_char_boundary.txt");
    skim_cmd()
        .args(["cargo", "test"])
        .write_stdin(fixture)
        .assert()
        .code(1)
        .stdout(predicate::str::contains("fail: 1"))
        .stdout(predicate::str::contains("panicked at"))
        .stdout(predicate::str::contains("char boundary"));
}

#[test]
fn test_exit_code_cargo_nextest_pass() {
    let fixture = include_str!("fixtures/cmd/test/cargo_nextest_pass.txt");
    skim_cmd()
        .args(["cargo", "test"])
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
        .args(["--debug", "cargo", "test"])
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
        .args(["--debug", "cargo", "test"])
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
    let fixture = include_str!("fixtures/cmd/test/vitest_pass.json");
    skim_cmd()
        .args(["vitest", "run"])
        .write_stdin(fixture)
        .assert()
        .code(0);
}

#[test]
fn test_exit_code_vitest_fail_json() {
    // Vitest infers exit code from parsed results (fail > 0 => FAILURE)
    let fixture = include_str!("fixtures/cmd/test/vitest_fail.json");
    skim_cmd()
        .args(["vitest", "run"])
        .write_stdin(fixture)
        .assert()
        .code(predicate::ne(0));
}

#[test]
fn test_exit_code_vitest_passthrough_garbage() {
    // Vitest passthrough always returns ExitCode::FAILURE
    skim_cmd()
        .args(["vitest", "run"])
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
        .args(["pytest"])
        .write_stdin(fixture)
        .assert()
        .code(0);
}

#[test]
fn test_exit_code_pytest_fail() {
    // Pytest infers exit code from parsed results (fail > 0 => FAILURE)
    let fixture = include_str!("fixtures/cmd/test/pytest_fail.txt");
    skim_cmd()
        .args(["pytest"])
        .write_stdin(fixture)
        .assert()
        .code(predicate::ne(0));
}

#[test]
fn test_exit_code_pytest_all_fail() {
    let fixture = include_str!("fixtures/cmd/test/pytest_all_fail.txt");
    skim_cmd()
        .args(["pytest"])
        .write_stdin(fixture)
        .assert()
        .code(predicate::ne(0));
}

#[test]
fn test_exit_code_pytest_passthrough_garbage() {
    // Pytest passthrough: exit_code is None (stdin) so it infers FAILURE
    skim_cmd()
        .args(["pytest"])
        .write_stdin("random garbage not pytest output\n")
        .assert()
        .code(predicate::ne(0));
}

// ============================================================================
// Fix B — diff exit-1 Full-tier E2E (fix/rewrite-hook-falseneg)
//
// These tests exercise the INTEGRATED path that Fix B changes: a real diff on
// two differing files exits 1 with a non-empty compressed body (Full tier).
// The pre-existing grep no-match test hits the Passthrough tier (empty body →
// already silent), so it does NOT exercise the new `!is_benign_exit1` term in
// `should_emit_compressed_hint`. These tests do.
//
// Discriminates against a regression in `RecordReport::program` threading: if
// `program` were not forwarded correctly into `should_emit_compressed_hint`,
// the benign guard would not fire and the compressed-output hint would appear
// in stderr — causing the `.stderr(predicate::str::contains(...).not())`
// assertion to FAIL, catching the regression.
// ============================================================================

/// diff exit 1 = files differ — Full-tier compressed body, hint suppressed.
///
/// This is the core Fix B integration test: skim compresses the diff output
/// (non-empty body → Full tier), propagates exit code 1, and does NOT print
/// the "[skim] compressed output" hint because diff is in BENIGN_EXIT1_PROGRAMS.
#[test]
fn test_diff_differing_files_exit1_full_tier_hint_suppressed() {
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("a.txt");
    let file_b = dir.path().join("b.txt");
    fs::write(&file_a, "alpha\nbeta\n").unwrap();
    fs::write(&file_b, "alpha\ngamma\n").unwrap();

    skim_cmd()
        .args(["diff", file_a.to_str().unwrap(), file_b.to_str().unwrap()])
        .assert()
        // Exit code must be propagated faithfully (files differ = 1).
        .code(1)
        // stdout must contain compressed diff body (Full tier, non-empty).
        // FileResult::render produces "diff 1" header when shown == total.
        .stdout(predicate::str::contains("diff"))
        .stdout(predicate::str::contains("changed"))
        // The hint must NOT appear: diff exit 1 is benign ("files differ"),
        // not an error. Printing the hint would mislead agents.
        .stderr(predicate::str::contains("[skim] compressed output").not());
}

/// diff exit >=2 = read error — hint DOES fire (not benign, real error).
///
/// Proves the suppression is exit-code-aware, not blanket: only exit 1 is
/// benign for diff. A missing-file error exits 2 and SHOULD get the hint so
/// agents know skim re-encoded the (forwarded-raw) diagnostic output.
///
/// Because exit 2 is an unexpected failure (not in expected_exit_codes=[1]),
/// skim actually raw-forwards the output with the "raw output (not compressed)"
/// notice, not the "compressed output" hint. Either notice signals the agent
/// to investigate — the key property is that diff exit 2 does NOT produce a
/// silent suppression identical to the benign exit-1 path.
#[test]
fn test_diff_missing_file_exit2_not_silent() {
    skim_cmd()
        .args([
            "diff",
            "/nonexistent/skim-fix-b-a",
            "/nonexistent/skim-fix-b-b",
        ])
        .assert()
        .code(2)
        // skim must emit SOME diagnostic to stderr for exit 2 — either the
        // raw-forward notice or (if the parser somehow reaches record_and_report)
        // the compressed-output hint. Either way it must not be silent.
        .stderr(predicate::str::is_empty().not());
}
