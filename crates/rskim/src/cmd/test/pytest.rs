//! Pytest parser with three-tier degradation (#47)
//!
//! Parses pytest text output into structured [`TestResult`] using a three-tier
//! strategy:
//!
//! - **Tier 1 (text state machine):** Scans all output lines, counting PASSED/FAILED/
//!   SKIPPED/ERROR outcomes and extracting individual test names. Requires the summary
//!   line to produce a `Full` result.
//! - **Tier 2 (passthrough):** Returns raw output unmodified when no summary can be
//!   found at all.
//!
//! ## Usage
//!
//! ```text
//! skim pytest [args...]          # Execute pytest, parse output
//! pytest ... | skim pytest       # Parse piped stdin
//! ```

use std::collections::HashSet;
use std::io;
use std::process::ExitCode;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

use crate::cmd::{combine_output, user_has_flag};
use crate::output::canonical::{TestEntry, TestOutcome, TestResult, TestSummary};
use crate::output::{ParseResult, strip_ansi};
use crate::runner::{CommandOutput, CommandRunner};

use super::shared::{self, try_read_stdin};

// ============================================================================
// Public entry point
// ============================================================================

/// Run pytest and parse its output, or parse piped stdin.
///
/// Detection logic (via [`try_read_stdin`]):
/// - If args are present OR stdin is a terminal → run pytest (execution mode)
/// - If args are empty AND stdin is piped → read stdin; if empty, fall back
///   to running pytest (handles test harness environments where stdin is a
///   pipe with no data)
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    // Passthrough mode: bypass compression and forward raw output unchanged.
    if crate::cmd::is_passthrough_mode() {
        return shared::run_passthrough(args, build_args, run_pytest);
    }

    // Intercept --help/-h: show skim's pytest help, then forward to real pytest
    // so the user sees both skim's flags and pytest's own options.
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_pytest_help();
    }

    let output = if let Some(raw) = try_read_stdin(args)? {
        // Piped stdin with non-empty content: wrap in a synthetic CommandOutput
        // so the rest of the function can treat it uniformly with spawned output.
        CommandOutput {
            stdout: raw,
            stderr: String::new(),
            exit_code: None,
            duration: Duration::ZERO,
        }
    } else {
        // Terminal, args present, or empty pipe: always run pytest.
        let final_args = build_args(args);
        let arg_refs: Vec<&str> = final_args.iter().map(String::as_str).collect();
        run_pytest(&arg_refs)?
    };

    let combined = combine_output(&output);
    // Strip ANSI escape codes before parsing so color sequences (e.g.,
    // `pytest --color=yes`) do not interfere with string matching.
    let cleaned = strip_ansi(&combined);
    let result = parse(&cleaned);

    emit_result(&result, &output, &cleaned)?;

    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(&cleaned, result.content());
        crate::process::report_token_stats(orig, comp, "");
    }

    // Record analytics (fire-and-forget, non-blocking).
    crate::analytics::try_record_command(
        rec.with_tier(result.tier_name()),
        cleaned,
        result.content().to_string(),
        crate::cmd::format_analytics_label("test", "pytest", &args.join(" ")),
        output.duration,
    );

    // Exit code: mirror pytest's exit code if we ran it, or infer from parse
    let code = match output.exit_code {
        Some(0) => ExitCode::SUCCESS,
        Some(_) => ExitCode::FAILURE,
        None => {
            // Piped or signal-killed: infer from parse result
            match &result {
                ParseResult::Full(tr) | ParseResult::Degraded(tr, _) => {
                    if tr.summary.fail > 0 {
                        ExitCode::FAILURE
                    } else {
                        ExitCode::SUCCESS
                    }
                }
                ParseResult::Passthrough(_) => ExitCode::FAILURE,
            }
        }
    };

    Ok(code)
}

