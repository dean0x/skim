//! E2E tests for build parsers (flat dispatch).
//!
//! v2.8.0: `skim build cargo` → `skim cargo build`
//!
//! Tests the cargo/clippy/make dispatch CLI behavior.
//!
//! NOTE: Build parsers do NOT support stdin piping — they always execute the
//! real build command. These tests verify real build execution behavior and
//! exit code semantics. TSC tests are skipped because `tsc` may not be
//! installed in the test environment.

use assert_cmd::Command;
use predicates::prelude::*;
use std::process::Command as StdCommand;
use tempfile::TempDir;
mod common;

fn skim_cmd() -> Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

/// Creates a TempDir containing a zero-dependency Cargo project used to
/// exercise the cargo/clippy wrappers without triggering a cold compile of
/// the entire skim workspace.
///
/// The crate compiles in ~1–2 s even on a cold runner, keeping the 120 s
/// timeout a comfortable bound rather than a latency time-bomb.
///
/// `[workspace]` is included even though /tmp is outside the skim workspace
/// tree — it makes workspace severance explicit and guards against any future
/// cargo heuristic that might walk upward.
///
/// Mirrors the `test_build_make_real_execution_success` TempDir pattern.
fn trivial_cargo_project() -> TempDir {
    let dir = TempDir::new().expect("failed to create temp dir");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"skim_e2e_probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"skim_e2e_probe\"\npath = \"main.rs\"\n\n[workspace]\n",
    )
    .expect("failed to write Cargo.toml");
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").expect("failed to write main.rs");
    dir
}

// ============================================================================
// Cargo build: real execution
// ============================================================================

#[test]
fn test_build_cargo_success_exit_code() {
    // Runs `skim cargo build` against a trivial zero-dependency crate so the
    // test is not cache-state-dependent. Verifies: skim spawns the real cargo,
    // the exit code propagates, and the parsed summary is rendered.
    // See: https://github.com/dean0x/skim/issues/447
    let dir = trivial_cargo_project();
    skim_cmd()
        .args(["cargo", "build"])
        .current_dir(dir.path())
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success()
        .stdout(predicate::str::contains("OK warnings:"));
}

// ============================================================================
// Clippy: real execution
// ============================================================================

#[test]
fn test_build_clippy_success_exit_code() {
    // Runs `skim cargo clippy` against a trivial zero-dependency crate so the
    // test is not cache-state-dependent. The trivial crate is warning-free and
    // outside the workspace, so no workspace lint denials apply.
    // See: https://github.com/dean0x/skim/issues/447
    let dir = trivial_cargo_project();
    skim_cmd()
        .args(["cargo", "clippy"])
        .current_dir(dir.path())
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
}

// ============================================================================
// Build error handling
// ============================================================================

#[test]
fn test_cargo_unknown_subcmd_exit_code() {
    skim_cmd()
        .args(["cargo", "webpack"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown subcommand"));
}

#[test]
fn test_cargo_no_subcmd_shows_help() {
    skim_cmd()
        .arg("cargo")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo"));
}

// ============================================================================
// Make: dispatch + help
// ============================================================================

#[test]
fn test_build_make_dispatches_through_build_module() {
    // `skim make --help` is intercepted before spawning the real `make` binary,
    // so this test is portable even on systems without `make` installed.
    // The guard below documents that intent and protects against future changes
    // that might remove the --help short-circuit.
    if StdCommand::new("make").arg("--version").output().is_err() {
        eprintln!("skipping: make not installed");
        return;
    }
    skim_cmd().args(["make", "--help"]).assert().success();
}

#[test]
fn test_build_make_real_execution_success() {
    // Verify that `skim make <target>` actually invokes the make parser — not
    // just the --help short-circuit in build::run. The cargo equivalent
    // (test_build_cargo_success_exit_code) executes a real build; this test
    // mirrors that pattern for make.
    //
    // The Makefile emits enough build-like output that the make parser's
    // compressed "OK warnings: 0 errors: 0" is strictly smaller (fewer tokens)
    // than the raw output, so the net-savings guard keeps the compressed form.
    // This preserves the "build handler summarises success" test intent.
    if StdCommand::new("make").arg("--version").output().is_err() {
        eprintln!("skipping: make not installed");
        return;
    }

    let dir = TempDir::new().expect("failed to create temp dir");
    // Emit multi-line build output so the compressed summary is strictly smaller
    // than the raw lines. The make parser classifies lines like "gcc …" or
    // "Compiling …" as build steps and strips them; the guard then keeps the
    // compressed "OK warnings: 0 errors: 0" form.
    let makefile = concat!(
        "all:\n",
        "\t@echo 'gcc -O2 -c src/foo.c -o build/foo.o'\n",
        "\t@echo 'gcc -O2 -c src/bar.c -o build/bar.o'\n",
        "\t@echo 'gcc -O2 -c src/baz.c -o build/baz.o'\n",
        "\t@echo 'gcc -O2 -c src/qux.c -o build/qux.o'\n",
        "\t@echo 'gcc -O2 -c src/quux.c -o build/quux.o'\n",
        "\t@echo 'gcc build/foo.o build/bar.o build/baz.o build/qux.o build/quux.o -o myapp'\n",
        "\t@echo 'Build complete.'\n",
    );
    std::fs::write(dir.path().join("Makefile"), makefile).expect("failed to write Makefile");

    skim_cmd()
        .args(["make", "all"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("OK warnings:"));
}
