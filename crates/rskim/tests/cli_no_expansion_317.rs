//! Behavioral no-expansion integration tests (#317 — reports 7.1 and 7.2).
//!
//! ## #317 Invariant
//!
//! "compress, never truncate — and never expand" (CLAUDE.md / PR #317).
//!
//! skim's net-savings guard (`savings_decision` in `cmd/execution.rs`) ensures
//! that compressed output is NEVER emitted when it is larger (in tokens/bytes)
//! than the raw tool output.  When the guard fires, skim falls back to the raw
//! output verbatim.
//!
//! ## Reported regressions
//!
//! **Report 7.1 (ls)**: `skim ls -la <dir>` expanded output relative to raw
//!   `ls -la <dir>`.  The net-savings guard should have prevented this.
//!
//! **Report 7.2 (wc)**: `skim wc -c` on a tiny/empty input expanded output
//!   relative to raw `wc -c`.
//!
//! ## What these tests assert
//!
//! For each reported command:
//!   - Run skim (the binary under test) and capture stdout length.
//!   - Run the underlying tool directly and capture its stdout length.
//!   - Assert: `skim_len <= raw_len` (never expand, #317 invariant).
//!
//! "Never expand" is the hard invariant — skim may be equal (passthrough) or
//! strictly shorter (compression), but never longer than raw.

use std::fs;

use assert_cmd::Command;
mod common;

fn skim_cmd() -> Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

// ============================================================================
// Report 7.1 — `skim ls -la <dir>` must not expand relative to raw `ls -la`
// ============================================================================

/// `skim ls -la <tiny_dir>` stdout must be ≤ raw `ls -la <tiny_dir>` stdout.
///
/// This converts the unit-level guard logic into proof on the real reported
/// regression (report 7.1).  A failure here means the net-savings guard is
/// not firing on the `ls` command path, allowing skim to emit a larger output
/// than the raw tool.
#[test]
#[cfg(unix)]
fn no_expansion_ls_la_tiny_dir() {
    let dir = tempfile::tempdir().unwrap();
    // Populate a tiny directory — a few files so ls has something to format.
    for name in &["alpha.txt", "beta.txt", "gamma.txt"] {
        fs::write(dir.path().join(name), "x").unwrap();
    }
    let dir_path = dir.path().to_str().unwrap();

    // Run skim ls -la <dir>
    let skim_output = skim_cmd()
        .args(["ls", "-la", dir_path])
        .output()
        .expect("skim ls must not fail to spawn");

    // Run raw ls -la <dir>
    let raw_output = std::process::Command::new("ls")
        .args(["-la", dir_path])
        .output()
        .expect("ls must be available on Unix");

    let skim_len = skim_output.stdout.len();
    let raw_len = raw_output.stdout.len();

    // #317 invariant: skim must NEVER emit MORE bytes than raw.
    assert!(
        skim_len <= raw_len,
        "report 7.1: skim ls -la expanded output\n  \
         raw={raw_len}B  skim={skim_len}B\n  \
         skim stdout={:?}\n  \
         raw stdout={:?}\n  \
         This means the net-savings guard failed to fire on the ls path.",
        String::from_utf8_lossy(&skim_output.stdout),
        String::from_utf8_lossy(&raw_output.stdout)
    );
}

/// `skim ls <dir>` (without -la) also must not expand.
///
/// Tests the basic `ls` compression path in addition to the `-la` variant.
#[test]
#[cfg(unix)]
fn no_expansion_ls_plain_tiny_dir() {
    let dir = tempfile::tempdir().unwrap();
    for name in &["one.txt", "two.txt"] {
        fs::write(dir.path().join(name), "x").unwrap();
    }
    let dir_path = dir.path().to_str().unwrap();

    let skim_output = skim_cmd()
        .args(["ls", dir_path])
        .output()
        .expect("skim ls must spawn");

    let raw_output = std::process::Command::new("ls")
        .arg(dir_path)
        .output()
        .expect("ls must be available");

    let skim_len = skim_output.stdout.len();
    let raw_len = raw_output.stdout.len();

    assert!(
        skim_len <= raw_len,
        "report 7.1 (plain ls): skim expanded output\n  \
         raw={raw_len}B  skim={skim_len}B",
    );
}

// ============================================================================
// Report 7.2 — `skim wc -c` must not expand relative to raw `wc -c`
// ============================================================================

/// `skim wc -c` on a tiny/empty input must not expand relative to raw `wc -c`.
///
/// This converts the unit-level guard logic into proof on the real reported
/// regression (report 7.2).
///
/// We use a tiny file (`"hello\n"`) passed as an argument so both skim and raw
/// wc process the same input without depending on stdin piping in tests.
#[test]
#[cfg(unix)]
fn no_expansion_wc_c_tiny_input() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("tiny.txt");
    fs::write(&file, "hello\n").unwrap();
    let file_path = file.to_str().unwrap();

    // Run skim wc -c <file>
    let skim_output = skim_cmd()
        .args(["wc", "-c", file_path])
        .output()
        .expect("skim wc must not fail to spawn");

    // Run raw wc -c <file>
    let raw_output = std::process::Command::new("wc")
        .args(["-c", file_path])
        .output()
        .expect("wc must be available on Unix");

    let skim_len = skim_output.stdout.len();
    let raw_len = raw_output.stdout.len();

    // #317 invariant: never expand.
    assert!(
        skim_len <= raw_len,
        "report 7.2: skim wc -c expanded output\n  \
         raw={raw_len}B  skim={skim_len}B\n  \
         skim stdout={:?}\n  \
         raw stdout={:?}\n  \
         This means the net-savings guard failed to fire on the wc path.",
        String::from_utf8_lossy(&skim_output.stdout),
        String::from_utf8_lossy(&raw_output.stdout)
    );
}

/// `skim wc -c` on an empty file (report 7.2, edge case).
///
/// wc -c on empty file emits "0 <filename>" (7 bytes or so).
/// skim must not expand this.
#[test]
#[cfg(unix)]
fn no_expansion_wc_c_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("empty.txt");
    fs::write(&file, "").unwrap();
    let file_path = file.to_str().unwrap();

    let skim_output = skim_cmd()
        .args(["wc", "-c", file_path])
        .output()
        .expect("skim wc must spawn");

    let raw_output = std::process::Command::new("wc")
        .args(["-c", file_path])
        .output()
        .expect("wc must be available");

    let skim_len = skim_output.stdout.len();
    let raw_len = raw_output.stdout.len();

    assert!(
        skim_len <= raw_len,
        "report 7.2 (empty file): skim wc -c expanded output\n  \
         raw={raw_len}B  skim={skim_len}B",
    );
}

// ============================================================================
// Extra: wc -l (report 7.2 variant — line count)
// ============================================================================

/// `skim wc -l` on a tiny input must not expand relative to raw `wc -l`.
#[test]
#[cfg(unix)]
fn no_expansion_wc_l_tiny_input() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lines.txt");
    fs::write(&file, "line1\nline2\nline3\n").unwrap();
    let file_path = file.to_str().unwrap();

    let skim_output = skim_cmd()
        .args(["wc", "-l", file_path])
        .output()
        .expect("skim wc must spawn");

    let raw_output = std::process::Command::new("wc")
        .args(["-l", file_path])
        .output()
        .expect("wc must be available");

    let skim_len = skim_output.stdout.len();
    let raw_len = raw_output.stdout.len();

    assert!(
        skim_len <= raw_len,
        "report 7.2 (wc -l): skim expanded output\n  \
         raw={raw_len}B  skim={skim_len}B",
    );
}