/// Print skim's pytest-specific help to stdout.
///
/// This is shown before forwarding `--help` to real pytest so the user
/// sees both skim's behavior and pytest's own flags.
fn print_pytest_help() {
    println!("skim pytest [ARGS...]");
    println!();
    println!("  Run pytest and parse its output into a structured summary.");
    println!();
    println!("  BEHAVIOR:");
    println!("    - Injects --tb=short and -q unless you override them");
    println!("    - Parses output into PASS/FAIL/SKIP counts with failure details");
    println!("    - Supports piped input: pytest ... | skim pytest");
    println!();
    println!("  FLAGS MANAGED BY SKIM:");
    println!("    --tb=short     Injected unless --tb is already set");
    println!("    -q             Injected unless -q/-v/--quiet/--verbose is set");
    println!();
    println!("--- pytest native help follows ---");
    println!();
}

// ============================================================================
// Arg building
// ============================================================================

/// Build the final argument list for pytest.
///
/// If the user hasn't set `--tb` or `-q`/`--quiet` or `-v`/`--verbose`,
/// inject `--tb=short` and `-q` for cleaner parseable output.
fn build_args(user_args: &[String]) -> Vec<String> {
    let mut args: Vec<String> = user_args.to_vec();

    if !user_has_flag(user_args, &["--tb"]) {
        args.push("--tb=short".to_string());
    }

    if !user_has_flag(user_args, &["-q", "--quiet", "-v", "--verbose"]) {
        args.push("-q".to_string());
    }

    args
}

// ============================================================================
// Command execution
// ============================================================================

/// Execute pytest with the given arguments.
fn run_pytest(args: &[&str]) -> anyhow::Result<CommandOutput> {
    let runner = CommandRunner::new(Some(crate::cmd::DEFAULT_CMD_TIMEOUT));
    runner
        .run("pytest", args)
        .map_err(|e| anyhow::anyhow!("{e}\n\nHint: Is pytest installed? Try: pip install pytest"))
}

// ============================================================================
// Three-tier parser
// ============================================================================

/// Parse pytest output using three-tier degradation.
///
/// Returns `Full` if tier 1 succeeds, or `Passthrough` if no summary line is found.
fn parse(output: &str) -> ParseResult<TestResult> {
    // Tier 1: full text state machine
    if let Some(result) = tier1_parse(output) {
        return ParseResult::Full(result);
    }

    // Tier 2: passthrough
    ParseResult::Passthrough(output.to_string())
}

/// Regex matching the pytest summary line structure.
///
/// Matches lines like:
/// - `============================== 5 passed in 0.12s ===============================`
/// - `=== 3 failed in 0.15s ===`
/// - `============== 4 passed, 1 failed, 1 skipped in 0.20s =============`
/// - `1 failed, 2 error in 0.30s`
///
/// The pattern matches `in <duration>s` at the end, with optional `=` padding.
/// Individual counts (passed/failed/skipped/error) are extracted by a separate
/// per-pair regex so that "passed" is not required.
static SUMMARY_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"=*\s*(?:\d+\s+(?:passed|failed|skipped|error)(?:,\s+)?)+\s+in\s+([\d.]+)s\s*=*")
        .expect("summary line regex is valid")
});

/// Regex extracting individual `N category` pairs from a summary line.
static SUMMARY_PAIR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d+)\s+(passed|failed|skipped|error)").expect("summary pair regex is valid")
});

/// Parsed summary counts extracted from a pytest summary line.
struct SummaryCounts {
    pass: usize,
    fail: usize,
    skip: usize,
    duration_ms: Option<u64>,
}

/// Try to parse the pytest summary line, extracting counts and duration.
///
/// Returns `None` if the line does not match the summary pattern.
fn parse_summary_line(line: &str) -> Option<SummaryCounts> {
    let line_caps = SUMMARY_LINE_RE.captures(line)?;

    // Extract duration from the capture group
    let duration_ms = line_caps.get(1).and_then(|m| {
        let secs: f64 = m.as_str().parse().ok()?;
        Some((secs * 1000.0) as u64)
    });

    let mut pass: usize = 0;
    let mut fail: usize = 0;
    let mut skip: usize = 0;

    for caps in SUMMARY_PAIR_RE.captures_iter(line) {
        let count: usize = caps[1].parse().unwrap_or(0);
        match &caps[2] {
            "passed" => pass = count,
            "failed" => fail += count,
            "skipped" => skip = count,
            "error" => fail += count,
            _ => {}
        }
    }

    Some(SummaryCounts {
        pass,
        fail,
        skip,
        duration_ms,
    })
}

