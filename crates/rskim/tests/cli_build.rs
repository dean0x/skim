//! Integration tests for `skim build` subcommand (#51).

use assert_cmd::Command;
use predicates::prelude::*;

// ============================================================================
// Help and dispatch
// ============================================================================

#[test]
fn test_skim_build_help() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("build")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build"))
        .stdout(predicate::str::contains("cargo"))
        .stdout(predicate::str::contains("tsc"));
}

#[test]
fn test_skim_build_short_help() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("build")
        .arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build"));
}

#[test]
fn test_skim_build_no_tool_shows_error() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("build")
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing required argument"));
}

#[test]
fn test_skim_build_unknown_tool_shows_error() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("build")
        .arg("webpack")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown build tool"));
}

// ============================================================================
// Cargo build integration (real execution)
// ============================================================================

/// Run a real `cargo build` on this repository.
///
/// Since we are running inside the skim repo which is already built,
/// this should succeed quickly with cached artifacts.
#[test]
fn test_skim_build_cargo_in_repo() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("build")
        .arg("cargo")
        .assert()
        .success()
        .stdout(predicate::str::contains("BUILD OK"));
}

// ============================================================================
// Cargo build dispatches through parser
// ============================================================================

#[test]
fn test_skim_build_cargo_stub_dispatches() {
    // Running `skim build cargo` should NOT show "not yet implemented"
    Command::cargo_bin("skim")
        .unwrap()
        .arg("build")
        .arg("cargo")
        .assert()
        .stdout(predicate::str::contains("not yet implemented").not())
        .stderr(predicate::str::contains("not yet implemented").not());
}
