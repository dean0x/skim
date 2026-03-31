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
