//! Shared test helpers for subcommand parser unit tests.
//!
//! Centralises `make_output`, `make_output_full`, and `load_fixture` so that
//! the ~34 local `make_output` definitions and ~41 local `load_fixture`
//! definitions across the `cmd` subtree are replaced by a single canonical
//! source. This eliminates drift between test helpers and ensures all tests
//! construct `CommandOutput` values consistently (e.g., `Duration::ZERO`
//! rather than arbitrary millisecond values).

use crate::runner::CommandOutput;
use std::time::Duration;

/// Build a `CommandOutput` from stdout only.
///
/// Sets `stderr` to empty, `exit_code` to `Some(0)`, and
/// `duration` to `Duration::ZERO`. Use this for the common
/// successful-output case.
pub(crate) fn make_output(stdout: &str) -> CommandOutput {
    CommandOutput {
        stdout: stdout.to_string(),
        stderr: String::new(),
        exit_code: Some(0),
        duration: Duration::ZERO,
    }
}

/// Build a `CommandOutput` with explicit stdout, stderr, and exit code.
///
/// Use when the test needs to exercise non-zero exits, stderr content,
/// or absent exit codes (`None`).
pub(crate) fn make_output_full(
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> CommandOutput {
    CommandOutput {
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        exit_code,
        duration: Duration::ZERO,
    }
}

/// Build a `CommandOutput` where all output is on stderr and exit code is 0.
///
/// Use for tools that write to stderr by default (e.g. `wget`, `curl`).
/// Equivalent to `make_output_full("", stderr, Some(0))` but clarifies
/// the intent at the call site.
pub(crate) fn make_output_stderr(stderr: &str) -> CommandOutput {
    CommandOutput {
        stdout: String::new(),
        stderr: stderr.to_string(),
        exit_code: Some(0),
        duration: Duration::ZERO,
    }
}

/// Load a test fixture from `tests/fixtures/cmd/{subdir}/{name}`.
///
/// Panics with a clear message if the fixture file cannot be read,
/// so test failures surface the missing-file path immediately.
pub(crate) fn load_fixture(subdir: &str, name: &str) -> String {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures/cmd");
    path.push(subdir);
    path.push(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
}
