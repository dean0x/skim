//! Shared helpers for test parser Tier-2 fallback paths.
//!
//! Provides [`scrape_failures`] which extracts failing test entries from
//! plain-text runner output when JSON parsing is unavailable, and
//! [`try_read_stdin`] which combines the stdin guard (via
//! [`crate::cmd::should_read_stdin`]), chunked read, and whitespace-only check
//! into a single call.

use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::output::canonical::{TestEntry, TestOutcome};
use crate::runner::CommandOutput;

/// Identifies which test runner produced the text being scraped.
///
/// Each runner has a distinct output format for failed tests, so kind-sensitive
/// regex patterns are required to avoid false positives across runners.
///
/// Variants `Pytest` and `Go` are provided for completeness and future use.
/// Currently only `Cargo` and `Vitest` are consumed by Tier-2 regex paths;
/// `Go`'s Tier-2 already extracts test names directly and `Pytest` uses
/// passthrough for its Tier-2.
#[derive(Debug, Clone, Copy)]
pub(super) enum TestKind {
    /// `cargo test` plain-text format: `test <path> ... FAILED`
    Cargo,
    /// `pytest` plain-text format: `FAILED tests/test_foo.py::test_bar - ...`
    #[allow(dead_code)]
    Pytest,
    /// `go test` plain-text format: `--- FAIL: TestFoo (0.01s)`
    #[allow(dead_code)]
    Go,
    /// `vitest` / `jest` plain-text format: `✕ <describe> > <name>` or `✗ <name>`
    Vitest,
}

/// ANSI color-code strip pattern (ESC [ ... m sequences).
static RE_ANSI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").expect("valid ANSI regex"));

/// Per-kind failure patterns — compiled once.
static RE_CARGO_FAIL: LazyLock<Regex> = LazyLock::new(|| {
    // `test my_module::test_foo ... FAILED`
    Regex::new(r"^test\s+(\S+)\s+\.\.\.\s+FAILED").expect("valid cargo fail regex")
});

static RE_PYTEST_FAIL: LazyLock<Regex> = LazyLock::new(|| {
    // `FAILED tests/test_math.py::test_divide - ZeroDivisionError`
    Regex::new(r"^FAILED\s+(\S+)").expect("valid pytest fail regex")
});

static RE_GO_FAIL: LazyLock<Regex> = LazyLock::new(|| {
    // `--- FAIL: TestFoo (0.01s)`
    Regex::new(r"^--- FAIL:\s+(\S+)\s+\(").expect("valid go fail regex")
});

static RE_VITEST_FAIL: LazyLock<Regex> = LazyLock::new(|| {
    // AD-TEST-19 (2026-04-11): `^\s*` prefix added so real vitest output like
    // `   × divides by zero` matches. Without `\s*` the regex anchors at
    // column 0 and silently misses all indented failure lines. The fix was
    // surfaced by scrutinizer review (commit ea4e52f).
    //
    // `✕ describe > it name`, `✗ test name`, or `× test name` — with optional
    // leading whitespace because vitest and jest indent failing-test lines.
    // Example real output: `   × divides by zero`.
    Regex::new(r"^\s*[✕✗×]\s+(.+?)$").expect("valid vitest fail regex")
});

/// Try to read piped stdin, returning `Some(content)` only when there is
/// non-whitespace data to process.
///
/// Combines three steps that all test parsers previously duplicated:
/// 1. [`should_read_stdin`] guard — if false, return `Ok(None)` immediately.
/// 2. [`crate::cmd::read_stdin_bounded`] — propagate I/O errors via `?`.
/// 3. Whitespace check — `bytes().any(|b| !b.is_ascii_whitespace())` for
///    short-circuit on the first non-whitespace byte; returns `Ok(None)` when
///    the pipe is empty so callers fall through to the spawn path.
///
/// Returns `Ok(Some(content))` when there is content to parse, `Ok(None)` when
/// the guard is false or the pipe is empty/whitespace-only.
pub(super) fn try_read_stdin(args: &[String]) -> anyhow::Result<Option<String>> {
    if !crate::cmd::should_read_stdin(args) {
        return Ok(None);
    }
    let content = crate::cmd::read_stdin_bounded()?;
    if content.bytes().any(|b| !b.is_ascii_whitespace()) {
        Ok(Some(content))
    } else {
        Ok(None)
    }
}

