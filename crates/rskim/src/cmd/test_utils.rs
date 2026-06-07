//! Shared test helpers for subcommand parser unit tests.
//!
//! Provides canonical `make_output`, `make_output_full`, `make_output_stderr`,
//! and `load_fixture` helpers so that subcommand parsers (e.g. `log`, `eslint`,
//! `mypy`) construct `CommandOutput` values consistently — `Duration::ZERO`
//! rather than arbitrary durations, and a single fixture-loading path rather
//! than per-module duplicates.  New subcommand parsers should import from here
//! rather than defining local equivalents.

use std::time::Duration;

use crate::runner::CommandOutput;

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
/// Both `subdir` and `name` must be single path components — no `/`, `\`,
/// or `..` are allowed. This prevents directory traversal from reaching
/// files outside the fixtures tree. Panics with a clear message if the
/// arguments are invalid or if the fixture file cannot be read, so test
/// failures surface the problem immediately.
pub(crate) fn load_fixture(subdir: &str, name: &str) -> String {
    assert!(
        !subdir.contains(['/', '\\'])
            && subdir != ".."
            && !name.contains(['/', '\\'])
            && name != "..",
        "load_fixture: subdir/name must be a single path component, got subdir={subdir:?} name={name:?}"
    );
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures/cmd");
    path.push(subdir);
    path.push(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
}
