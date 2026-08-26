//! Integration tests for build tool dispatch (flat dispatch).
//!
//! v2.8.0: `skim build cargo` → `skim cargo build`

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

/// D2: unknown cargo subcommands are now passed through to cargo itself via
/// run_raw_passthrough. Cargo exits non-zero and emits its own "no such command"
/// error — skim no longer wraps it in a custom "unknown subcommand" message.
#[test]
fn test_skim_cargo_unknown_subcmd_exits_nonzero() {
    common::skim()
        .arg("cargo")
        .arg("webpack")
        .assert()
        .failure()
        // D2: skim passes through to cargo; cargo's own error surfaces here.
        .stderr(
            predicate::str::contains("no such command").or(predicate::str::contains("unknown")),
        );
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