/// Run the passthrough path for a test runner.
///
/// Handles the two sub-cases of SKIM_PASSTHROUGH mode for test runners:
/// 1. Piped stdin — print the raw content and return FAILURE (no exit code available).
/// 2. Spawn mode — run the command via `run_cmd` and forward the combined output.
///
/// `prepare_args` transforms the user args into the final argument list that
/// `run_cmd` receives (e.g., adding `--reporter=json` or `--tb=short`).
/// `run_cmd` receives the prepared args as `&[&str]` and returns a `CommandOutput`.
pub(super) fn run_passthrough(
    args: &[String],
    prepare_args: impl FnOnce(&[String]) -> Vec<String>,
    run_cmd: impl FnOnce(&[&str]) -> anyhow::Result<CommandOutput>,
) -> anyhow::Result<ExitCode> {
    if let Some(raw) = try_read_stdin(args)? {
        print!("{raw}");
        return Ok(ExitCode::FAILURE);
    }
    let final_args = prepare_args(args);
    let arg_refs: Vec<&str> = final_args.iter().map(String::as_str).collect();
    let output = run_cmd(&arg_refs)?;
    print!("{}", crate::cmd::combine_output(&output));
    let code = output.exit_code.unwrap_or(1).clamp(0, 255) as u8;
    Ok(ExitCode::from(code))
}

/// Extract failing test entries from plain-text runner output when JSON parsing
/// is unavailable (Tier 2 fallback).
///
/// # Design decision (Commit 2, 2026-04-11)
/// All four test handlers previously returned `vec![]` from their Tier-2 regex
/// paths, so LLMs saw `FAIL: 2` with zero failing-test names. Scraping names
/// additively preserves the name signal without inflating Tier-1 complexity.
/// Durations and messages stay `None` in Tier-2 — they would require parsing
/// the runner's full output format, which is precisely what Tier-1 JSON exists
/// to avoid.
///
/// Cap matches Tier-1's entry cap (100) to keep output size predictable
/// regardless of tier.
pub(super) fn scrape_failures(text: &str, kind: TestKind) -> Vec<TestEntry> {
    // Strip ANSI escape codes so color-enabled output (e.g. pytest --color=yes,
    // vitest with TTY detected) does not break pattern matching.
    //
    // Fast-path: when the caller has already stripped ANSI (no ESC bytes remain),
    // borrow the slice directly rather than running the regex over it a second time.
    // This eliminates the double-strip in `vitest::try_parse_regex`, which calls
    // `output::strip_ansi(raw)` → `cleaned` and then passes `cleaned` here.
    let cleaned: std::borrow::Cow<str> = if text.as_bytes().contains(&0x1b) {
        std::borrow::Cow::Owned(RE_ANSI.replace_all(text, "").into_owned())
    } else {
        std::borrow::Cow::Borrowed(text)
    };

    let re = match kind {
        TestKind::Cargo => &*RE_CARGO_FAIL,
        TestKind::Pytest => &*RE_PYTEST_FAIL,
        TestKind::Go => &*RE_GO_FAIL,
        TestKind::Vitest => &*RE_VITEST_FAIL,
    };

    let mut entries: Vec<TestEntry> = Vec::new();
    for line in cleaned.lines() {
        if entries.len() >= 100 {
            break;
        }
        if let Some(caps) = re.captures(line) {
            let name = caps[1].trim().to_string();
            if !name.is_empty() {
                entries.push(TestEntry {
                    name,
                    outcome: TestOutcome::Fail,
                    detail: None,
                });
            }
        }
    }

    entries
}

