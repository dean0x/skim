//! Integration tests for build tool dispatch (flat dispatch).
//!
//! v2.8.0: `skim build cargo` → `skim cargo build`

use assert_cmd::Command;
use predicates::prelude::*;
mod common;

// ============================================================================
// Help and dispatch — cargo
// ============================================================================

#[test]
fn test_skim_cargo_help() {
    common::skim()
        .arg("cargo")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo"))
        .stdout(predicate::str::contains("build"))
        .stdout(predicate::str::contains("clippy"));
}

#[test]
fn test_skim_cargo_short_help() {
    common::skim()
        .arg("cargo")
        .arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo"));
}

#[test]
fn test_skim_cargo_no_subcmd_shows_help() {
    common::skim()
        .arg("cargo")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo"));
}

#[test]
fn test_skim_cargo_unknown_subcmd_shows_error() {
    common::skim()
        .arg("cargo")
        .arg("webpack")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown subcommand"));
}

// ============================================================================
// Cargo build integration (real execution)
// ============================================================================

/// Run a real `cargo build` on this repository.
///
/// Since we are running inside the skim repo which is already built,
/// this should succeed quickly with cached artifacts.
#[test]
fn test_skim_cargo_build_in_repo() {
    common::skim()
        .arg("cargo")
        .arg("build")
        .assert()
        .success()
        .stdout(predicate::str::contains("OK warnings:"));
}

// ============================================================================
// Cargo build dispatches through parser
// ============================================================================

#[test]
fn test_skim_cargo_build_dispatches() {
    // Running `skim cargo build` should NOT show "not yet implemented"
    common::skim()
        .arg("cargo")
        .arg("build")
        .assert()
        .stdout(predicate::str::contains("not yet implemented").not())
        .stderr(predicate::str::contains("not yet implemented").not());
}
