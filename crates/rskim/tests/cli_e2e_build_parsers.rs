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

// ============================================================================
// Cargo build: real execution
// ============================================================================

#[test]
fn test_build_cargo_success_exit_code() {
    // Runs `skim cargo build` against a trivial zero-dependency crate so the
    // test is not cache-state-dependent. Verifies: skim spawns the real cargo,
    // the exit code propagates, and the parsed summary is rendered.
    // See: https://github.com/dean0x/skim/issues/447
    let dir = common::trivial_cargo_project();
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
    let dir = common::trivial_cargo_project();
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

/// D2: unknown cargo subcommands are passed through to cargo itself via
/// run_raw_passthrough. Cargo exits non-zero and emits its own error message
/// ("no such command") rather than skim's old "unknown subcommand" message.
#[test]
fn test_cargo_unknown_subcmd_exit_code() {
    skim_cmd()
        .args(["cargo", "webpack"])
        .assert()
        .failure()
        // D2: cargo's own error surfaces; "no such command" or "unknown" covers
        // both older and newer cargo versions.
        .stderr(
            predicate::str::contains("no such command").or(predicate::str::contains("unknown")),
        );
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

/// Is `make` runnable here?  Fails the test outright when it is missing from an
/// environment that is supposed to have it.
///
/// A bare `return` in a `#[test]` is recorded as **PASS**, so a tool-presence
/// guard is a silent-pass branch: the case disappears from the suite and the
/// report still reads green.  That is tolerable on a developer box without
/// `make`, and not tolerable in CI, where every runner image skim's workflows
/// use ships `make` — there, an absence is an environment fault to surface, not
/// a portability case to absorb.
///
/// `CI` is the standard marker set by GitHub Actions (and by every other runner
/// skim is exercised on), so it is what decides which of the two this is.
fn make_is_available() -> bool {
    if StdCommand::new("make").arg("--version").output().is_ok() {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "`make` is not installed, but CI is set — the runner image is expected \
         to provide it.  Skipping here would record this case as a silent PASS."
    );
    eprintln!("skipping: make not installed (local run; set CI=1 to make this fatal)");
    false
}

#[test]
fn test_build_make_dispatches_through_build_module() {
    // `skim make --help` is intercepted before spawning the real `make` binary,
    // so this case does not itself need `make` on a developer box.  The guard
    // below documents that intent and protects against future changes that might
    // remove the --help short-circuit; it is strict under CI for the reason on
    // `make_is_available`, where `make` is part of the image.
    if !make_is_available() {
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
    if !make_is_available() {
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

// ============================================================================
// ADR-011 class-1 disclosure: the diagnostic-summary marker
// ============================================================================

/// The marker fires IF AND ONLY IF the compressed summary is what reached the
/// reader.
///
/// Deliberately written as a biconditional. Which branch the net-savings guard
/// takes is a SIZE verdict, and a test that pinned one branch could be made to
/// pass by resizing the payload (PF-027's fixture-resize route) rather than by
/// the gate being correct. Here stdout says which branch ran and stderr must
/// agree with it, so no payload size can make the assertion pass falsely.
///
/// On the `Keep` branch the reader loses each diagnostic's body — tsc's related
/// spans and following context — and the marker discloses it. On the
/// `Passthrough` branch the child's own bytes reach the reader intact, so a
/// marker there would claim a loss that did not occur.
#[cfg(unix)]
#[test]
fn test_diagnostics_marker_fires_exactly_when_the_summary_is_served() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let tsc_stderr = concat!(
        "src/index.ts(10,5): error TS2304: Cannot find name 'foo'.\n",
        "src/api/client.ts(22,11): error TS2345: Argument of type 'string' is not \
         assignable to parameter of type 'number'.\n",
        "src/api/client.ts(48,3): error TS2551: Property 'requset' does not exist on \
         type 'Client'. Did you mean 'request'?\n",
    );
    common::make_stub(dir.path(), "tsc", "", tsc_stderr, 2);

    let out = skim_cmd()
        .env("PATH", common::stub_path(dir.path()))
        .args(["tsc", "--noEmit"])
        .output()
        .expect("skim runs");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    let summary_served = stdout.contains("FAILED warnings: 0 errors: 3");
    let marker_emitted = stderr.contains("diagnostics summarised");

    assert_eq!(
        summary_served, marker_emitted,
        "class-1 disclosure must be emitted exactly when the summary replaces \
         raw.\nstdout: {stdout:?}\nstderr: {stderr:?}"
    );

    if marker_emitted {
        let marker = stderr
            .lines()
            .find(|line| line.contains("diagnostics summarised"))
            .expect("marker_emitted is true, so the line exists");

        assert!(
            marker.contains("3 diagnostics"),
            "marker must carry the exact count: {marker:?}"
        );

        // A class-1 marker must carry a remedy that is LITERALLY REACHABLE from
        // the invocation printing it (ADR-011). For the build family that is not
        // `SKIM_PASSTHROUGH=1`: `handler_visible_args` strips the leading
        // subcommand token, so off a TTY `should_read_stdin` is true, the
        // passthrough gate declines, and `cmd::build::run_parsed_command` has no
        // passthrough branch of its own — the hatch is a measured no-op there
        // (PF-039). `diagnostics_summary_marker` therefore passes
        // `passthrough_reproduces_argv: false` unconditionally and
        // `fidelity::remedy_for` returns the one remedy that is true.
        assert!(
            marker.contains("run 'tsc' directly for the full output"),
            "class-1 marker must carry a REACHABLE remedy — for the build family \
             that is running the tool itself, not the passthrough hatch: {marker:?}"
        );
        assert!(
            !marker.contains("SKIM_PASSTHROUGH=1"),
            "the passthrough hatch is a no-op for the build family off a TTY \
             (PF-039); advertising it is the defect this remedy replaced: \
             {marker:?}"
        );
    }
}

/// Gate (1): a build with nothing to report drops no diagnostic bodies, so the
/// summary line IS the whole truth and no marker is emitted — even on the
/// `Keep` branch, where something WAS compressed away.
#[cfg(unix)]
#[test]
fn test_zero_diagnostics_emits_no_marker_on_the_keep_branch() {
    let dir = TempDir::new().expect("failed to create temp dir");
    // Same shape as `test_build_make_real_execution_success`: build-step noise
    // the make parser strips, and no diagnostics at all.
    let noisy = concat!(
        "gcc -O2 -c src/foo.c -o build/foo.o\n",
        "gcc -O2 -c src/bar.c -o build/bar.o\n",
        "gcc -O2 -c src/baz.c -o build/baz.o\n",
        "gcc -O2 -c src/qux.c -o build/qux.o\n",
        "gcc -O2 -c src/quux.c -o build/quux.o\n",
        "gcc build/foo.o build/bar.o build/baz.o build/qux.o build/quux.o -o myapp\n",
        "Build complete.\n",
    );
    common::make_stub(dir.path(), "make", noisy, "", 0);

    let out = skim_cmd()
        .env("PATH", common::stub_path(dir.path()))
        .args(["make", "all"])
        .output()
        .expect("skim runs");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The invariant under test — true on either branch when the count is zero.
    assert!(
        !stderr.contains("diagnostics summarised"),
        "no diagnostics means no diagnostic bodies were dropped: {stderr:?}"
    );
    // Coverage check, not the invariant: if this fails the payload no longer
    // reaches the `Keep` branch and the test needs a different one — it does not
    // mean the marker gate regressed.
    assert!(
        stdout.contains("OK warnings: 0 errors: 0"),
        "payload no longer exercises the Keep branch; pick another: {stdout:?}"
    );
}
