//! Integration tests for `skim vitest` subcommand (#48).
//!
//! v2.8.0: Flat dispatch — `skim vitest` replaces `skim test vitest`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;
mod common;

fn fixture_path(name: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push("fixtures");
    path.push("cmd");
    path.push("test");
    path.push(name);
    path
}

fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|e| panic!("Failed to read fixture {name}: {e}"))
}

fn skim_cmd() -> Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd
}

// ============================================================================
// Help output
// ============================================================================

#[test]
fn test_skim_vitest_help() {
    // v2.8.0: `skim vitest --help` — "test" is no longer a subcommand.
    //
    // Regression (ADR-008): use `skim_cmd()` so SKIM_PASSTHROUGH is removed and
    // the daemon guard is ACTIVE. `--help` must be treated as finite (print and
    // exit) — otherwise the guard would route `vitest --help` through
    // `run_inherited_passthrough` and exit 127 instead of printing skim's help.
    skim_cmd()
        .arg("vitest")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim vitest"));
}

// ============================================================================
// Stdin piping (Tier 1 JSON parse)
// ============================================================================

#[test]
fn test_skim_test_vitest_stdin_pass() {
    let fixture = read_fixture("vitest_pass.json");

    skim_cmd()
        .args(["vitest", "run"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 3"))
        .stdout(predicate::str::contains("fail: 0"));
}

#[test]
fn test_skim_test_vitest_stdin_fail() {
    let fixture = read_fixture("vitest_fail.json");

    skim_cmd()
        .args(["vitest", "run"])
        .write_stdin(fixture)
        .assert()
        .failure()
        .stdout(predicate::str::contains("pass: 1"))
        .stdout(predicate::str::contains("fail: 1"))
        .stdout(predicate::str::contains("skip: 1"))
        .stdout(predicate::str::contains("math > divides"));
}

#[test]
fn test_skim_test_vitest_stdin_pnpm_prefix() {
    let fixture = read_fixture("vitest_pnpm_prefix.json");

    skim_cmd()
        .args(["vitest", "run"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 2"))
        .stdout(predicate::str::contains("fail: 0"));
}

// ============================================================================
// Stdin piping (Tier 2 regex fallback)
// ============================================================================

#[test]
fn test_skim_test_vitest_stdin_regex_fallback() {
    let input = "Tests  5 passed | 1 failed | 6 total";

    skim_cmd()
        .args(["--debug", "vitest", "run"])
        .write_stdin(input)
        .assert()
        .failure() // fail > 0
        .stdout(predicate::str::contains("pass: 5"))
        .stdout(predicate::str::contains("fail: 1"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// Stdin piping (Tier 3 passthrough)
// ============================================================================

#[test]
fn test_skim_test_vitest_stdin_passthrough() {
    // Fix #3.1: passthrough with ExitSource::Stdin now returns exit 0.
    // Unparseable stdin has no spawned process to report failure; resolve_exit_code
    // defers to the process exit which is absent (0) for pure stdin paths.
    // The verbatim raw content is still forwarded to stdout, and the [skim:notice]
    // marker is still emitted to stderr via emit_markers.
    let input = "completely unparseable output";

    skim_cmd()
        .args(["--debug", "vitest", "run"])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(predicate::str::contains("completely unparseable output"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// Unknown runner
// ============================================================================

// v2.8.0: "test" is no longer a subcommand — unknown tool names are now
// Unknown subcommands handled at the dispatch level. This test is removed
// because there is no "test <runner>" dispatch path anymore.

// ============================================================================
// Jest alias
// ============================================================================

#[test]
fn test_skim_test_jest_alias_works() {
    let fixture = read_fixture("vitest_pass.json");

    // `skim jest` should route to the vitest parser (compatible format)
    skim_cmd()
        .arg("jest")
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 3"));
}

// ============================================================================
// Args prevent stdin mode (regression: subprocess stdin detection bug)
// ============================================================================

#[test]
fn test_vitest_with_args_does_not_read_stdin() {
    // assert_cmd provides non-terminal stdin by default. Without write_stdin,
    // stdin is non-terminal but EMPTY. `should_read_stdin(["run"])` returns true
    // (run is treated as a routing hint, not a real arg), but `try_read_stdin`
    // finds empty stdin → returns Ok(None) → falls through to the spawn path.
    //
    // The key assertion: stdout must NOT be empty. Skim spawns vitest run (which
    // is not installed), producing an error message on stdout.
    common::skim()
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .arg("vitest")
        .arg("run")
        .assert()
        .failure()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn test_jest_with_args_does_not_read_stdin() {
    common::skim()
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .arg("jest")
        .arg("run")
        .assert()
        .failure()
        .stdout(predicate::str::is_empty().not());
}
