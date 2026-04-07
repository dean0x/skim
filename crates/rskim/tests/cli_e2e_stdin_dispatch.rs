//! E2E tests for stdin dispatch logic across all subcommands.
//!
//! Verifies subcommands correctly handle non-terminal stdin with no data
//! (the agent/CI environment scenario). With args present, subcommands must
//! execute the underlying tool, NOT read from empty stdin.

use std::process::Command;

fn skim_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_skim"))
}

#[test]
fn vitest_with_args_does_not_read_stdin() {
    let output = skim_bin()
        .args(["test", "vitest", "--help"])
        .output()
        .expect("failed to spawn skim");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("output passed through without parsing"),
        "vitest with args should run command, not read empty stdin.\n{combined}"
    );
}

#[test]
fn pytest_with_args_does_not_read_stdin() {
    let output = skim_bin()
        .args(["test", "pytest", "--help"])
        .output()
        .expect("failed to spawn skim");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("output passed through without parsing"),
        "pytest with args should run command, not read empty stdin.\n{combined}"
    );
}

#[test]
fn cargo_test_with_args_does_not_read_stdin() {
    let output = skim_bin()
        .args(["test", "cargo", "--", "--help"])
        .output()
        .expect("failed to spawn skim");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("output passed through without parsing"),
        "cargo test with args should run command, not read empty stdin"
    );
}

#[test]
fn lint_with_args_does_not_read_stdin() {
    let output = skim_bin()
        .args(["lint", "eslint", "--help"])
        .output()
        .expect("failed to spawn skim");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("output passed through without parsing"),
        "lint with args should run command, not read empty stdin"
    );
}

#[test]
fn pkg_with_args_does_not_read_stdin() {
    let output = skim_bin()
        .args(["pkg", "npm", "ls", "--help"])
        .output()
        .expect("failed to spawn skim");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("output passed through without parsing"),
        "pkg with args should run command, not read empty stdin"
    );
}

#[test]
fn infra_with_args_does_not_read_stdin() {
    let output = skim_bin()
        .args(["infra", "curl", "--help"])
        .output()
        .expect("failed to spawn skim");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("output passed through without parsing"),
        "infra with args should run command, not read empty stdin"
    );
}

#[test]
fn file_with_args_does_not_read_stdin() {
    let output = skim_bin()
        .args(["file", "find", "--help"])
        .output()
        .expect("failed to spawn skim");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("output passed through without parsing"),
        "file with args should run command, not read empty stdin"
    );
}

#[test]
fn vitest_no_args_reads_piped_stdin() {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = skim_bin()
        .args(["test", "vitest"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn skim");

    let stdin = child.stdin.as_mut().unwrap();
    stdin.write_all(b"fake test output for passthrough").unwrap();
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("failed to wait");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("fake test output for passthrough"),
        "vitest with no args should read and passthrough stdin.\nstdout: {stdout}"
    );
}

#[test]
fn pytest_no_args_reads_piped_stdin() {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = skim_bin()
        .args(["test", "pytest"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn skim");

    let stdin = child.stdin.as_mut().unwrap();
    stdin.write_all(b"fake pytest output for passthrough").unwrap();
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("failed to wait");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("fake pytest output for passthrough"),
        "pytest with no args should read and passthrough stdin.\nstdout: {stdout}"
    );
}
