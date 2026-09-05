//! Integration tests for `skim cargo test` subcommand (#46).
//!
//! v2.8.0: Flat dispatch — `skim cargo test` replaces `skim test cargo`.
//!
//! Tests the end-to-end cargo test parser via the CLI binary.

use predicates::prelude::*;
mod common;

// ============================================================================
// Real cargo test execution
// ============================================================================

/// Run `skim cargo test` against a trivial zero-dependency temp crate.
///
/// # Why a temp crate (not `-p rskim-core` on this repo)
///
/// The previous version of this test ran `skim cargo test -p rskim-core`
/// against skim's own workspace. That approach shared `target/` with the outer
/// test run, relinked 10 test binaries after any rskim-core edit, ran 16
/// doctests, and hit the 120 s timeout twice during a machine-wide stall on
/// 2026-09-02 (measured 4.8–6.1 s warm locally, ~26 s on CI). A temp crate
/// decouples from `target/` state and machine load — same precedent used by
/// `test_build_cargo_success_exit_code` in `cli_e2e_build_parsers.rs` (issue
/// #447). The crate compiles in ~1–2 s on a cold runner.
///
/// Workspace isolation: `common::trivial_cargo_project()` places the directory
/// under the system temp root, outside the skim workspace tree. Its
/// `Cargo.toml` also includes an explicit `[workspace]` table that severs any
/// upward workspace walk cargo might attempt.
///
/// # What this still uniquely proves (four properties)
///
/// 1. Real cargo accepts `--message-format=json` injected by `build_cargo_args`
///    + `inject_flag_before_separator` (`crates/rskim/src/cmd/test/cargo.rs`).
/// 2. `RE_CARGO_SUMMARY` (tier-2 regex) matches genuine stable-toolchain
///    libtest summary output (`test result: ok. 1 passed; 0 failed; 0 ignored;
///    …`), rendering `pass: 1 fail: 0 skip: 0` on stdout.
/// 3. Exit-code handling on a real passing `cargo test` (`expected_exit_codes =
///    &[101]` path) plus the ADR-001 net-savings guard against a real baseline.
/// 4. `should_read_stdin` declines stdin when args are present so cargo is
///    actually spawned (no stdin path taken).
#[test]
fn test_skim_test_cargo_real_cargo_trivial_crate() {
    let dir = common::trivial_cargo_project();
    common::skim()
        .args(["cargo", "test"])
        .current_dir(dir.path())
        .env_remove("SKIM_PASSTHROUGH")
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 1"));
}

// ============================================================================
// Help text
// ============================================================================

