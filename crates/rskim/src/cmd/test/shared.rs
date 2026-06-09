//! Shared helpers for test parser Tier-2 fallback paths.
//!
//! Provides [`scrape_failures`] which extracts failing test entries from
//! plain-text runner output when JSON parsing is unavailable, and
//! [`try_read_stdin`] which combines the stdin guard (via
//! [`crate::cmd::should_read_stdin`]), chunked read, and whitespace-only check
//! into a single call.
//!
//! Also provides [`run_test_runner`] which encapsulates the complete
//! passthrough-check / stdin-or-spawn / parse / emit / stats / analytics
//! pipeline shared by vitest, playwright, cypress, swift, and dotnet.

use std::io;
use std::process::ExitCode;
use std::sync::LazyLock;
use std::time::Instant;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::{TestEntry, TestOutcome, TestResult};
use crate::runner::{CommandOutput, CommandRunner};

// ============================================================================
// Shared run_test_runner pipeline
// ============================================================================

/// Source of the exit code for a test-runner invocation.
///
/// Replaces `Option<Option<i32>>` to make each case unambiguous in debug output
/// and match arms:
/// - [`ExitSource::Stdin`] — output came from piped stdin; no process exit code.
/// - [`ExitSource::Process`]`(Some(n))` — process exited normally with code `n`.
/// - [`ExitSource::Process`]`(None)` — process was killed by a signal (no numeric
///   code).
#[derive(Debug, Clone, Copy)]
pub(super) enum ExitSource {
    /// No process was spawned — output arrived from stdin.
    Stdin,
    /// A process was spawned. `Some(n)` = exited with code `n`; `None` = signal-killed.
    Process(Option<i32>),
}

/// Argument-preparation strategy for a test runner command.
///
/// Groups the two argument-transformation closures passed to [`run_test_runner`]
/// so that call sites name each closure explicitly rather than relying on
/// positional order (which is easy to confuse when both have identical types).
///
/// - `passthrough` — called on the `SKIM_PASSTHROUGH` path. MUST NOT inject
///   reporter/logger flags; it may only prepend the required subcommand token.
/// - `normal` — called on the normal spawn path to inject reporter flags (e.g.
///   `--reporter=json`, `--logger trx`).
pub(super) struct ArgPreparation<F, G>
where
    F: Fn(&[String]) -> Vec<String>,
    G: Fn(&[String]) -> Vec<String>,
{
    /// Args transformation for `SKIM_PASSTHROUGH` mode — subcommand only, no flags.
    pub passthrough: F,
    /// Args transformation for the normal spawn path — subcommand + reporter flags.
    pub normal: G,
}

/// Configuration for a test runner command.
///
/// Used by [`run_test_runner`] to spawn the test process. Each field controls
/// a specific aspect of how the command is launched.
pub(super) struct TestRunnerConfig<'a> {
    /// The binary name to invoke (e.g. `"vitest"`, `"swift"`, `"dotnet"`).
    pub program: &'a str,
    /// Human-readable install hint emitted in the error when the binary is not
    /// found (e.g. `"Install vitest locally (npm install -D vitest)"`).
    pub install_hint: &'a str,
    /// When `true`, fall back to `npx <program>` if the binary is not in PATH
    /// (appropriate for Node.js tools: vitest, jest, playwright, cypress).
    pub node_fallback: bool,
    /// Extra environment variables to set when spawning the process. Pass `&[]`
    /// if none are needed.
    pub env_overrides: &'a [(&'a str, &'a str)],
}

