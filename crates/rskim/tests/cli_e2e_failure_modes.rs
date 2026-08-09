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
        .env("SKIM_DEBUG", "1")
        .args(["grep", "pat", "/nonexistent/skim-317-test"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("No such file"))
        .stderr(predicate::str::contains("raw output (not compressed)"))
        .stderr(predicate::str::contains("compressed output").not());
}

// ============================================================================
// grep: native path:line:content passthrough — every match emitted, line=match
// ============================================================================

/// Fix 3: grep emits native path:line:content (or lineno:content for single-file).
/// Line count must equal match count — no header/footer lines inflating the count.
#[test]
fn test_grep_single_file_attributed_and_complete() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("t.txt");
    let content: String = (1..=10).map(|i| format!("needle {i}\n")).collect();
    fs::write(&file, content).unwrap();

    // Native single-file grep output with -n is: `lineno:content` (no file prefix).
    // Every match must appear; no header/footer lines.
    let output = skim_cmd()
        .args(["grep", "-n", "needle", file.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "grep must exit 0; stderr: {stderr}"
    );

    // Fix 3: native passthrough — no grouped header or footer lines.
    assert!(
        !stdout.contains("grep 10"),
        "must not contain old grouped header; stdout: {stdout}"
    );
    assert!(
        !stdout.contains("1 file"),
        "must not contain old grouped footer; stdout: {stdout}"
    );

    // Single-file grep -n emits `lineno:content` with no file prefix.
    // (The previous `<stdin>` guard was vacuous post-Fix-3 — GrepArgs::fallback_label removed.)
    assert!(
        !stdout.contains("t.txt"),
        "single-file grep -n must not emit file prefix; stdout: {stdout}"
    );

    // testing-07: line count must equal match count — no header/footer inflating the count.
    let line_count = stdout.lines().count();
    assert_eq!(
        line_count, 10,
        "line count must equal match count (10 needles); got {line_count}\nstdout: {stdout}"
    );

    // testing-07: native lineno:content format — every output line starts with a line number.
    for line in stdout.lines() {
        assert!(
            line.chars().next().is_some_and(|c| c.is_ascii_digit()),
            "native grep -n: each line must start with a line number; offending line: {line:?}\nfull stdout: {stdout}"
        );
    }

    // Every match line must be present — no cap.
    for i in 1..=10 {
        assert!(
            stdout.contains(&format!("needle {i}")),
            "match needle {i} missing from stdout; stdout: {stdout}"
        );
    }
}

/// Fix 3: multi-file grep emits native `file:line:content` passthrough so that
/// downstream pipes (`head -N`, `wc -l`, `sed -n`) get one line per match.
/// No grouped header or footer — one output line per match.
#[test]
fn test_grep_small_multifile_emits_native_path_line_content() {
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
        // Fix 3: native path:line:content — no grouped header or footer.
        .stdout(predicate::str::contains("grep 2").not())
        .stdout(predicate::str::contains("2 files").not())
        // Both files and both matches must appear.
        .stdout(predicate::str::contains("a.txt"))
        .stdout(predicate::str::contains("b.txt"))
        .stdout(predicate::str::contains("alpha MARK one"))
        .stdout(predicate::str::contains("beta MARK two"))
        // Native format: `a.txt:1:alpha MARK one` (file:line:content, no indent).
        .stdout(predicate::str::contains("a.txt:1:alpha MARK one"));
}

// ============================================================================
// rg: native path:line:content passthrough (Fix 3 — rg half, PF-004 sibling)
// ============================================================================

/// Fix 3 (rg): rg emits native path:line:content passthrough so that downstream
/// pipes (`head -N`, `wc -l`, `sed -n`) get one line per match.
/// No grouped header or footer — one output line per match.
///
/// Gated on rg availability — skips gracefully when ripgrep is not installed.
#[test]
fn test_rg_small_multifile_emits_native_path_line_content() {
    if std::process::Command::new("rg")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!(
            "skipping test_rg_small_multifile_emits_native_path_line_content: rg not installed"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    fs::write(&a, "alpha MARK one\nplain\n").unwrap();
    fs::write(&b, "plain\nbeta MARK two\n").unwrap();

    skim_cmd()
        .args(["rg", "-n", "MARK", a.to_str().unwrap(), b.to_str().unwrap()])
        .assert()
        .code(0)
        // Fix 3: native path:line:content — no grouped header or footer.
        .stdout(predicate::str::contains("rg 2").not())
        .stdout(predicate::str::contains("2 files").not())
        // Both files and both matches must appear.
        .stdout(predicate::str::contains("a.txt"))
        .stdout(predicate::str::contains("b.txt"))
        .stdout(predicate::str::contains("alpha MARK one"))
        .stdout(predicate::str::contains("beta MARK two"))
        // regression-09: native path:line:content format assertion — a grouped or
        // JSON renderer could satisfy the above; this pins the exact format.
        .stdout(predicate::str::contains("a.txt:1:alpha MARK one"));
}

// ============================================================================
// Over-cap file with --max-lines: exit 0 and bounded output (B4)
// ============================================================================

/// B4: `skim file --mode=pseudo --max-lines N` on a Rust source that overflows
/// the AST node cap (MAX_AST_NODES = 100,000) must return exit 0 and at most
/// ~N lines — never an error exit code.
///
/// Library-level tests in rskim-core cover the transform result. This test pins
/// the CLI exit-disposition: the correct exit code and bounded stdout must survive
/// any future wiring change between the dispatcher's degrade-to-passthrough path
/// and the CLI layer.
#[test]
fn test_over_cap_rs_file_with_max_lines_exits_0_and_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("generated.rs");

    // Generate a Rust file that exceeds MAX_AST_NODES (100,000).
    // Strategy mirrors over_cap_python_source in rskim-core/src/types.rs:
    // ~40+ AST nodes per `let` statement × 4500 statements ≈ 180,000 > cap.
    let mut content = String::from("fn generated() {\n");
    for i in 0usize..4500 {
        content.push_str("    let _ = ");
        for j in 0..20usize {
            if j > 0 {
                content.push_str(" + ");
            }
            content.push_str(&(i * 20 + j).to_string());
        }
        content.push_str(";\n");
    }
    content.push_str("}\n");
    fs::write(&file, &content).unwrap();

    const MAX_LINES: usize = 40;
    let max_lines_str = MAX_LINES.to_string();
    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=pseudo")
        .arg("--max-lines")
        .arg(&max_lines_str)
        .arg("--no-cache")
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(0),
        "over-cap Rust file with --max-lines must exit 0 (degrade to passthrough); \
         got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout_str = String::from_utf8(output.stdout).unwrap();
    let line_count = stdout_str.lines().count();
    // Small slack: the windowed passthrough may emit slightly fewer or more lines
    // than exactly MAX_LINES depending on trailing newline handling. +2 is generous
    // but bounded — a 4500-line passthrough would fail this immediately.
    assert!(
        line_count <= MAX_LINES + 2,
        "stdout must be bounded to ~{MAX_LINES} lines after degrade, got {line_count} lines\n\
         first 5 lines:\n{}",
        stdout_str.lines().take(5).collect::<Vec<_>>().join("\n"),
    );
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
        .env("SKIM_DEBUG", "1")
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
        .env("SKIM_DEBUG", "1")
        .env("PATH", stub_path(dir.path()))
        .args(["eslint", "a.js"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("could not find config file"))
        .stderr(predicate::str::contains(
            "eslint exited 2; raw output (not compressed)",
        ));
}
