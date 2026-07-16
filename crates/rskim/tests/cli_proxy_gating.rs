//! E2E integration tests for `skim proxy` feature-gate (#352).
//!
//! These tests verify the behavior of the `proxy` subcommand when it is
//! compiled OUT of the default build (i.e., `--features proxy` is NOT set).
//!
//! In default builds:
//! - `skim proxy` must fail with exit code 1 and a clear actionable error on stderr.
//! - A file named `proxy` in the cwd must NOT be silently skimmed (the routing guard
//!   beats the file-op path — prevents the "silent-skim footgun").
//! - Shell completions must NOT list `proxy` as a completion candidate.
//!
//! These tests are only meaningful in default-features builds. They are gated at
//! the file level so that `cargo test --all-features` skips this entire file, and
//! the proxy subcommand is exercised in the all-features suite instead.
//!
//! ## Coupling note
//!
//! This test file is tightly coupled to the routing guard in
//! `crates/rskim/src/main.rs` (the `#[cfg(not(feature = "proxy"))]` guard that
//! intercepts the bare positional "proxy") and to `KNOWN_SUBCOMMANDS` /
//! `META_SUBCOMMANDS` in `registry.rs`. If the guard or registry entries change,
//! these tests are the first line of defense.
#![cfg(not(feature = "proxy"))]

use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

mod common;

// ============================================================================
// Helper
// ============================================================================

fn skim_cmd() -> assert_cmd::Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

// ============================================================================
// Test: bare `skim proxy` → exit 1 + actionable error on stderr
//
// Discriminating: asserts BOTH exit code and stderr content (PF-007).
// A test that only asserts exit-0 or exit-nonzero asserts nothing useful.
// ============================================================================

/// Bare `skim proxy` in a default-features build must exit 1 and print an
/// actionable error on stderr explaining how to enable the feature.
///
/// PF-007: assert stderr content, not just exit code.
#[test]
fn cli_proxy_gating_bare_proxy_exits_1_with_actionable_error() {
    let tmp = TempDir::new().expect("tempdir");
    skim_cmd()
        .arg("proxy")
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "'proxy' requires a build with --features proxy",
        ));
}

// ============================================================================
// Test: `skim proxy` with a file named `proxy` present → same friendly error
//
// Discriminating: the routing guard must beat the file-op fallthrough path so
// that `proxy` is never silently skimmed as a file.
// ============================================================================

/// When a file named `proxy` exists in the current directory, `skim proxy`
/// must still print the feature-gate error rather than silently reading the
/// file. The routing guard in main.rs must fire before the file-op path.
///
/// PF-007: assert stderr content, not just exit code.
#[test]
fn cli_proxy_gating_with_proxy_file_still_errors() {
    let tmp = TempDir::new().expect("tempdir");
    // Create a file named `proxy` in the temp cwd so the file-op path would
    // normally succeed — the guard must intercept first.
    fs::write(tmp.path().join("proxy"), b"fn proxy() {}\n").expect("write proxy file");

    skim_cmd()
        .arg("proxy")
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "'proxy' requires a build with --features proxy",
        ));
}

// ============================================================================
// Test: `skim completions bash` does NOT list `proxy`
//
// Discriminating: asserts content, not just exit-0 (PF-007).
// Shell completions iterate KNOWN_SUBCOMMANDS; when proxy is gated off it must
// not appear in the completion candidates.
// ============================================================================

/// Shell completions in a default-features build must not offer `proxy` as
/// a candidate — it is gated behind `--features proxy`.
///
/// PF-007: assert completion output content.
#[test]
fn cli_proxy_gating_completions_bash_no_proxy_entry() {
    let tmp = TempDir::new().expect("tempdir");
    let output = skim_cmd()
        .args(["completions", "bash"])
        .current_dir(tmp.path())
        .output()
        .expect("skim completions bash must run");

    assert!(output.status.success(), "completions bash must exit 0");

    let stdout = String::from_utf8(output.stdout).expect("completions stdout must be UTF-8");
    assert!(
        !stdout.contains("proxy"),
        "completions bash must not list 'proxy' in a default-features build; \
         it appeared in:\n{stdout}"
    );
}