/// Orchestrate the full test-runner pipeline.
///
/// Handles in order:
/// 1. Passthrough mode — if `SKIM_PASSTHROUGH=1`, forwards raw output and
///    returns immediately.
/// 2. Stdin — if the caller has piped stdin (`try_read_stdin`), use that as
///    the raw output.
/// 3. Spawn — otherwise, spawn the process via `spawn_runner`.
/// 4. Parse — call `parse_fn` on the raw output string.
/// 5. Emit — print parsed result to stdout, emit tier markers to stderr, and
///    call [`emit_failure_context`] when failures are present.
/// 6. Stats — emit token-reduction stats when `show_stats` is `true`.
/// 7. Analytics — record usage via [`crate::analytics::try_record_command`].
///
/// `arg_prep` groups the two argument-transformation closures (see [`ArgPreparation`]).
/// The `passthrough` closure MUST NOT inject reporter/logger flags. For runners that
/// require a subcommand token (e.g. playwright "test", cypress "run"), it should
/// prepend that token and nothing else. For runners with no required subcommand
/// (e.g. vitest, jest), use `|a| a.to_vec()`.
/// The `normal` closure injects reporter flags for the normal spawn path
/// (e.g. `--reporter=json`). It is `Fn` rather than `FnOnce` because the borrow
/// checker requires a shared reference.
///
/// `parse_fn` is `FnOnce` because it is called exactly once and may capture
/// owned state (e.g. closures that embed TRX detection for dotnet).
///
/// # Exit-code semantics
///
/// A non-zero or missing (signal-killed) exit code from the spawned process is
/// treated as a failure even when the parser reports zero failed tests. This
/// prevents skim from returning `SUCCESS` when the runner is terminated
/// abnormally (e.g. OOM kill, timeout, compilation error before any test runs).
/// The stdin path has no process exit code and therefore does not apply this
/// override.
pub(super) fn run_test_runner<F, G>(
    config: &TestRunnerConfig<'_>,
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
    arg_prep: ArgPreparation<F, G>,
    parse_fn: impl FnOnce(&str) -> ParseResult<TestResult>,
) -> anyhow::Result<ExitCode>
where
    F: Fn(&[String]) -> Vec<String>,
    G: Fn(&[String]) -> Vec<String>,
{
    // Passthrough mode: bypass compression, run the raw command and forward output.
    if crate::cmd::is_passthrough_mode() {
        return run_passthrough(args, arg_prep.passthrough, |arg_refs| {
            spawn_runner_raw(config, arg_refs)
        });
    }

    let start = Instant::now();

    // Prefer stdin if piped; otherwise spawn the process.
    let (raw_output, exit_source) = if let Some(stdin_content) = try_read_stdin(args)? {
        (stdin_content, ExitSource::Stdin)
    } else {
        let prepared = (arg_prep.normal)(args);
        let (text, code) = spawn_runner(config, &prepared)?;
        (text, ExitSource::Process(code))
    };

    let result = parse_fn(&raw_output);

    let exit_code = match &result {
        ParseResult::Full(test_result) | ParseResult::Degraded(test_result, _) => {
            println!("{test_result}");
            let stderr = io::stderr();
            let mut handle = stderr.lock();
            let _ = result.emit_markers(&mut handle);

            let ec = resolve_exit_code(test_result.summary.fail, exit_source);
            if ec != ExitCode::SUCCESS {
                emit_failure_context(&raw_output, exit_code_byte(exit_source) as i32);
            }
            ec
        }
        ParseResult::Passthrough(raw) => {
            println!("{raw}");
            let _ = result.emit_markers(&mut io::stderr().lock());
            ExitCode::FAILURE
        }
    };

    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(&raw_output, result.content());
        crate::process::report_token_stats(orig, comp, "");
    }

    crate::analytics::try_record_command(
        rec.with_tier(result.tier_name()),
        raw_output,
        result.content().to_string(),
        crate::cmd::format_analytics_label("test", config.program, &args.join(" ")),
        start.elapsed(),
    );

    Ok(exit_code)
}

// ============================================================================
// Exit-code resolution helpers
// ============================================================================

/// Determine the final [`ExitCode`] for a parsed test result.
///
/// Returns `FAILURE` (or a specific non-zero code) when either:
/// - `fail_count > 0` — the parser found at least one failing test, OR
/// - `exit_source` is [`ExitSource::Process`] with a non-zero or missing code.
///
/// This prevents skim from returning `SUCCESS` when the runner exits non-zero
/// even though the parser reports zero failures (e.g. OOM kill before tests run,
/// compilation error, or framework-level panic).
///
/// When `exit_source` is [`ExitSource::Stdin`] and `fail_count` is zero, the
/// result is `SUCCESS` — no process was spawned so no non-zero exit can be observed.
pub(super) fn resolve_exit_code(fail_count: usize, exit_source: ExitSource) -> ExitCode {
    let process_failed = match exit_source {
        ExitSource::Stdin => false,
        ExitSource::Process(Some(0)) => false,
        ExitSource::Process(_) => true, // non-zero or signal-killed
    };

    if fail_count > 0 || process_failed {
        ExitCode::from(exit_code_byte(exit_source))
    } else {
        ExitCode::SUCCESS
    }
}