// ============================================================================
// Raw failure context helpers
// ============================================================================

/// Maximum number of raw output lines to append as failure context.
///
/// This gives the agent enough signal to understand why a test failed
/// without overwhelming the context window. Full output is always
/// available via `SKIM_PASSTHROUGH=1`.
pub(super) const MAX_FAILURE_CONTEXT_LINES: usize = 50;

/// Append raw failure context to stdout and emit a compressed-output hint to
/// stderr.
///
/// Called by test-runner handlers (vitest, go, …) when `summary.fail > 0` so
/// the agent can read the actual error messages without re-running with
/// `SKIM_PASSTHROUGH=1`.
///
/// # Performance
/// Applies [`last_n_lines`] first (zero-allocation `&str` slice) and then runs
/// [`crate::output::strip_ansi`] only on the ~50-line tail, limiting the ANSI
/// strip allocation to the tail rather than the full output string.
///
/// # Parameters
/// - `raw_output`: the full raw output string from the test runner.
/// - `exit_code`: the actual process exit code (e.g. `1` for test failures,
///   `2` for compilation errors in `go test`). Used in the stderr hint so the
///   caller knows the precise exit status to reproduce.
pub(super) fn emit_failure_context(raw_output: &str, exit_code: i32) {
    // Take the tail first (zero-allocation slice), then strip ANSI only on
    // those ~50 lines instead of the entire output buffer.
    let tail_raw = last_n_lines(raw_output, MAX_FAILURE_CONTEXT_LINES);
    let tail = crate::output::strip_ansi(tail_raw);
    if !tail.is_empty() {
        println!(
            "\n--- failure context (last {} lines) ---",
            tail.lines().count()
        );
        println!("{tail}");
    }
    eprintln!("[skim] compressed output (exit {exit_code}). SKIM_PASSTHROUGH=1 for full output.");
}

