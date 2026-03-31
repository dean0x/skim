//! Integration tests for `skim search` subcommand (#3).

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

#[test]
fn test_search_help() {
    skim_cmd()
        .args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim search"))
        .stdout(predicate::str::contains("--ast"));
}

#[test]
fn test_search_short_help() {
    skim_cmd()
        .args(["search", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim search"));
}

#[test]
fn test_search_stub_with_query() {
    skim_cmd()
        .args(["search", "test"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not yet implemented"));
}

#[test]
fn test_search_stub_without_args() {
    skim_cmd()
        .args(["search"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not yet implemented"));
}

#[test]
fn test_search_stub_exit_code() {
    let output = skim_cmd().args(["search", "test"]).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "non-help should exit with code 1"
    );
}

#[test]
fn test_search_help_contains_all_flags() {
    let assert = skim_cmd().args(["search", "--help"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let expected_flags = [
        "--build",
        "--rebuild",
        "--update",
        "--ast",
        "--blast-radius",
        "--limit",
        "--hot",
        "--cold",
        "--risky",
        "--help",
    ];
    for flag in &expected_flags {
        assert!(
            stdout.contains(flag),
            "help output missing flag: {flag}"
        );
    }
}

#[test]
fn test_search_help_contains_usage_line() {
    skim_cmd()
        .args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: skim search"));
}

#[test]
fn test_search_help_at_end() {
    // --help after positional arg still shows help
    skim_cmd()
        .args(["search", "test", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim search"));
}

#[test]
fn test_search_stub_with_flags() {
    skim_cmd()
        .args(["search", "--hot", "--limit", "10", "test"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not yet implemented"));
}

#[test]
fn test_search_stub_with_ast_flag() {
    skim_cmd()
        .args(["search", "--ast", "fn _()"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not yet implemented"));
}

#[test]
fn test_search_hint_message() {
    skim_cmd()
        .args(["search", "test"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Wave 1+"));
}

#[test]
fn test_search_empty_query() {
    skim_cmd()
        .args(["search", ""])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not yet implemented"));
}

#[test]
fn test_search_in_main_help() {
    skim_cmd()
        .args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("search"));
}

#[test]
fn test_search_completions_registered() {
    skim_cmd()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("search"));
}