/// Extract a clamped 1–255 exit code byte from an [`ExitSource`].
///
/// - `Process(Some(n))` → `n.clamp(1, 255) as u8`
/// - `Process(None)` (signal kill) → `1`
/// - `Stdin` (no process spawned) → `1`
fn exit_code_byte(exit_source: ExitSource) -> u8 {
    match exit_source {
        ExitSource::Process(Some(n)) => n.clamp(1, 255) as u8,
        _ => 1,
    }
}

/// Spawn the test runner, combine stdout+stderr, and strip ANSI escape codes.
///
/// Used by the normal (non-passthrough) path of [`run_test_runner`]. Returns
/// the clean combined output paired with the process exit code. The exit code
/// is `None` when the process was killed by a signal before exiting normally.
///
/// Returning the exit code lets [`run_test_runner`] treat a non-zero or missing
/// exit as a failure even when the parser reports zero failed tests (e.g. the
/// runner was OOM-killed before executing any test cases).
fn spawn_runner(
    config: &TestRunnerConfig<'_>,
    args: &[String],
) -> anyhow::Result<(String, Option<i32>)> {
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let runner = CommandRunner::new();
    let output = if config.node_fallback {
        runner.run_with_node_fallback(config.program, &arg_refs)
    } else if config.env_overrides.is_empty() {
        runner.run(config.program, &arg_refs)
    } else {
        runner.run_with_env(config.program, &arg_refs, config.env_overrides)
    }
    .map_err(|e| {
        anyhow::anyhow!(
            "failed to run {}: {e}\nHint: {}",
            config.program,
            config.install_hint
        )
    })?;

    let exit_code = output.exit_code;
    let combined = crate::cmd::combine_output(&output);
    Ok((crate::output::strip_ansi(&combined), exit_code))
}

/// Spawn the test runner for the passthrough path, returning a [`CommandOutput`].
///
/// Does NOT combine or strip — passthrough forwards the raw process output.
fn spawn_runner_raw(
    config: &TestRunnerConfig<'_>,
    arg_refs: &[&str],
) -> anyhow::Result<CommandOutput> {
    let runner = CommandRunner::new();
    if config.node_fallback {
        runner.run_with_node_fallback(config.program, arg_refs)
    } else if config.env_overrides.is_empty() {
        runner.run(config.program, arg_refs)
    } else {
        runner.run_with_env(config.program, arg_refs, config.env_overrides)
    }
    .map_err(|e| {
        anyhow::anyhow!(
            "failed to run {}: {e}\nHint: {}",
            config.program,
            config.install_hint
        )
    })
}

// ============================================================================

/// Identifies which test runner produced the text being scraped.
///
/// Each runner has a distinct output format for failed tests, so kind-sensitive
/// regex patterns are required to avoid false positives across runners.
///
/// Variants `Pytest` and `Go` are provided for completeness and future use.
/// Currently only `Cargo`, `Vitest`, and `Cypress` are consumed by Tier-2 regex
/// paths; `Go`'s Tier-2 already extracts test names directly and `Pytest` uses
/// passthrough for its Tier-2. `Swift` and `Dotnet` use their own local regexes
/// instead of scrape_failures.
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
    /// `cypress run` text format: failure names scraped from indented lines
    Cypress,
}

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

