//! E2E failure-transparency verification (#317).
//!
//! Skim's contract: compress, never truncate — and never compress output the
//! parser was not designed for. These tests pin the exit-disposition matrix:
//!
//! - expected non-zero exit + Passthrough tier → silent (raw-tool parity)
//! - expected non-zero exit + Full/Degraded tier → "compressed output" notice
//! - unexpected non-zero exit / signal → raw stdout+stderr, "raw output" notice
//! - `forward_stderr` tools surface child stderr even on success
//!
//! Stub tools (shell scripts on a prepended PATH) give deterministic
//! stdout/stderr/exit without depending on real infra binaries.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
mod common;

fn skim_cmd() -> Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

/// Create a stub tool script that prints fixed stdout/stderr and exits `code`.
///
/// The payloads are written to sidecar files and `cat`-ed by the script, so no
/// shell escaping of the content is needed.
///
/// Unix-only: the script uses `#!/bin/sh` and the executable bit requires
/// `std::os::unix::fs::PermissionsExt`.
#[cfg(unix)]
fn make_stub(dir: &Path, name: &str, stdout: &str, stderr: &str, code: i32) {
    let out_path = dir.join(format!("{name}.out"));
    let err_path = dir.join(format!("{name}.err"));
    fs::write(&out_path, stdout).unwrap();
    fs::write(&err_path, stderr).unwrap();
    let script = format!(
        "#!/bin/sh\ncat '{}'\ncat '{}' >&2\nexit {code}\n",
        out_path.display(),
        err_path.display()
    );
    let script_path = dir.join(name);
    fs::write(&script_path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// PATH with the stub dir prepended so skim's spawned child resolves to it.
///
/// Unix-only: uses `:` as the PATH separator.
#[cfg(unix)]
fn stub_path(dir: &Path) -> String {
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

// ============================================================================
// grep: expected exit 1 (no matches) — raw-grep parity
// ============================================================================

#[test]
fn test_grep_no_match_exits_1_silently() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("t.txt");
    fs::write(&file, "alpha\nbeta\n").unwrap();

    skim_cmd()
        .args(["grep", "zzz", file.to_str().unwrap()])
        .assert()
        .code(1)
        .stdout(predicate::str::is_empty())
        // Raw grep is silent on no-match; skim must not add a notice.
        .stderr(predicate::str::is_empty());
}

// ============================================================================
// grep: unexpected exit 2 (real error) — raw forward with full diagnostics
// ============================================================================

#[test]
fn test_grep_missing_file_forwards_error_raw() {
    skim_cmd()
        .args(["grep", "pat", "/nonexistent/skim-317-test"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("No such file"))
        .stderr(predicate::str::contains("raw output (not compressed)"))
        .stderr(predicate::str::contains("compressed output").not());
}

// ============================================================================
// grep: single-file attribution + every match emitted
// ============================================================================

#[test]
fn test_grep_single_file_attributed_and_complete() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("t.txt");
    let content: String = (1..=10).map(|i| format!("needle {i}\n")).collect();
    fs::write(&file, content).unwrap();

    // grep now groups-by-file ALWAYS (skip_net_savings_guard), so the output is
    // deterministic regardless of match volume: canonical `grep N` header, the
    // attributed file path, every match, no `<stdin>` mislabel, no truncation.
    let mut assert = skim_cmd()
        .args(["grep", "-n", "needle", file.to_str().unwrap()])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("<stdin>").not())
        .stdout(predicate::str::contains("showing").not())
        // Deterministic grouped header (was a volume-dependent flip before #issues-4/5).
        .stdout(predicate::str::contains("grep 10"));
    // Every match line must be present — no per-file cap.
    for i in 1..=10 {
        assert = assert.stdout(predicate::str::contains(format!("needle {i}")));
    }
}

/// Issues #4/#5: a SMALL multi-file grep must use the SAME grouped shape as a
/// large one. Before the fix the net-savings guard flipped small result sets
/// back to raw `file:line:content`, so the same `grep -n` produced two different
/// formats depending on match volume. Now grep groups consistently.
#[test]
fn test_grep_small_multifile_groups_consistently() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    fs::write(&a, "alpha MARK one\nplain\n").unwrap();
    fs::write(&b, "plain\nbeta MARK two\n").unwrap();

    skim_cmd()
        .args([
            "grep",
            "-n",
            "MARK",
            a.to_str().unwrap(),
            b.to_str().unwrap(),
        ])
        .assert()
        .code(0)
        // Canonical grouped header + footer (grouped even though only 2 matches).
        .stdout(predicate::str::contains("grep 2"))
        .stdout(predicate::str::contains("2 files"))
        // Both files appear as group headers and both matches are present.
        .stdout(predicate::str::contains("a.txt"))
        .stdout(predicate::str::contains("b.txt"))
        .stdout(predicate::str::contains("alpha MARK one"))
        .stdout(predicate::str::contains("beta MARK two"))
        // Grouped form uses indented `:line:` entries, not raw `file:line:content`.
        .stdout(predicate::str::contains(":1: alpha MARK one"));
}