/// Return the last `n` lines of `text` as a `&str` slice.
///
/// Scans backward through the bytes looking for newline characters. When the
/// `n`-th newline from the end is found, returns everything after it. Falls
/// back to the full input when `text` has fewer than `n` newlines.
///
/// The returned slice borrows from `text` — no allocation.
pub(super) fn last_n_lines(text: &str, n: usize) -> &str {
    if n == 0 {
        return "";
    }
    let mut count = 0;
    for (i, byte) in text.as_bytes().iter().enumerate().rev() {
        if *byte == b'\n' {
            count += 1;
            if count == n {
                return &text[i + 1..];
            }
        }
    }
    // Fewer than `n` newlines → return the whole input
    text
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    // ========================================================================
    // run_passthrough tests (spawn branch)
    //
    // In Rust unit tests stdin is never piped (it's the test harness terminal),
    // so `try_read_stdin` always returns Ok(None) and `run_passthrough` always
    // takes the spawn branch. The stdin branch is covered by E2E tests in
    // cli_e2e_test_parsers (test_vitest_passthrough_* and
    // test_pytest_passthrough_*) which pipe stdin via `write_stdin`.
    // ========================================================================

    fn make_output(stdout: &str, stderr: &str, code: Option<i32>) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code: code,
            duration: Duration::ZERO,
        }
    }

    #[test]
    fn test_run_passthrough_spawn_exit_zero_returns_success() {
        let code = run_passthrough(
            &[],
            |a| a.to_vec(),
            |_| Ok(make_output("ok output\n", "", Some(0))),
        )
        .expect("run_passthrough should not error");
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn test_run_passthrough_spawn_exit_nonzero_preserves_code() {
        let code = run_passthrough(
            &[],
            |a| a.to_vec(),
            |_| Ok(make_output("fail output\n", "", Some(2))),
        )
        .expect("run_passthrough should not error");
        // exit code 2 → ExitCode::from(2)
        assert_eq!(code, ExitCode::from(2u8));
    }

    #[test]
    fn test_run_passthrough_spawn_exit_none_returns_failure() {
        // When the command is killed by a signal, exit_code is None.
        // run_passthrough falls back to exit code 1 (FAILURE).
        let code = run_passthrough(&[], |a| a.to_vec(), |_| Ok(make_output("", "", None)))
            .expect("run_passthrough should not error");
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn test_run_passthrough_spawn_prepare_args_is_called() {
        // Verify that prepare_args is invoked: inject a sentinel arg and assert
        // run_cmd receives it.
        let mut received_args: Vec<String> = Vec::new();
        let code = run_passthrough(
            &["base-arg".to_string()],
            |a| {
                let mut v = a.to_vec();
                v.push("--injected".to_string());
                v
            },
            |arg_refs| {
                received_args = arg_refs.iter().map(|s| s.to_string()).collect();
                Ok(make_output("", "", Some(0)))
            },
        )
        .expect("run_passthrough should not error");
        assert!(
            received_args.contains(&"--injected".to_string()),
            "prepare_args sentinel must reach run_cmd: {received_args:?}"
        );
        assert!(
            received_args.contains(&"base-arg".to_string()),
            "original args must be forwarded: {received_args:?}"
        );
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn test_run_passthrough_spawn_run_cmd_error_propagates() {
        let result = run_passthrough(
            &[],
            |a| a.to_vec(),
            |_| Err(anyhow::anyhow!("spawn failed: binary not found")),
        );
        assert!(result.is_err(), "error from run_cmd must propagate");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("spawn failed"),
            "error message should contain spawn context: {msg}"
        );
    }

    #[test]
    fn test_scrape_failures_cargo_basic() {
        let text = "test my_module::test_foo ... FAILED\n\
                    test other::test_bar ... ok\n";
        let entries = scrape_failures(text, TestKind::Cargo);
        assert!(
            !entries.is_empty(),
            "should find at least one failure: {entries:?}"
        );
        assert!(
            entries[0].name.contains("test_foo"),
            "first entry should be test_foo: {:?}",
            entries[0].name
        );
        assert_eq!(entries[0].outcome, TestOutcome::Fail);
    }

    #[test]
    fn test_scrape_failures_pytest_basic() {
        let text = "FAILED tests/test_math.py::test_divide - ZeroDivisionError\n\
                    FAILED tests/test_api.py::test_endpoint\n";
        let entries = scrape_failures(text, TestKind::Pytest);
        assert!(!entries.is_empty(), "should find failures: {entries:?}");
        assert!(
            entries[0].name.contains("test_divide"),
            "first entry: {:?}",
            entries[0].name
        );
    }

    #[test]
    fn test_scrape_failures_go_basic() {
        let text = "--- FAIL: TestFoo (0.01s)\n\
                    --- PASS: TestBar (0.00s)\n";
        let entries = scrape_failures(text, TestKind::Go);
        assert!(!entries.is_empty(), "should find TestFoo: {entries:?}");
        assert!(
            entries[0].name.contains("TestFoo"),
            "entry: {:?}",
            entries[0].name
        );
    }

    #[test]
    fn test_scrape_failures_vitest_basic() {
        let text = "✕ math > adds correctly\n\
                    ✓ math > multiplies\n";
        let entries = scrape_failures(text, TestKind::Vitest);
        assert!(
            !entries.is_empty(),
            "should find vitest failure: {entries:?}"
        );
        assert!(
            entries[0].name.contains("adds correctly"),
            "entry: {:?}",
            entries[0].name
        );
    }

    /// Regression: vitest indents failing-test lines with leading whitespace
    /// (e.g. `   × divides by zero`). The regex must tolerate optional
    /// leading whitespace so real vitest output matches, not just the
    /// hand-crafted unit fixture.
    #[test]
    fn test_scrape_failures_vitest_indented_failure_line() {
        let text = " ❯ src/utils.test.ts (3 tests | 1 failed)\n\
                     ✓ adds numbers\n\
                     ✓ subtracts numbers\n\
                     × divides by zero\n";
        let entries = scrape_failures(text, TestKind::Vitest);
        assert!(
            !entries.is_empty(),
            "indented vitest fail line must match: {entries:?}"
        );
        assert!(
            entries.iter().any(|e| e.name.contains("divides by zero")),
            "entries must contain 'divides by zero': {entries:?}"
        );
    }

    #[test]
    fn test_scrape_failures_ansi_stripped() {
        // Cargo output with ANSI color codes.
        let text = "\x1b[31mtest my_mod::test_colored ... FAILED\x1b[0m\n";
        let entries = scrape_failures(text, TestKind::Cargo);
        assert!(
            !entries.is_empty(),
            "ANSI-stripped output should still match: {entries:?}"
        );
        assert!(
            entries[0].name.contains("test_colored"),
            "name: {:?}",
            entries[0].name
        );
    }

    #[test]
    fn test_scrape_failures_cap_at_100() {
        // Build 200-failure fixture.
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!("test test_{i} ... FAILED\n"));
        }
        let entries = scrape_failures(&text, TestKind::Cargo);
        assert_eq!(
            entries.len(),
            100,
            "must be capped at 100: {}",
            entries.len()
        );
    }

    #[test]
    fn test_scrape_failures_no_matches_returns_empty() {
        let text = "test foo ... ok\ntest bar ... ok\n";
        let entries = scrape_failures(text, TestKind::Cargo);
        assert!(
            entries.is_empty(),
            "no failures should return empty: {entries:?}"
        );
    }

    // ========================================================================
    // last_n_lines tests
    // ========================================================================

    #[test]
    fn test_last_n_lines_fewer_than_n() {
        // 3 lines, n=50 → full input returned
        let text = "line1\nline2\nline3";
        assert_eq!(last_n_lines(text, 50), text);
    }

    #[test]
    fn test_last_n_lines_exact_n() {
        // 50 lines, n=50 → full input
        let text = (0..50)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = last_n_lines(&text, 50);
        assert_eq!(result, text);
    }

    #[test]
    fn test_last_n_lines_more_than_n() {
        // 100 lines (0..99), n=50 → last 50 lines
        let lines: Vec<String> = (0..100).map(|i| format!("line{i}")).collect();
        let text = lines.join("\n");
        let result = last_n_lines(&text, 50);
        // Last 50 lines are lines 50..99
        let expected = lines[50..].join("\n");
        assert_eq!(result, expected);
    }

    #[test]
    fn test_last_n_lines_empty() {
        assert_eq!(last_n_lines("", 50), "");
    }

    #[test]
    fn test_last_n_lines_no_newlines() {
        // Single line with no newlines → full input returned
        let text = "single line no newlines";
        assert_eq!(last_n_lines(text, 50), text);
    }

    #[test]
    fn test_last_n_lines_n_zero_returns_empty() {
        let text = "line1\nline2\nline3";
        assert_eq!(last_n_lines(text, 0), "");
    }

    #[test]
    fn test_last_n_lines_trailing_newline() {
        // Text ending with newline: "line1\nline2\n" — the trailing newline means
        // the last "line" is empty. last_n_lines(text, 1) returns everything
        // after the first-from-the-end newline, which is "".
        let text = "line1\nline2\n";
        let result = last_n_lines(text, 1);
        assert_eq!(result, "");
    }

    #[test]
    fn test_last_n_lines_windows_line_endings() {
        // \r\n — only \n is counted as a newline delimiter; \r is data.
        // "line1\r\nline2\r\nline3" has 2 \n chars.
        let text = "line1\r\nline2\r\nline3";
        // n=2 → find 2nd \n from end, which is after "line1\r", return "line2\r\nline3"
        let result = last_n_lines(text, 2);
        assert_eq!(result, "line2\r\nline3");
    }
}