// ============================================================================
// Tier 1: Text state machine
// ============================================================================

/// Extract `(name, outcome)` from a verbose pytest marker line.
///
/// Matches lines that end with ` PASSED`, ` FAILED`, or ` SKIPPED` (verbose
/// mode output). Returns `None` for all other lines.
fn parse_verbose_marker(line: &str) -> Option<(String, TestOutcome)> {
    let (suffix, outcome) = if line.ends_with(" PASSED") {
        (" PASSED", TestOutcome::Pass)
    } else if line.ends_with(" FAILED") {
        (" FAILED", TestOutcome::Fail)
    } else if line.ends_with(" SKIPPED") {
        (" SKIPPED", TestOutcome::Skip)
    } else {
        return None;
    };
    Some((line.strip_suffix(suffix).unwrap().to_string(), outcome))
}

/// Tracks which section of pytest output the parser is currently inside.
///
/// The two section booleans (`in_failures`, `in_summary_info`) were mutually
/// exclusive — exactly one was true at any time, or both were false (Normal).
/// An enum makes all three states explicit and eliminates the illegal state
/// where both would be true simultaneously.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PytestSection {
    /// Outside any special section.
    Normal,
    /// Inside the `=== FAILURES ===` block.
    Failures,
    /// Inside the `=== short test summary info ===` block.
    SummaryInfo,
}

/// Classify a `=== ... ===` section-header line.
///
/// Returns `Some(PytestSection)` when `trimmed` is a pytest section header,
/// or `None` when it is not (allowing the caller to fall through to other
/// processing).
///
/// The three patterns, in priority order:
/// 1. Contains `"FAILURES"` → `Failures` section begins.
/// 2. Contains `"short test summary info"` → `SummaryInfo` section begins.
/// 3. Starts **and** ends with `"==="` (catch-all) → `Normal` (section ends).
fn detect_section_header(trimmed: &str) -> Option<PytestSection> {
    if !trimmed.starts_with("===") {
        return None;
    }
    if trimmed.contains("FAILURES") {
        Some(PytestSection::Failures)
    } else if trimmed.contains("short test summary info") {
        Some(PytestSection::SummaryInfo)
    } else if trimmed.ends_with("===") {
        Some(PytestSection::Normal)
    } else {
        None
    }
}

/// Tier 1: Full text state machine parse.
///
/// Scans every line for PASSED/FAILED/SKIPPED/ERROR markers, extracts test names
/// from "short test summary" lines, collects failure output, and validates against
/// the summary line.
fn tier1_parse(output: &str) -> Option<TestResult> {
    let mut entries: Vec<TestEntry> = Vec::new();
    let mut section = PytestSection::Normal;
    let mut current_failure_name: Option<String> = None;
    let mut current_failure_detail: Vec<String> = Vec::new();

    // Track summary values
    let mut summary_counts: Option<SummaryCounts> = None;

    for line in output.lines() {
        let trimmed = line.trim();

        // Detect summary line
        if let Some(counts) = parse_summary_line(trimmed) {
            summary_counts = Some(counts);
            continue;
        }

        // Detect section headers (=== ... ===)
        if let Some(new_section) = detect_section_header(trimmed) {
            // Flush any pending failure when leaving Failures or entering SummaryInfo.
            if section == PytestSection::Failures || new_section == PytestSection::SummaryInfo {
                flush_failure(
                    &mut entries,
                    &mut current_failure_name,
                    &mut current_failure_detail,
                );
            }
            section = new_section;
            continue;
        }

        // Inside FAILURES section: extract individual test failure blocks
        if section == PytestSection::Failures {
            // Test failure headers look like: "________ test_name ________"
            if trimmed.starts_with('_') && trimmed.ends_with('_') {
                // Flush previous failure
                flush_failure(
                    &mut entries,
                    &mut current_failure_name,
                    &mut current_failure_detail,
                );
                // Extract test name from between underscores
                let name = trimmed.trim_matches('_').trim().to_string();
                if !name.is_empty() {
                    current_failure_name = Some(name);
                }
            } else if current_failure_name.is_some() {
                current_failure_detail.push(line.to_string());
            }
            continue;
        }

        // Inside "short test summary info": parse FAILED/ERROR lines
        if section == PytestSection::SummaryInfo {
            let rest = trimmed
                .strip_prefix("FAILED ")
                .or_else(|| trimmed.strip_prefix("ERROR "));
            if let Some(rest) = rest {
                // Format: "FAILED tests/test_b.py::test_two - assert 1 == 2"
                let (name, detail) = if let Some(dash_pos) = rest.find(" - ") {
                    (
                        rest[..dash_pos].to_string(),
                        Some(rest[dash_pos + 3..].to_string()),
                    )
                } else {
                    (rest.to_string(), None)
                };
                entries.push(TestEntry {
                    name,
                    outcome: TestOutcome::Fail,
                    detail,
                });
            }
            continue;
        }

        // Outside special sections: look for per-line PASSED/FAILED/SKIPPED markers.
        // These appear in verbose mode output like:
        //   tests/test_a.py::test_one PASSED
        //   tests/test_a.py::test_two FAILED
        if let Some((name, outcome)) = parse_verbose_marker(trimmed) {
            entries.push(TestEntry {
                name,
                outcome,
                detail: None,
            });
        }
    }

    // Flush any remaining failure
    flush_failure(
        &mut entries,
        &mut current_failure_name,
        &mut current_failure_detail,
    );

    // Must have found a summary line to be a tier 1 result
    let counts = summary_counts?;

    // Deduplicate entries by test name. When pytest outputs verbose mode AND
    // a FAILURES section AND "short test summary info", the same test can
    // appear multiple times. Keep the first occurrence (which has the richest
    // detail from the FAILURES section).
    let mut seen = HashSet::new();
    entries.retain(|e| seen.insert(e.name.clone()));

    let summary = TestSummary {
        pass: counts.pass,
        fail: counts.fail,
        skip: counts.skip,
        duration_ms: counts.duration_ms,
    };

    Some(TestResult::new(summary, entries))
}