#[test]
fn test_skim_cargo_help() {
    // v2.8.0: `skim cargo --help` — "test" is no longer a subcommand.
    common::skim()
        .args(["cargo", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo"));
}

// ============================================================================
// Unknown runner
// ============================================================================

// v2.8.0: "test" is no longer a subcommand. Unknown tool names are unknown
// subcommands at the dispatch level. This test is removed.

// ============================================================================
// Piped stdin parsing
// ============================================================================

#[test]
fn test_skim_test_cargo_stdin_json() {
    // Pipe cargo JSON output via stdin
    let json_input = r#"{"type":"suite","event":"started","test_count":2}
{"type":"test","event":"ok","name":"test_a","exec_time":0.001}
{"type":"test","event":"ok","name":"test_b","exec_time":0.002}
{"type":"suite","event":"ok","passed":2,"failed":0,"ignored":0,"measured":0,"filtered_out":0,"exec_time":0.003}
"#;

    common::skim()
        .args(["cargo", "test"])
        // Remove SKIM_PASSTHROUGH so compression is not bypassed inside the child process.
        .env_remove("SKIM_PASSTHROUGH")
        .write_stdin(json_input)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 2"))
        .stdout(predicate::str::contains("fail: 0"));
}

#[test]
fn test_skim_test_cargo_stdin_plain_text() {
    // Pipe plain text cargo test output (tier 2 regex)
    let text_input = "running 5 tests\n\
        test test_one ... ok\n\
        test test_two ... ok\n\
        test test_three ... ok\n\
        test test_four ... ok\n\
        test test_five ... ok\n\n\
        test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";

    common::skim()
        .args(["cargo", "test"])
        // Remove SKIM_PASSTHROUGH so compression is not bypassed inside the child process.
        .env_remove("SKIM_PASSTHROUGH")
        .write_stdin(text_input)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 5"));
}

// ============================================================================
// `skim cargo nextest` — dispatch correctness (issue: missing coverage)
// ============================================================================

/// `skim cargo nextest` must not produce an "unknown subcommand" error.
///
/// The nextest dispatch path keeps the "nextest" token in args (unlike the "test"
/// arm which strips it), routing through a distinct code path in `dispatch_cargo`.
///
/// Design note: `skim cargo nextest` always spawns the real cargo binary because
/// `should_read_stdin` returns false when args are non-empty (the "nextest" token
/// is passed through as a runner arg). Piped stdin is therefore not available for
/// this subcommand. The help text confirms nextest is listed as supported.
#[test]
fn test_skim_cargo_nextest_is_listed_in_help_as_supported() {
    common::skim()
        .args(["cargo", "--help"])
        .assert()
        .success()
        // nextest must appear in the help text as a supported subcommand
        .stdout(predicate::str::contains("nextest"))
        // must not have an unknown-subcommand error
        .stderr(predicate::str::contains("unknown subcommand").not());
}

/// Piping nextest-style output to `skim cargo test` (without the nextest token)
/// exercises the passthrough tier because `is_nextest=false` (no "nextest" in args).
///
/// NOTE: This is the nearest proxy for "pipe nextest output and get output".
/// The nextest format does not match the JSON or `test result:` regex tiers,
/// so it passes through unchanged.  The test verifies the dispatch succeeds
/// and the content is forwarded rather than producing an error.
#[test]
fn test_skim_cargo_nextest_output_piped_via_test_arm_passes_through() {
    let nextest_pass = include_str!("fixtures/cmd/test/cargo_nextest_pass.txt");

    common::skim()
        .args(["cargo", "test"])
        .write_stdin(nextest_pass)
        .assert()
        .success()
        // nextest content is passed through (passthrough tier — PASS token present)
        .stdout(predicate::str::contains("PASS"));
}

// ============================================================================
// `skim cargo t` and `skim cargo b` short aliases (issue: zero coverage)
// ============================================================================

/// `skim cargo t` is an alias for `skim cargo test`.
/// Pipe a minimal JSON test fixture and verify the alias dispatches correctly
/// and produces compressed output (pass count present).
#[test]
fn test_skim_cargo_t_alias_stdin_json() {
    let json_input = r#"{"type":"suite","event":"started","test_count":1}
{"type":"test","event":"ok","name":"alias_test","exec_time":0.001}
{"type":"suite","event":"ok","passed":1,"failed":0,"ignored":0,"measured":0,"filtered_out":0,"exec_time":0.001}
"#;

    common::skim()
        .args(["cargo", "t"])
        // Remove SKIM_PASSTHROUGH so compression is not bypassed inside the child process.
        .env_remove("SKIM_PASSTHROUGH")
        .write_stdin(json_input)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 1"))
        .stdout(predicate::str::contains("fail: 0"));
}

/// `skim cargo b` is an alias for `skim cargo build`.
///
/// Build commands always spawn the real executable (no stdin path), so the
/// alias is verified by running real `cargo build` on this repo.
/// Since the repo is already built, incremental compilation is fast.
#[test]
fn test_skim_cargo_b_alias_dispatches_to_build() {
    common::skim()
        .args(["cargo", "b"])
        // Must not produce an error about unknown subcommand
        .assert()
        .success()
        .stderr(predicate::str::contains("unknown subcommand").not())
        // Must also not show "missing subcommand" (the alias was recognised)
        .stderr(predicate::str::contains("missing subcommand").not());
}

// ============================================================================
// Unknown subcommand — error path coverage (D2 passthrough semantics)
// ============================================================================

// D2: unknown subcommands in these dispatchers are forwarded to the native
// binary via run_raw_passthrough instead of returning a skim-generated "unknown
// subcommand" error. These tests verify:
//   (a) skim exits non-zero (the stub exits 1)
//   (b) the stub's sentinel appears in stdout — proves skim forwarded to the
//       tool, not that the test merely passed because the tool binary was absent
//   (c) skim's own debug-gated banner is absent from stderr in default mode
//       (ADR-011 lossless path; real messages are e.g. "skim cargo: unknown
//       subcommand '{x}' — passing through", never the bare "skim: unknown
//       subcommand" literal the old assertions checked)
//
// Stub-based (unix-only): a shell script stands in for the real tool so the
// test distinguishes "skim forwarded" from "skim failed to spawn missing binary".

/// D2 stub test: `skim cargo unknownthing` forwards to cargo, exits non-zero.
/// The stub exits 1 with a sentinel — proves forwarding, not spawn-failure.
#[cfg(unix)]
#[test]
fn test_skim_cargo_unknown_subcommand_errors() {
    let stub_dir = tempfile::tempdir().unwrap();
    common::make_stub(stub_dir.path(), "cargo", "STUB-CARGO-SENTINEL\n", "", 1);
    let path = common::stub_path(stub_dir.path());
    common::skim()
        .env("PATH", &path)
        .args(["cargo", "unknownthing"])
        .assert()
        .failure()
        // Stub ran — skim forwarded to cargo via run_raw_passthrough.
        .stdout(predicate::str::contains("STUB-CARGO-SENTINEL"))
        // D2: debug-gated banner never reaches stderr in default mode.
        .stderr(predicate::str::contains("skim cargo: unknown subcommand").not());
}

/// D2 stub test: `skim go unknownthing` forwards to go, exits non-zero.
#[cfg(unix)]
#[test]
fn test_skim_go_unknown_subcommand_errors() {
    let stub_dir = tempfile::tempdir().unwrap();
    common::make_stub(stub_dir.path(), "go", "STUB-GO-SENTINEL\n", "", 1);
    let path = common::stub_path(stub_dir.path());
    common::skim()
        .env("PATH", &path)
        .args(["go", "unknownthing"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("STUB-GO-SENTINEL"))
        .stderr(predicate::str::contains("skim go: unknown subcommand").not());
}

/// D2 stub test: `skim npm unknownthing` forwards to npm, exits non-zero.
#[cfg(unix)]
#[test]
fn test_skim_npm_unknown_subcommand_errors() {
    let stub_dir = tempfile::tempdir().unwrap();
    common::make_stub(stub_dir.path(), "npm", "STUB-NPM-SENTINEL\n", "", 1);
    let path = common::stub_path(stub_dir.path());
    common::skim()
        .env("PATH", &path)
        .args(["npm", "unknownthing"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("STUB-NPM-SENTINEL"))
        .stderr(predicate::str::contains("skim npm: unknown subcommand").not());
}

/// D2 stub test: `skim pnpm unknownthing` forwards to pnpm, exits non-zero.
#[cfg(unix)]
#[test]
fn test_skim_pnpm_unknown_subcommand_errors() {
    let stub_dir = tempfile::tempdir().unwrap();
    common::make_stub(stub_dir.path(), "pnpm", "STUB-PNPM-SENTINEL\n", "", 1);
    let path = common::stub_path(stub_dir.path());
    common::skim()
        .env("PATH", &path)
        .args(["pnpm", "unknownthing"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("STUB-PNPM-SENTINEL"))
        .stderr(predicate::str::contains("skim pnpm: unknown subcommand").not());
}

/// D2 stub test: `skim pip unknownthing` forwards to pip, exits non-zero.
#[cfg(unix)]
#[test]
fn test_skim_pip_unknown_subcommand_errors() {
    let stub_dir = tempfile::tempdir().unwrap();
    common::make_stub(stub_dir.path(), "pip", "STUB-PIP-SENTINEL\n", "", 1);
    let path = common::stub_path(stub_dir.path());
    common::skim()
        .env("PATH", &path)
        .args(["pip", "unknownthing"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("STUB-PIP-SENTINEL"))
        .stderr(predicate::str::contains("skim pip: unknown subcommand").not());
}
