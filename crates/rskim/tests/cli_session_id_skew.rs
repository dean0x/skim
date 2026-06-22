//! Cluster B skew-simulation integration tests (#1.1 / spec §4).
//!
//! ## Background
//!
//! The hook no longer injects `--session-id` into rewritten commands.  However,
//! an OLDER hook binary talking to a NEWER skim binary (version-skew scenario)
//! might still inject the flag.  Without `strip_session_id_flag` at the dispatch
//! entry, the stray flag would be forwarded to the underlying tool (e.g. `git`,
//! `grep`) which would fail with "unrecognised option --session-id" — a hard
//! failure with NO output (exit 2 from git's perspective), silently destroying
//! the agent's output.
//!
//! `dispatch()` in `cmd/dispatch.rs` strips `--session-id` as its FIRST action,
//! before routing to any subcommand handler.  These tests simulate the skew
//! scenario on multiple subcommands and assert:
//!
//! - Exit code is NOT 2 (no "unrecognised argument" failure from the underlying tool).
//! - Output IS produced (not empty — the subcommand ran normally).
//! - The `--session-id` token does NOT appear in stdout (it was not forwarded).
//!
//! ## Subcommands tested
//!
//! `git`, `grep`, and `ls` — three representative families to prove
//! `strip_session_id_flag` at the dispatch entry covers all routing paths.

use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd.env("SKIM_DISABLE_ANALYTICS", "1");
    cmd
}

/// Build a tiny git repo with one commit so `git status` works.
///
/// Returns the temp dir (caller must keep alive) and the worker path.
fn make_tiny_git_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    for (prog, args) in [
        ("git", vec!["init"]),
        ("git", vec!["config", "user.email", "test@example.com"]),
        ("git", vec!["config", "user.name", "Test"]),
    ] {
        std::process::Command::new(prog)
            .args(&args)
            .current_dir(&repo)
            .output()
            .ok();
    }
    // Create a file and commit it so git status returns exit 0.
    fs::write(repo.join("file.txt"), "hello\n").unwrap();
    for (prog, args) in [
        ("git", vec!["add", "."]),
        ("git", vec!["commit", "-m", "init"]),
    ] {
        std::process::Command::new(prog)
            .args(&args)
            .current_dir(&repo)
            .output()
            .ok();
    }
    (dir, repo)
}

// ============================================================================
// git status with injected --session-id (equals form)
// ============================================================================

/// `skim git status --session-id=sess-test` must succeed and produce output.
///
/// Without `strip_session_id_flag`, git would receive `--session-id=sess-test`
/// and fail with exit 129 + "unknown option: --session-id".
#[test]
fn skew_git_status_session_id_stripped() {
    let (_dir, repo) = make_tiny_git_repo();

    skim_cmd()
        .args(["git", "status", "--session-id=sess-test"])
        .current_dir(&repo)
        .assert()
        // git status exits 0 on a clean repo.
        .code(0)
        // Must produce output — the stray flag must have been stripped.
        .stdout(predicate::str::is_empty().not())
        // --session-id must not leak to stdout.
        .stdout(predicate::str::contains("--session-id").not())
        // No "unrecognised option" from git.
        .stderr(predicate::str::contains("unknown option").not())
        .stderr(predicate::str::contains("unrecognised").not());
}

/// Space-separated form `--session-id sess-test` for git status.
#[test]
fn skew_git_status_session_id_space_form_stripped() {
    let (_dir, repo) = make_tiny_git_repo();

    skim_cmd()
        .args(["git", "status", "--session-id", "sess-test"])
        .current_dir(&repo)
        .assert()
        .code(0)
        .stdout(predicate::str::is_empty().not())
        .stdout(predicate::str::contains("--session-id").not())
        .stderr(predicate::str::contains("unknown option").not());
}

// ============================================================================
// grep with injected --session-id
// ============================================================================

/// `skim grep --session-id=sess-test <pattern> <file>` must find the pattern.
///
/// Without stripping, grep would receive `--session-id=sess-test` and exit 2
/// with "invalid option" — no output produced.
#[test]
#[cfg(unix)]
fn skew_grep_session_id_stripped() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.txt");
    fs::write(&file, "alpha\nbeta\ngamma\n").unwrap();

    skim_cmd()
        .args([
            "grep",
            "--session-id=sess-test",
            "alpha",
            file.to_str().unwrap(),
        ])
        .assert()
        // grep exits 0 when it finds a match.
        .code(0)
        // alpha must appear in output — grep ran normally.
        .stdout(predicate::str::contains("alpha"))
        // The flag must not have been forwarded to grep.
        .stdout(predicate::str::contains("--session-id").not())
        .stderr(predicate::str::contains("invalid option").not())
        .stderr(predicate::str::contains("unrecognised").not());
}

// ============================================================================
// ls with injected --session-id
// ============================================================================

/// `skim ls --session-id=sess-test <dir>` must list files normally.
///
/// Real `ls` ignores unknown flags on some platforms but errors on others.
/// Either way, skim must strip the flag before forwarding to ls.
#[test]
#[cfg(unix)]
fn skew_ls_session_id_stripped() {
    let dir = tempfile::tempdir().unwrap();
    // Create a file so ls has something to show.
    fs::write(dir.path().join("canary.txt"), "x").unwrap();

    skim_cmd()
        .args(["ls", "--session-id=sess-test", dir.path().to_str().unwrap()])
        .assert()
        // ls must exit 0 (not 2 from "unknown option").
        .code(predicate::ne(2))
        // canary.txt must appear in output.
        .stdout(predicate::str::contains("canary.txt"))
        // --session-id must not appear in output.
        .stdout(predicate::str::contains("--session-id").not());
}

// ============================================================================
// Verify exit code is NOT 2 (no clap "unexpected argument") on ALL three
// ============================================================================

/// Composite smoke test: run all three subcommands with an injected
/// `--session-id=sess-test` and assert exit code ≠ 2 for each.
///
/// Exit 2 would indicate clap or the underlying tool rejected the flag.
/// This test is the canonical "Cluster B acceptance criterion" assertion.
#[test]
#[cfg(unix)]
fn skew_all_subcommands_exit_code_not_2() {
    let git_dir = make_tiny_git_repo();

    // git status
    let status_code = skim_cmd()
        .args(["git", "status", "--session-id=sess-test"])
        .current_dir(&git_dir.1)
        .output()
        .unwrap()
        .status
        .code()
        .unwrap_or(1);
    assert_ne!(
        status_code, 2,
        "skim git status --session-id=sess-test must not exit 2 (no 'unknown option' failure)"
    );

    // grep — file with known content
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("x.txt");
    fs::write(&f, "hello\n").unwrap();
    let grep_code = skim_cmd()
        .args([
            "grep",
            "--session-id=sess-test",
            "hello",
            f.to_str().unwrap(),
        ])
        .output()
        .unwrap()
        .status
        .code()
        .unwrap_or(1);
    assert_ne!(
        grep_code, 2,
        "skim grep --session-id=sess-test must not exit 2 (no 'invalid option' failure)"
    );

    // ls
    let ls_code = skim_cmd()
        .args(["ls", "--session-id=sess-test", tmp.path().to_str().unwrap()])
        .output()
        .unwrap()
        .status
        .code()
        .unwrap_or(1);
    assert_ne!(
        ls_code, 2,
        "skim ls --session-id=sess-test must not exit 2 (no 'unknown option' failure)"
    );
}
