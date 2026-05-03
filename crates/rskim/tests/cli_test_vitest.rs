//! Integration tests for `skim vitest` subcommand (#48).
//!
//! v2.8.0: Flat dispatch — `skim vitest` replaces `skim test vitest`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;

fn fixture_path(name: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push("fixtures");
    path.push("vitest");
    path.push(name);
    path
}

fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|e| panic!("Failed to read fixture {name}: {e}"))
}

// ============================================================================
// Help output
// ============================================================================

#[test]
fn test_skim_vitest_help() {
    // v2.8.0: `skim vitest --help` — "test" is no longer a subcommand.
    Command::cargo_bin("skim")
        .unwrap()
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

    Command::cargo_bin("skim")
        .unwrap()
        .arg("vitest")
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 3"))
        .stdout(predicate::str::contains("fail: 0"));
}

#[test]
fn test_skim_test_vitest_stdin_fail() {
    let fixture = read_fixture("vitest_fail.json");

    Command::cargo_bin("skim")
        .unwrap()
        .arg("vitest")
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

    Command::cargo_bin("skim")
        .unwrap()
        .arg("vitest")
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

    Command::cargo_bin("skim")
        .unwrap()
        .arg("--debug")
        .arg("vitest")
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
    let input = "completely unparseable output";

    Command::cargo_bin("skim")
        .unwrap()
        .arg("--debug")
        .arg("vitest")
        .write_stdin(input)
        .assert()
        .failure()
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
    Command::cargo_bin("skim")
        .unwrap()
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
    // assert_cmd provides non-terminal stdin by default — exactly the bug
    // scenario where `!is_terminal()` alone would incorrectly read stdin.
    // With args present, skim should attempt to spawn vitest (and fail since
    // it's not installed in the test environment).
    //
    // The key assertion: stdout must NOT be empty. Before the fix, skim would
    // read empty stdin and produce empty stdout. After the fix, the spawn path
    // is taken, producing either vitest output or an npm/runner error message.
    Command::cargo_bin("skim")
        .unwrap()
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
    Command::cargo_bin("skim")
        .unwrap()
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .arg("jest")
        .arg("run")
        .assert()
        .failure()
        .stdout(predicate::str::is_empty().not());
}
