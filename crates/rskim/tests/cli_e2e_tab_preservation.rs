//! E2E tests proving that tab characters survive the ANSI-strip step for `gh`
//! and `diff` wrappers, and that `gh` exit-8 is treated as a parseable result
//! rather than an unexpected failure.
//!
//! ## Why stubs drive the REAL pipeline
//!
//! Unit tests call `parse_impl` / `try_parse_checks_text` directly, bypassing
//! the `run_tool` path where ANSI-stripping occurs.  These e2e tests inject a
//! stub binary on PATH so the full `run_tool` pipeline fires:
//!
//!   stub (emits fixture + chosen exit code)
//!     → run_tool → [skip_ansi_strip gate] → parse → render
//!
//! Per PF-004, this exercises the **hook / direct-invocation surface**
//! (`skim <tool> [args]`).  Flags arrive as ordinary argv and the stub
//! controls stdout/exit so fixture content feeds the live strip→parse path.
//!
//! ## Regression property
//!
//! - Without `skip_ansi_strip: true` on gh's CONFIG, tabs are stripped and
//!   `RE_GH_CHECK_TAB` never matches → passthrough (no "N checks" in output).
//! - Without `expected_exit_codes: &[8]` on gh's CONFIG, exit 8 is classified
//!   as UnexpectedFailure → raw-forwarded before parsing (no "N checks").
//! - Without `skip_ansi_strip: true` on diff's CONFIG, the `--- path\t<mtime>`
//!   header loses its tab → path and mtime are fused in the parsed entry.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

const GH_PR_CHECKS_TEXT: &str = include_str!("fixtures/cmd/infra/gh_pr_checks_text.txt");
const DIFF_UNIFIED_TEXT: &str = include_str!("fixtures/cmd/file/diff_unified.txt");

/// Create a stub directory with a script named `name` that prints `stdout`
/// and exits with `code`.  Returns the TempDir (must stay alive for PATH use).
fn make_stub(name: &str, stdout: &str, code: i32) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join(format!("{name}.out"));
    fs::write(&out_path, stdout).unwrap();
    let script = format!("#!/bin/sh\ncat '{}'\nexit {code}\n", out_path.display());
    let script_path = dir.path().join(name);
    fs::write(&script_path, script).unwrap();
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

/// Build a PATH string with `extra_dir` prepended before the system PATH.
fn prepend_path(extra_dir: &std::path::Path) -> String {
    format!(
        "{}:{}",
        extra_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

// ============================================================================
// gh pr checks — exit 0
// ============================================================================

/// Stub gh emits the tab-separated pr-checks fixture (exit 0).
///
/// With `skip_ansi_strip: true` on gh's CONFIG, tabs survive and
/// `RE_GH_CHECK_TAB` matches → parser produces a compressed summary.
/// Without the flag, tabs would be stripped and the output would raw-forward.
///
/// Asserts: exit 0, stdout contains "5 checks" and the check name "CI / build"
/// (proving fields were correctly separated, not fused).
#[test]
fn test_gh_pr_checks_exit0_compressed_summary() {
    let stub_dir = make_stub("gh", GH_PR_CHECKS_TEXT, 0);
    let path = prepend_path(stub_dir.path());
    let skim = common::skim_bin();

    let out = std::process::Command::new(&skim)
        .args(["gh", "pr", "checks", "421"])
        .env("PATH", &path)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .output()
        .expect("skim gh pr checks must be spawnable");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "exit 0 must propagate; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("5 checks"),
        "must contain '5 checks' summary — tabs must survive ANSI-strip; got: {stdout}"
    );
    assert!(
        stdout.contains("CI / build"),
        "check name 'CI / build' must appear as a separated field; got: {stdout}"
    );
}

// ============================================================================
// gh pr checks — exit 8 (pending/failing checks)
// ============================================================================

/// Stub gh emits the tab-separated pr-checks fixture and exits 8.
///
/// gh exits 8 when any check is pending or failing.  With
/// `expected_exit_codes: &[8]`, this is classified as ExpectedFailure (not
/// UnexpectedFailure) and the output is compressed before forwarding.  Exit 8
/// is then propagated so callers see the true status.
///
/// Without `&[8]`, exit 8 would be UnexpectedFailure → raw-forward BEFORE
/// parsing → no "N checks" summary in stdout.
///
/// Asserts: exit 8, stdout contains "5 checks", the failing check name
/// "CI / lint", and the failure URL "https://" (AD-INFRA-15).
#[test]
fn test_gh_pr_checks_exit8_summary_and_exit_code() {
    let stub_dir = make_stub("gh", GH_PR_CHECKS_TEXT, 8);
    let path = prepend_path(stub_dir.path());
    let skim = common::skim_bin();

    let out = std::process::Command::new(&skim)
        .args(["gh", "pr", "checks", "421"])
        .env("PATH", &path)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .output()
        .expect("skim gh pr checks (exit 8) must be spawnable");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(8),
        "exit 8 must be propagated; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("5 checks"),
        "must contain '5 checks' summary even on exit 8 — exit 8 is a parseable result; got: {stdout}"
    );
    assert!(
        stdout.contains("CI / lint"),
        "failing check name 'CI / lint' must appear; got: {stdout}"
    );
    assert!(
        stdout.contains("https://"),
        "AD-INFRA-15: URL must appear for failing check; got: {stdout}"
    );
}

// ============================================================================
// diff — tab-header path must not be glued to mtime
// ============================================================================

/// Stub diff emits a unified diff with `--- a/src/main.rs\t<mtime>` headers
/// and exits 1 (files differ — the normal, expected exit code).
///
/// With `skip_ansi_strip: true` on diff's CONFIG, the `\t` in the header
/// line survives and `try_parse_standalone_unified` splits on it, keeping
/// path and timestamp separate.  Without the flag, the tab is dropped and
/// the path is fused with the mtime.
///
/// Asserts: exit 1, the entry contains "src/main.rs" as part of a clean
/// path reference, and does NOT contain the glued form "src/main.rs2026".
#[test]
fn test_diff_tab_header_path_not_glued() {
    let stub_dir = make_stub("diff", DIFF_UNIFIED_TEXT, 1);
    let path = prepend_path(stub_dir.path());
    let skim = common::skim_bin();

    let out = std::process::Command::new(&skim)
        .args(["diff", "a/src/main.rs", "b/src/main.rs"])
        .env("PATH", &path)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .output()
        .expect("skim diff must be spawnable");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "exit 1 must propagate (files differ); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("src/main.rs"),
        "parsed entry must contain file path 'src/main.rs'; got: {stdout}"
    );
    assert!(
        !stdout.contains("src/main.rs2026"),
        "path must NOT be glued to mtime — tab must survive ANSI-strip; got: {stdout}"
    );
}