// ============================================================================
// Unexpected failure on an infra tool — raw stdout+stderr, child exit code
// ============================================================================

#[cfg(unix)]
#[test]
fn test_kubectl_unexpected_failure_raw_forwards_everything() {
    let dir = tempfile::tempdir().unwrap();
    make_stub(
        dir.path(),
        "kubectl",
        "NAME  READY\npod-a 0/1\n",
        "error: connection refused (cluster unreachable)\n",
        1,
    );

    skim_cmd()
        .env("PATH", stub_path(dir.path()))
        .args(["kubectl", "get", "pods"])
        .assert()
        .code(1)
        // stdout forwarded verbatim, not re-encoded
        .stdout(predicate::str::contains("NAME  READY"))
        .stdout(predicate::str::contains("pod-a 0/1"))
        // child stderr diagnostic survives
        .stderr(predicate::str::contains("connection refused"))
        .stderr(predicate::str::contains(
            "kubectl exited 1; raw output (not compressed)",
        ))
        .stderr(predicate::str::contains("compressed output").not());
}

// ============================================================================
// forward_stderr: db tool success with warnings — stderr surfaces
// ============================================================================

#[cfg(unix)]
#[test]
fn test_psql_success_with_stderr_warning_forwarded() {
    let dir = tempfile::tempdir().unwrap();
    make_stub(
        dir.path(),
        "psql",
        "id\tname\n1\talice\n2\tbob\n(2 rows)\n",
        "WARNING: terminal is not fully functional\n",
        0,
    );

    skim_cmd()
        .env("PATH", stub_path(dir.path()))
        .args(["psql", "-c", "SELECT 1"])
        .assert()
        .code(0)
        .stderr(predicate::str::contains(
            "WARNING: terminal is not fully functional",
        ));
}

// ============================================================================
// Expected failure with Full tier — escape-hatch notice still fires
// ============================================================================

#[cfg(unix)]
#[test]
fn test_eslint_expected_failure_full_tier_keeps_notice() {
    let dir = tempfile::tempdir().unwrap();
    make_stub(
        dir.path(),
        "eslint",
        r#"[{"filePath":"/tmp/a.js","messages":[{"ruleId":"semi","severity":2,"message":"Missing semicolon.","line":1,"column":10}],"errorCount":1,"warningCount":0}]"#,
        "",
        1,
    );

    skim_cmd()
        .env("PATH", stub_path(dir.path()))
        .args(["eslint", "a.js"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("semi"))
        .stdout(predicate::str::contains("1 errors"))
        .stderr(predicate::str::contains(
            "compressed output (exit 1). SKIM_PASSTHROUGH=1",
        ));
}

// ============================================================================
// Signal-kill classification sanity: unexpected exit code ≠ in expected list
// ============================================================================

#[cfg(unix)]
#[test]
fn test_lint_unexpected_exit_code_goes_raw() {
    // eslint expects exit 1; exit 2 (config error) must raw-forward.
    let dir = tempfile::tempdir().unwrap();
    make_stub(
        dir.path(),
        "eslint",
        "",
        "Oops! Something went wrong: could not find config file\n",
        2,
    );

    skim_cmd()
        .env("PATH", stub_path(dir.path()))
        .args(["eslint", "a.js"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("could not find config file"))
        .stderr(predicate::str::contains(
            "eslint exited 2; raw output (not compressed)",
        ));
}