static RE_CYPRESS_FAIL: LazyLock<Regex> = LazyLock::new(|| {
    // Cypress mocha text: `    N) test name` (numbered failure list)
    Regex::new(r"^\s+\d+\)\s+(.+)$").expect("valid cypress fail regex")
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
/// # Stdin path always returns FAILURE
///
/// When input arrives from stdin, no process exit code is available — skim does not
/// know whether the upstream command succeeded or failed. `FAILURE` is returned as a
/// conservative default so that callers in a pipeline treat ambiguous output as an
/// error rather than silently swallowing it. Full exit-code semantics are only
/// available on the spawn path (sub-case 2).
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
/// Maximum number of test entries collected during parsing (Tier-1 and Tier-2).
///
/// Caps both the regex-scrape path in [`scrape_failures`] and Tier-1 JSON
/// collection in parsers that have wide (many suites, shallow depth) payloads.
/// All parsers import this constant rather than defining their own.
pub(super) const MAX_ENTRIES: usize = 100;

/// Cap matches [`MAX_ENTRIES`] to keep output size predictable regardless of tier.
pub(super) fn scrape_failures(text: &str, kind: TestKind) -> Vec<TestEntry> {
    // Strip ANSI escape codes so color-enabled output (e.g. pytest --color=yes,
    // vitest with TTY detected) does not break pattern matching.
    //
    // Fast-path: when the caller has already stripped ANSI (no ESC bytes remain),
    // borrows the slice directly — no allocation. This eliminates the double-strip
    // in `vitest::try_parse_regex`, which strips ANSI before calling here.
    let cleaned = crate::output::strip_ansi_cow(text);

    let re = match kind {
        TestKind::Cargo => &*RE_CARGO_FAIL,
        TestKind::Pytest => &*RE_PYTEST_FAIL,
        TestKind::Go => &*RE_GO_FAIL,
        TestKind::Vitest => &*RE_VITEST_FAIL,
        TestKind::Cypress => &*RE_CYPRESS_FAIL,
    };

    let mut entries: Vec<TestEntry> = Vec::new();
    for line in cleaned.lines() {
        if entries.len() >= MAX_ENTRIES {
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

/// Extract the first balanced JSON object from `text`.
///
/// Used by Cypress and Playwright parsers to isolate the JSON report object
/// from output that may contain preamble or trailing log lines. Handles
/// nested objects and string literals with escaped characters.
pub(super) fn extract_json_object(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;

    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, &b) in bytes[start..].iter().enumerate() {
        let idx = start + i;
        if escape_next {
            escape_next = false;
            continue;
        }
        if in_string {
            match b {
                b'\\' => escape_next = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=idx]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_utils::make_output_full;

    // ========================================================================
    // resolve_exit_code tests
    //
    // Tests for the extracted exit-code decision logic, covering the
    // "zero failures + non-zero exit" path that was previously only
    // indirectly exercised through run_passthrough.
    // ========================================================================

    #[test]
    fn test_resolve_exit_code_zero_fail_exit_zero_is_success() {
        // Parser reports 0 failures, process exited 0 → SUCCESS.
        let code = resolve_exit_code(0, ExitSource::Process(Some(0)));
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn test_resolve_exit_code_zero_fail_nonzero_exit_is_failure() {
        // Parser reports 0 failures but process exited 1 (e.g. compilation error
        // before any tests ran). Must return FAILURE rather than SUCCESS.
        let code = resolve_exit_code(0, ExitSource::Process(Some(1)));
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn test_resolve_exit_code_zero_fail_exit_two_preserves_code() {
        // Process exited 2 (common for "compilation failed") with 0 parser failures.
        let code = resolve_exit_code(0, ExitSource::Process(Some(2)));
        assert_eq!(code, ExitCode::from(2u8));
    }

    #[test]
    fn test_resolve_exit_code_zero_fail_signal_kill_is_failure() {
        // Signal-killed process (Process(None)) with 0 parser failures → FAILURE.
        let code = resolve_exit_code(0, ExitSource::Process(None));
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn test_resolve_exit_code_nonzero_fail_exit_zero_is_failure() {
        // Parser found failures; process exited 0 → still FAILURE.
        let code = resolve_exit_code(3, ExitSource::Process(Some(0)));
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn test_resolve_exit_code_stdin_path_zero_fail_is_success() {
        // Stdin path (no process spawned), parser found 0 failures → SUCCESS.
        let code = resolve_exit_code(0, ExitSource::Stdin);
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn test_resolve_exit_code_stdin_path_nonzero_fail_is_failure() {
        // Stdin path, parser found failures → FAILURE.
        let code = resolve_exit_code(2, ExitSource::Stdin);
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn test_resolve_exit_code_clamps_exit_255() {
        // Exit code 255 is the max allowed; should be preserved.
        let code = resolve_exit_code(0, ExitSource::Process(Some(255)));
        assert_eq!(code, ExitCode::from(255u8));
    }

    // ========================================================================
    // run_passthrough tests (spawn branch)
    //
    // In Rust unit tests stdin is never piped (it's the test harness terminal),
    // so `try_read_stdin` always returns Ok(None) and `run_passthrough` always
    // takes the spawn branch. The stdin branch is covered by E2E tests in
    // cli_e2e_test_parsers (test_vitest_passthrough_* and
    // test_pytest_passthrough_*) which pipe stdin via `write_stdin`.
    // ========================================================================

    #[test]
    fn test_run_passthrough_spawn_exit_zero_returns_success() {
        let code = run_passthrough(
            &[],
            |a| a.to_vec(),
            |_| Ok(make_output_full("ok output\n", "", Some(0))),
        )
        .expect("run_passthrough should not error");
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn test_run_passthrough_spawn_exit_nonzero_preserves_code() {
        let code = run_passthrough(
            &[],
            |a| a.to_vec(),
            |_| Ok(make_output_full("fail output\n", "", Some(2))),
        )
        .expect("run_passthrough should not error");
        // exit code 2 → ExitCode::from(2)
        assert_eq!(code, ExitCode::from(2u8));
    }

    #[test]
    fn test_run_passthrough_spawn_exit_none_returns_failure() {
        // When the command is killed by a signal, exit_code is None.
        // run_passthrough falls back to exit code 1 (FAILURE).
        let code = run_passthrough(&[], |a| a.to_vec(), |_| Ok(make_output_full("", "", None)))
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
                Ok(make_output_full("", "", Some(0)))
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

    // ========================================================================
    // Issue regression: passthrough must NOT inject reporter flags
    //
    // Before the fix, run_test_runner passed `prepare_args` (which injects
    // --reporter=json) to run_passthrough. The passthrough path must call
    // `passthrough_prepare_args` instead, which is flag-free.
    // ========================================================================

    /// Regression: vitest passthrough must NOT inject `--reporter=json`.
    ///
    /// The passthrough closure for vitest is `|a| a.to_vec()` (identity).
    /// run_passthrough must receive exactly the user-supplied args, with no
    /// reporter flag appended.
    #[test]
    fn test_passthrough_prepare_args_does_not_inject_reporter_vitest() {
        let user_args = vec!["--run".to_string(), "math".to_string()];
        let mut received_args: Vec<String> = Vec::new();

        run_passthrough(
            &user_args,
            // identity — mimics vitest's passthrough_prepare_args
            |a| a.to_vec(),
            |arg_refs| {
                received_args = arg_refs.iter().map(|s| s.to_string()).collect();
                Ok(make_output_full("", "", Some(0)))
            },
        )
        .expect("run_passthrough should not error");

        assert!(
            !received_args.iter().any(|a| a.contains("--reporter")),
            "passthrough must not inject --reporter flag: {received_args:?}"
        );
        assert!(
            !received_args.iter().any(|a| a.contains("--json")),
            "passthrough must not inject --json flag: {received_args:?}"
        );
        assert_eq!(
            received_args,
            vec!["--run", "math"],
            "passthrough must forward user args unchanged: {received_args:?}"
        );
    }

    /// Regression: playwright/cypress passthrough must prepend the subcommand
    /// token ("test" or "run") but must NOT inject `--reporter=json`.
    ///
    /// The passthrough closure for playwright is:
    ///   `|a| { let mut v = vec!["test".to_string()]; v.extend_from_slice(a); v }`
    /// run_passthrough must receive ["test", ...user_args] with no reporter flag.
    #[test]
    fn test_passthrough_prepare_args_prepends_subcommand_without_reporter() {
        let user_args = vec!["--project=chromium".to_string()];
        let mut received_args: Vec<String> = Vec::new();

        run_passthrough(
            &user_args,
            // subcommand-only — mimics playwright's passthrough_prepare_args
            |a| {
                let mut v = vec!["test".to_string()];
                v.extend_from_slice(a);
                v
            },
            |arg_refs| {
                received_args = arg_refs.iter().map(|s| s.to_string()).collect();
                Ok(make_output_full("", "", Some(0)))
            },
        )
        .expect("run_passthrough should not error");

        assert_eq!(
            received_args.first().map(String::as_str),
            Some("test"),
            "first arg must be the subcommand token: {received_args:?}"
        );
        assert!(
            !received_args.iter().any(|a| a.contains("--reporter")),
            "passthrough must not inject --reporter flag: {received_args:?}"
        );
        assert_eq!(
            received_args,
            vec!["test", "--project=chromium"],
            "passthrough must be [subcommand, ...user_args]: {received_args:?}"
        );
    }

    // ========================================================================
    // Issue regression: spawn_runner exit code surfacing
    //
    // Before the fix, spawn_runner returned only String and the exit code was
    // discarded. The run_test_runner function now receives (String, Option<i32>)
    // and treats non-zero / missing exit codes as failures even when the parser
    // reports zero failing tests.
    //
    // We test the run_passthrough exit-code forwarding as a proxy for the
    // spawn_exit_code semantics (the passthrough path already surfaces the exit
    // code correctly and those tests cover exit code 0, non-zero, and None).
    //
    // The spawn_runner itself cannot be called from unit tests (it requires a
    // live binary), so we instead test the outcome logic via make_output helpers
    // that exercise the same Option<Option<i32>> handling indirectly through the
    // run_passthrough path (which already has full exit-code tests above).
    // ========================================================================

    /// Regression: signal-killed process (exit_code=None) must return FAILURE.
    ///
    /// Previously run_passthrough (and by extension run_test_runner) would use
    /// .unwrap_or(1) — this test verifies that path is still correct.
    #[test]
    fn test_passthrough_signal_kill_returns_failure() {
        let code = run_passthrough(
            &[],
            |a| a.to_vec(),
            |_| Ok(make_output_full("partial output\n", "", None)),
        )
        .expect("run_passthrough should not error");
        assert_eq!(
            code,
            ExitCode::FAILURE,
            "signal-killed process must map to FAILURE"
        );
    }

    /// Regression: non-zero exit code must be preserved even when stdout is empty.
    ///
    /// Exercises the path where the process exited with code 2 (e.g. compilation
    /// error) and produced no parseable test output.
    #[test]
    fn test_passthrough_compilation_error_exit_code_preserved() {
        let code = run_passthrough(
            &[],
            |a| a.to_vec(),
            |_| Ok(make_output_full("error: could not compile\n", "", Some(2))),
        )
        .expect("run_passthrough should not error");
        assert_eq!(
            code,
            ExitCode::from(2u8),
            "compilation error exit code 2 must be forwarded"
        );
    }

    // ========================================================================
    // extract_json_object tests
    //
    // `extract_json_object` is used by the Cypress and Playwright parsers to
    // isolate the JSON report from output that may include preamble log lines.
    // Vitest's equivalent `extract_json_by_brace_balance` has 8 edge-case tests;
    // these four tests establish the same coverage baseline for this function.
    // ========================================================================

    #[test]
    fn test_extract_json_object_simple() {
        let input = r#"{"key":"value"}"#;
        let result = extract_json_object(input);
        assert_eq!(result, Some(r#"{"key":"value"}"#));
    }

    #[test]
    fn test_extract_json_object_nested() {
        let input = r#"{"outer":{"inner":"data"},"count":42}"#;
        let result = extract_json_object(input);
        assert_eq!(result, Some(r#"{"outer":{"inner":"data"},"count":42}"#));
    }

    #[test]
    fn test_extract_json_object_garbage_prefix() {
        // Preamble log lines precede the JSON object — common in cypress/playwright output.
        let input =
            "Starting Cypress run...\nConnecting to Cypress Cloud\n{\"stats\":{\"passes\":3}}\n";
        let result = extract_json_object(input);
        assert!(result.is_some(), "should find JSON despite garbage prefix");
        assert!(
            result.unwrap().starts_with('{'),
            "extracted slice must start at opening brace"
        );
        // Verify the extracted object parses correctly
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(result.unwrap());
        assert!(parsed.is_ok(), "extracted JSON must parse: {:?}", result);
    }

    #[test]
    fn test_extract_json_object_unclosed_brace_returns_none() {
        // An unclosed brace means depth never returns to 0 — None is returned.
        let input = r#"{"key":"value", "nested": {"missing_close": true}"#;
        let result = extract_json_object(input);
        assert!(
            result.is_none(),
            "unclosed brace must return None, got: {result:?}"
        );
    }
}
