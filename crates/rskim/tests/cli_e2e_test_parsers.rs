//! E2E tests for test parser degradation tiers (#54).
//!
//! Tests each parser at different degradation tiers via stdin piping,
//! verifying structured output markers and stderr diagnostics.
//!
//! Tier behavior reference (from emit_markers in output/mod.rs):
//! - Full: no stderr markers
//! - Degraded: "[skim:warning] ..." on stderr (only with --debug)
//! - Passthrough: "[skim:notice] output passed through without parsing" on stderr (only with --debug)

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
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
        .args(["--debug", "test", "cargo"])
        .write_stdin(fixture)
        .assert()
        .success()
        // Content is passed through as-is
        .stdout(predicate::str::contains("PASS"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

#[test]
fn test_cargo_nextest_fail_passthrough_via_stdin() {
    // Without "nextest" in args, nextest output hits passthrough tier
    let fixture = include_str!("fixtures/cmd/test/cargo_nextest_fail.txt");
    skim_cmd()
        .args(["--debug", "test", "cargo"])
        .write_stdin(fixture)
        .assert()
        // Exit code 0 from synthetic stdin exit code
        .code(0)
        .stdout(predicate::str::contains("FAIL"))
        .stderr(predicate::str::contains("[skim:notice]"));
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
        .args(["--debug", "test", "cargo"])
        .write_stdin(text_input)
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS: 5"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// Cargo: Tier 3 (passthrough) — Passthrough
// ============================================================================

#[test]
fn test_cargo_tier3_passthrough_garbage_input() {
    let fixture = include_str!("fixtures/cmd/test/cargo_passthrough.txt");
    skim_cmd()
        .args(["--debug", "test", "cargo"])
        .write_stdin(fixture)
        .assert()
        // Passthrough preserves raw content on stdout
        .stdout(predicate::str::contains("This is not cargo test output"))
        // Passthrough emits [skim:notice] on stderr when --debug is set
        .stderr(predicate::str::contains("[skim:notice]"));
}

#[test]
fn test_cargo_passthrough_preserves_raw_content() {
    let garbage = "completely unparseable output\nno json, no regex match\n";
    skim_cmd()
        .args(["--debug", "test", "cargo"])
        .write_stdin(garbage)
        .assert()
        .stdout(predicate::str::contains("completely unparseable output"))
        .stderr(predicate::str::contains("[skim:notice]"));
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
        .args(["--debug", "test", "vitest"])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS: 3"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

#[test]
fn test_vitest_tier2_regex_fail_fixture() {
    let fixture = include_str!("fixtures/cmd/test/vitest_regex_fail.txt");
    skim_cmd()
        .args(["--debug", "test", "vitest"])
        .write_stdin(fixture)
        .assert()
        .code(predicate::ne(0))
        .stdout(predicate::str::contains("FAIL: 1"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// Vitest: Tier 3 (passthrough) — Passthrough
// ============================================================================

#[test]
fn test_vitest_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "test", "vitest"])
        .write_stdin("random garbage not vitest output\n")
        .assert()
        .stderr(predicate::str::contains("[skim:notice]"));
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
        .args(["--debug", "test", "pytest"])
        .write_stdin("random garbage not pytest output\n")
        .assert()
        .stderr(predicate::str::contains("[skim:notice]"));
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

// ============================================================================
// Silent stderr without --debug
// ============================================================================

/// Verify that degraded/passthrough output produces NO stderr markers when
/// --debug is not set (the default). This is a subprocess-isolated test so
/// AtomicBool state from other tests does not interfere.
#[test]
fn test_vitest_degraded_silent_without_debug() {
    let input = "Tests  3 passed | 0 failed | 3 total\n";
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS: 3"))
        // No --debug flag: stderr must contain no skim markers
        .stderr(predicate::str::contains("[skim:warning]").not())
        .stderr(predicate::str::contains("[skim:notice]").not());
}

#[test]
fn test_vitest_passthrough_silent_without_debug() {
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin("random garbage not vitest output\n")
        .assert()
        // No --debug flag: no markers on stderr
        .stderr(predicate::str::contains("[skim:notice]").not())
        .stderr(predicate::str::contains("[skim:warning]").not());
}

// ============================================================================
// stderr hint on compressed failure (Fix E)
// ============================================================================

/// Pipe a failing vitest JSON fixture through stdin to `skim test vitest`,
/// capture stderr, verify it contains `[skim] compressed output` (the hint
/// that tells the user how to see full raw output).
#[test]
fn test_stderr_hint_on_compressed_failure() {
    let fixture = include_str!("fixtures/vitest/vitest_fail.json");
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin(fixture)
        .assert()
        .code(predicate::ne(0))
        .stderr(predicate::str::contains("[skim] compressed output"));
}

/// Pipe a passing vitest JSON fixture through stdin, capture stderr, verify
/// it does NOT contain `[skim]` (hint must only fire on non-zero exit codes).
#[test]
fn test_no_stderr_hint_on_success() {
    let fixture = include_str!("fixtures/vitest/vitest_pass.json");
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]").not());
}

/// Pipe unparseable output through stdin (triggers Passthrough tier), capture
/// stderr, verify the hint is NOT emitted. Passthrough output is uncompressed
/// so the hint is not needed.
#[test]
fn test_no_stderr_hint_on_passthrough() {
    // Unparseable input triggers tier 3 passthrough.
    // stdin path uses synthetic exit_code Some(0), so process exits 0.
    // The hint must NOT fire because result.is_passthrough() is true.
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin("completely unparseable garbage output that matches nothing\n")
        .assert()
        .stderr(predicate::str::contains("[skim] compressed output").not());
}

/// Pipe a failing vitest JSON fixture through stdin to `skim test vitest`,
/// capture stderr, and assert it contains BOTH `[skim] compressed output`
/// AND `SKIM_PASSTHROUGH=1`. This validates the full hint message format,
/// not just the prefix.
#[test]
fn test_stderr_hint_contains_passthrough_instruction() {
    let fixture = include_str!("fixtures/vitest/vitest_fail.json");
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin(fixture)
        .assert()
        .code(predicate::ne(0))
        .stderr(predicate::str::contains("[skim] compressed output"))
        .stderr(predicate::str::contains("SKIM_PASSTHROUGH=1"));
}

// ============================================================================
// Go test: passthrough exec path (#49)
// ============================================================================
//
// NOTE: `skim test go` does NOT read from stdin. Unlike vitest/cargo, the go
// handler always spawns the `go` binary directly (with `-json` injection or
// without it in SKIM_PASSTHROUGH mode). There is no stdin-reading code path.
// The unit tests in src/cmd/test/go.rs cover the three-tier parse() function
// exhaustively. The E2E test below verifies the passthrough exec path
// dispatches to `go` and surfaces the install hint when `go` is absent.

/// Verify that `skim test go` with SKIM_PASSTHROUGH=1 attempts to exec `go`
/// and surfaces the install hint when the binary is not available.
///
/// This test is skipped when `go` is installed because the exec succeeds (or
/// fails for a different reason — missing package args) and the install hint
/// is not emitted. The intent is to exercise the passthrough exec code path
/// on CI environments where `go` is not present.
#[test]
fn test_go_passthrough_exec_path_surfaces_install_hint() {
    // Skip this test if `go` is installed — the exec path succeeds and the
    // "install Go" hint is not emitted.
    if std::process::Command::new("go")
        .arg("version")
        .output()
        .is_ok()
    {
        return;
    }

    // With SKIM_PASSTHROUGH=1 and no `go` binary, the passthrough branch
    // tries runner.run("go", &["test"]) which fails with "failed to execute".
    // The go::run() function maps that error to include the install hint.
    let output = skim_cmd()
        .args(["test", "go"])
        .env("SKIM_PASSTHROUGH", "1")
        .output()
        .unwrap();

    // Process must exit non-zero (go binary not found).
    assert_ne!(
        output.status.code(),
        Some(0),
        "expected non-zero exit when go is not installed"
    );

    // The error output must contain the install hint injected by go::run().
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("https://go.dev/dl/"),
        "expected Go install hint in error output, got: {stderr}"
    );
}

// ============================================================================
// Vitest: failure context banner (#49)
// ============================================================================

/// Pipe a failing vitest JSON fixture through stdin to `skim test vitest`
/// and assert that stdout contains the `--- failure context` banner.
///
/// When skim compresses a failing test run, it appends raw tail lines
/// under a banner so the agent can see the actual failure details without
/// needing to re-run with SKIM_PASSTHROUGH=1.
#[test]
fn test_vitest_failure_context_banner_present() {
    let fixture = include_str!("fixtures/vitest/vitest_fail.json");
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin(fixture)
        .assert()
        .code(predicate::ne(0))
        .stdout(predicate::str::contains("--- failure context"));
}