/// Flush a pending failure entry from the FAILURES section.
fn flush_failure(
    entries: &mut Vec<TestEntry>,
    name: &mut Option<String>,
    detail_lines: &mut Vec<String>,
) {
    if let Some(test_name) = name.take() {
        let detail = if detail_lines.is_empty() {
            None
        } else {
            // Take only non-empty trimmed lines for concise detail
            let trimmed: Vec<&str> = detail_lines
                .iter()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .collect();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.join("\n"))
            }
        };
        entries.push(TestEntry {
            name: test_name,
            outcome: TestOutcome::Fail,
            detail,
        });
        detail_lines.clear();
    }
}

// ============================================================================
// Output emission
// ============================================================================

/// Emit the parsed result to stdout/stderr.
///
/// When failures are present, delegates to [`shared::emit_failure_context`]
/// to append the last [`shared::MAX_FAILURE_CONTEXT_LINES`] lines of
/// `cleaned_output` so the agent can read error details without re-running
/// with `SKIM_PASSTHROUGH=1`.
fn emit_result(
    result: &ParseResult<TestResult>,
    output: &CommandOutput,
    cleaned_output: &str,
) -> anyhow::Result<()> {
    use std::io::Write;

    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();

    match result {
        ParseResult::Full(tr) | ParseResult::Degraded(tr, _) => {
            writeln!(out, "{tr}")?;
            result.emit_markers(&mut err)?;

            if tr.summary.fail > 0 {
                shared::emit_failure_context(cleaned_output, 1);
            }
        }
        ParseResult::Passthrough(raw) => {
            write!(out, "{raw}")?;
            result.emit_markers(&mut err)?;
        }
    }

    // If there were stderr warnings from pytest itself, forward them
    if !output.stderr.is_empty() && !result.is_passthrough() {
        write!(err, "{}", output.stderr)?;
    }

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_support::load_fixture;

    // ========================================================================
    // Tier 1 tests
    // ========================================================================

    #[test]
    fn test_tier1_all_pass() {
        let input = load_fixture("test", "pytest_pass.txt");
        let result = parse(&input);

        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );

        if let ParseResult::Full(tr) = &result {
            assert_eq!(tr.summary.pass, 5, "expected 5 passed");
            assert_eq!(tr.summary.fail, 0, "expected 0 failed");
            assert_eq!(tr.summary.skip, 0, "expected 0 skipped");
        }
    }

    #[test]
    fn test_tier1_with_failures() {
        let input = load_fixture("test", "pytest_fail.txt");
        let result = parse(&input);

        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );

        if let ParseResult::Full(tr) = &result {
            assert_eq!(tr.summary.pass, 2, "expected 2 passed");
            assert!(tr.summary.fail > 0, "expected failures");
            assert_eq!(tr.summary.fail, 1, "expected 1 failed");

            // Should have at least one failure entry
            let fail_entries: Vec<_> = tr
                .entries
                .iter()
                .filter(|e| e.outcome == TestOutcome::Fail)
                .collect();
            assert!(
                !fail_entries.is_empty(),
                "expected at least one FAIL entry, got none"
            );
        }
    }

    #[test]
    fn test_tier1_mixed() {
        let input = load_fixture("test", "pytest_mixed.txt");
        let result = parse(&input);

        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );

        if let ParseResult::Full(tr) = &result {
            assert_eq!(tr.summary.pass, 4, "expected 4 passed");
            assert_eq!(tr.summary.fail, 1, "expected 1 failed");
            assert_eq!(tr.summary.skip, 1, "expected 1 skipped");
        }
    }

    #[test]
    fn test_passthrough() {
        let input = "totally unrelated output\nno pytest here";
        let result = parse(input);
        assert!(
            result.is_passthrough(),
            "expected Passthrough, got {:?}",
            result.tier_name()
        );
    }

    // ========================================================================
    // Flag injection tests
    // ========================================================================

    #[test]
    fn test_flag_injection_skipped_with_verbose() {
        let user_args: Vec<String> = vec!["-v".to_string(), "tests/".to_string()];
        let built = build_args(&user_args);

        // Should NOT inject -q (because -v is present)
        assert!(
            !built.contains(&"-q".to_string()),
            "-q should not be injected when -v is present: {built:?}"
        );
        // Should still inject --tb=short (no --tb present)
        assert!(
            built.contains(&"--tb=short".to_string()),
            "--tb=short should be injected: {built:?}"
        );
    }

    #[test]
    fn test_flag_injection_skipped_with_tb() {
        let user_args: Vec<String> = vec!["--tb=long".to_string()];
        let built = build_args(&user_args);

        // Should NOT inject --tb=short (because --tb=long is present)
        assert!(
            !built.contains(&"--tb=short".to_string()),
            "--tb=short should not be injected when --tb=long is present: {built:?}"
        );
        // Should inject -q (no -q/-v/--quiet/--verbose present)
        assert!(
            built.contains(&"-q".to_string()),
            "-q should be injected: {built:?}"
        );
    }

    #[test]
    fn test_flag_injection_default() {
        let user_args: Vec<String> = vec!["tests/".to_string()];
        let built = build_args(&user_args);

        assert!(
            built.contains(&"--tb=short".to_string()),
            "--tb=short should be injected by default: {built:?}"
        );
        assert!(
            built.contains(&"-q".to_string()),
            "-q should be injected by default: {built:?}"
        );
    }

    #[test]
    fn test_flag_injection_skipped_with_quiet() {
        let user_args: Vec<String> = vec!["--quiet".to_string()];
        let built = build_args(&user_args);

        assert!(
            !built.contains(&"-q".to_string()),
            "-q should not be injected when --quiet is present: {built:?}"
        );
    }

    // ========================================================================
    // user_has_flag tests
    // ========================================================================

    #[test]
    fn test_user_has_flag_exact_match() {
        let args = vec!["-v".to_string(), "tests/".to_string()];
        assert!(user_has_flag(&args, &["-v"]));
    }

    #[test]
    fn test_user_has_flag_with_equals() {
        let args = vec!["--tb=long".to_string()];
        assert!(user_has_flag(&args, &["--tb"]));
    }

    #[test]
    fn test_user_has_flag_not_present() {
        let args = vec!["tests/".to_string()];
        assert!(!user_has_flag(&args, &["-v", "--verbose"]));
    }

    // ========================================================================
    // Summary parsing edge cases
    // ========================================================================

    #[test]
    fn test_summary_passed_only() {
        let line =
            "============================== 5 passed in 0.12s ===============================";
        let counts = parse_summary_line(line).expect("should match");
        assert_eq!(counts.pass, 5);
        assert_eq!(counts.fail, 0);
        assert_eq!(counts.skip, 0);
        assert_eq!(counts.duration_ms, Some(120));
    }

    #[test]
    fn test_summary_all_groups() {
        let line = "======= 10 passed, 2 failed, 3 skipped, 1 error in 1.50s =======";
        let counts = parse_summary_line(line).expect("should match");
        assert_eq!(counts.pass, 10);
        assert_eq!(counts.fail, 3); // 2 failed + 1 error
        assert_eq!(counts.skip, 3);
        assert_eq!(counts.duration_ms, Some(1500));
    }

    #[test]
    fn test_summary_no_match_on_garbage() {
        assert!(parse_summary_line("hello world").is_none());
    }

    #[test]
    fn test_summary_failed_only_no_passed() {
        let line = "=== 3 failed in 0.15s ===";
        let counts = parse_summary_line(line).expect("should match failed-only summary");
        assert_eq!(counts.pass, 0, "no passed tests");
        assert_eq!(counts.fail, 3, "3 failed tests");
        assert_eq!(counts.skip, 0, "no skipped tests");
        assert_eq!(counts.duration_ms, Some(150));
    }

    #[test]
    fn test_summary_failed_and_error_no_passed() {
        let line = "=== 1 failed, 2 error in 0.30s ===";
        let counts = parse_summary_line(line).expect("should match failed+error summary");
        assert_eq!(counts.pass, 0);
        assert_eq!(counts.fail, 3); // 1 failed + 2 error
        assert_eq!(counts.skip, 0);
        assert_eq!(counts.duration_ms, Some(300));
    }

    #[test]
    fn test_summary_duration_extraction() {
        let line = "============== 4 passed, 1 failed, 1 skipped in 0.20s =============";
        let counts = parse_summary_line(line).expect("should match");
        assert_eq!(counts.duration_ms, Some(200));
    }

    #[test]
    fn test_summary_quiet_mode_no_equals() {
        // Quiet mode can produce summary without === padding
        let line = "2 passed in 0.00s";
        let counts = parse_summary_line(line).expect("should match quiet mode");
        assert_eq!(counts.pass, 2);
        assert_eq!(counts.fail, 0);
        assert_eq!(counts.duration_ms, Some(0));
    }

    // ========================================================================
    // All-failures fixture test
    // ========================================================================

    #[test]
    fn test_tier1_all_failures() {
        let input = load_fixture("test", "pytest_all_fail.txt");
        let result = parse(&input);

        assert!(
            result.is_full(),
            "expected Full for all-failures output, got {:?}",
            result.tier_name()
        );

        if let ParseResult::Full(tr) = &result {
            assert_eq!(tr.summary.pass, 0, "expected 0 passed");
            assert_eq!(tr.summary.fail, 3, "expected 3 failed");
            assert_eq!(tr.summary.skip, 0, "expected 0 skipped");
            assert!(
                tr.summary.duration_ms.is_some(),
                "duration should be extracted"
            );

            // Should have failure entries
            let fail_entries: Vec<_> = tr
                .entries
                .iter()
                .filter(|e| e.outcome == TestOutcome::Fail)
                .collect();
            assert!(
                !fail_entries.is_empty(),
                "expected at least one FAIL entry for all-failures fixture"
            );
        }
    }

    // ========================================================================
    // Duration extraction tests
    // ========================================================================

    #[test]
    fn test_tier1_extracts_duration() {
        let input = load_fixture("test", "pytest_pass.txt");
        let result = parse(&input);

        if let ParseResult::Full(tr) = &result {
            assert!(
                tr.summary.duration_ms.is_some(),
                "duration_ms should be populated from summary line"
            );
            assert_eq!(tr.summary.duration_ms, Some(120));
        }
    }

    #[test]
    fn test_summary_line_extracts_duration() {
        let input = "============== 4 passed, 1 failed, 1 skipped in 0.20s ==============";
        let counts = parse_summary_line(input);
        assert!(counts.is_some());
        let counts = counts.unwrap();
        assert_eq!(counts.duration_ms, Some(200));
    }

    // ========================================================================
    // Deduplication tests
    // ========================================================================

    #[test]
    fn test_tier1_deduplicates_entries() {
        // Simulate verbose output with short test summary where the same
        // fully-qualified test name appears both as a verbose FAILED line
        // and in the "short test summary info" section.
        let input = "\
tests/test_a.py::test_one PASSED
tests/test_b.py::test_two FAILED
=========================== short test summary info ============================
FAILED tests/test_b.py::test_two - assert 1 == 2
========================= 1 passed, 1 failed in 0.10s =========================";

        let result = parse(input);
        if let ParseResult::Full(tr) = &result {
            // tests/test_b.py::test_two should appear exactly once despite
            // being in both the verbose output and the short summary.
            let fail_entries: Vec<_> = tr
                .entries
                .iter()
                .filter(|e| e.outcome == TestOutcome::Fail)
                .collect();
            assert_eq!(
                fail_entries.len(),
                1,
                "test_two should be deduplicated to a single entry, got {}",
                fail_entries.len()
            );
            // The first occurrence (from verbose line) should be kept
            assert_eq!(fail_entries[0].name, "tests/test_b.py::test_two");
        }
    }

    // ========================================================================
    // parse_verbose_marker unit tests
    // ========================================================================

    #[test]
    fn test_parse_verbose_marker_passed() {
        let (name, outcome) =
            parse_verbose_marker("tests/test_a.py::test_one PASSED").expect("should match PASSED");
        assert_eq!(name, "tests/test_a.py::test_one");
        assert_eq!(outcome, TestOutcome::Pass);
    }

    #[test]
    fn test_parse_verbose_marker_failed() {
        let (name, outcome) =
            parse_verbose_marker("tests/test_b.py::test_two FAILED").expect("should match FAILED");
        assert_eq!(name, "tests/test_b.py::test_two");
        assert_eq!(outcome, TestOutcome::Fail);
    }

    #[test]
    fn test_parse_verbose_marker_skipped() {
        let (name, outcome) = parse_verbose_marker("tests/test_c.py::test_three SKIPPED")
            .expect("should match SKIPPED");
        assert_eq!(name, "tests/test_c.py::test_three");
        assert_eq!(outcome, TestOutcome::Skip);
    }

    #[test]
    fn test_parse_verbose_marker_no_suffix_returns_none() {
        assert!(
            parse_verbose_marker("tests/test_a.py::test_one").is_none(),
            "line with no outcome marker should return None"
        );
    }

    #[test]
    fn test_parse_verbose_marker_pathological_name_containing_marker_word() {
        // A test whose name itself contains "FAILED" should still parse correctly
        // as long as the line ends with the real suffix.
        let (name, outcome) =
            parse_verbose_marker("tests/test_FAILED_case.py::test_x PASSED")
                .expect("should match PASSED suffix");
        assert_eq!(name, "tests/test_FAILED_case.py::test_x");
        assert_eq!(outcome, TestOutcome::Pass);
    }

    // ========================================================================
    // detect_section_header unit tests
    // ========================================================================

    #[test]
    fn test_detect_section_header_failures() {
        assert_eq!(
            detect_section_header("=== FAILURES ==="),
            Some(PytestSection::Failures)
        );
    }

    #[test]
    fn test_detect_section_header_summary_info() {
        assert_eq!(
            detect_section_header("=== short test summary info ==="),
            Some(PytestSection::SummaryInfo)
        );
    }

    #[test]
    fn test_detect_section_header_normal_catch_all() {
        assert_eq!(
            detect_section_header("=== warnings summary ==="),
            Some(PytestSection::Normal)
        );
    }

    #[test]
    fn test_detect_section_header_non_header_returns_none() {
        assert!(
            detect_section_header("some regular output line").is_none(),
            "non-header line should return None"
        );
    }

    #[test]
    fn test_detect_section_header_incomplete_banner_returns_none() {
        // A line that starts with === but does not end with === and is neither
        // FAILURES nor "short test summary info" — treat as non-header.
        assert!(
            detect_section_header("=== warnings summary").is_none(),
            "=== prefix without === suffix should return None"
        );
    }
}
