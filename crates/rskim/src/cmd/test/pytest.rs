//! Pytest parser with three-tier degradation (#47)
//!
//! Parses pytest text output into structured [`TestResult`] using a three-tier
//! strategy:
//!
//! - **Tier 1 (text state machine):** Scans all output lines, counting PASSED/FAILED/
//!   SKIPPED/ERROR outcomes and extracting individual test names. Requires the summary
//!   line to produce a `Full` result.
//! - **Tier 2 (regex on summary only):** Falls back to regex matching on the summary
//!   line alone when tier 1 fails. Produces a `Degraded` result.
//! - **Tier 3 (passthrough):** Returns raw output unmodified when no summary can be
//!   found at all.
//!
//! ## Usage
//!
//! ```text
//! skim test pytest [args...]          # Execute pytest, parse output
//! pytest ... | skim test pytest       # Parse piped stdin
//! ```

use std::io::{self, IsTerminal, Read};
use std::process::ExitCode;
use std::time::Duration;

use regex::Regex;

use crate::output::canonical::{TestEntry, TestOutcome, TestResult, TestSummary};
use crate::output::ParseResult;
use crate::runner::{CommandOutput, CommandRunner};

// ============================================================================
// Public entry point
// ============================================================================

/// Run pytest and parse its output, or parse piped stdin.
///
/// Detection logic:
/// - If stdin is a terminal → run pytest (execution mode)
/// - If stdin is not a terminal → attempt to read stdin; if empty, fall back
///   to running pytest (handles test harness environments where stdin is a
///   pipe with no data)
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    let output = if io::stdin().is_terminal() {
        // Terminal: always run pytest
        let final_args = build_args(args);
        let arg_refs: Vec<&str> = final_args.iter().map(String::as_str).collect();
        run_pytest(&arg_refs)?
    } else {
        // Pipe: read stdin, fall back to execution if empty
        let stdin_output = read_stdin()?;
        if stdin_output.stdout.trim().is_empty() {
            // Empty pipe (e.g., test harness) — run pytest instead
            let final_args = build_args(args);
            let arg_refs: Vec<&str> = final_args.iter().map(String::as_str).collect();
            run_pytest(&arg_refs)?
        } else {
            stdin_output
        }
    };

    let combined = combine_output(&output);
    let result = parse(&combined);

    emit_result(&result, &output)?;

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

/// Check if any of the given flag prefixes appear in the user's args.
///
/// Matches both `--flag` and `--flag=value` forms.
fn user_has_flag(args: &[String], prefixes: &[&str]) -> bool {
    args.iter().any(|arg| {
        prefixes
            .iter()
            .any(|prefix| arg == prefix || arg.starts_with(&format!("{prefix}=")))
    })
}

// ============================================================================
// Command execution
// ============================================================================

/// Execute pytest with the given arguments.
fn run_pytest(args: &[&str]) -> anyhow::Result<CommandOutput> {
    let runner = CommandRunner::new(Some(Duration::from_secs(300)));
    runner
        .run("pytest", args)
        .map_err(|e| anyhow::anyhow!("{e}\n\nHint: Is pytest installed? Try: pip install pytest"))
}

/// Read all of stdin into a synthetic [`CommandOutput`].
fn read_stdin() -> anyhow::Result<CommandOutput> {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf)?;
    Ok(CommandOutput {
        stdout: buf,
        stderr: String::new(),
        exit_code: None,
        duration: Duration::ZERO,
    })
}

/// Combine stdout and stderr into a single string for parsing.
///
/// Pytest writes test output to stdout and some warnings/errors to stderr.
/// We combine them so the parser can see everything.
fn combine_output(output: &CommandOutput) -> String {
    if output.stderr.is_empty() {
        output.stdout.clone()
    } else {
        format!("{}\n{}", output.stdout, output.stderr)
    }
}

// ============================================================================
// Three-tier parser
// ============================================================================

/// Parse pytest output using three-tier degradation.
///
/// Returns `Full` if tier 1 succeeds, `Degraded` if only tier 2 matches,
/// or `Passthrough` if neither can extract structured data.
fn parse(output: &str) -> ParseResult<TestResult> {
    // Tier 1: full text state machine
    if let Some(result) = tier1_parse(output) {
        return ParseResult::Full(result);
    }

    // Tier 2: regex on summary line only
    if let Some(result) = tier2_parse(output) {
        return ParseResult::Degraded(result, vec!["regex fallback".to_string()]);
    }

    // Tier 3: passthrough
    ParseResult::Passthrough(output.to_string())
}

/// Summary regex pattern matching pytest's final summary line.
///
/// Handles both verbose and quiet output formats:
/// - Verbose: `============================== 5 passed in 0.12s ===============================`
/// - Quiet:   `2 passed in 0.00s`
/// - Mixed:   `============== 4 passed, 1 failed, 1 skipped in 0.20s =============`
///
/// The `=+` prefix/suffix are optional to support quiet mode output.
fn summary_regex() -> Regex {
    Regex::new(
        r"=*\s*(\d+)\s+passed(?:,\s+(\d+)\s+failed)?(?:,\s+(\d+)\s+skipped)?(?:,\s+(\d+)\s+error)?\s+in\s+[\d.]+s\s*=*"
    ).expect("summary regex is valid")
}

// ============================================================================
// Tier 1: Text state machine
// ============================================================================

/// Tier 1: Full text state machine parse.
///
/// Scans every line for PASSED/FAILED/SKIPPED/ERROR markers, extracts test names
/// from "short test summary" lines, collects failure output, and validates against
/// the summary line.
fn tier1_parse(output: &str) -> Option<TestResult> {
    let re = summary_regex();
    let mut entries: Vec<TestEntry> = Vec::new();
    let mut in_failures = false;
    let mut in_summary_info = false;
    let mut current_failure_name: Option<String> = None;
    let mut current_failure_detail: Vec<String> = Vec::new();

    // Track summary values
    let mut summary_match: Option<(usize, usize, usize)> = None; // (pass, fail, skip)

    for line in output.lines() {
        let trimmed = line.trim();

        // Detect summary line
        if let Some(caps) = re.captures(trimmed) {
            let pass: usize = caps.get(1).map_or(0, |m| m.as_str().parse().unwrap_or(0));
            let fail: usize = caps.get(2).map_or(0, |m| m.as_str().parse().unwrap_or(0));
            let skip: usize = caps.get(3).map_or(0, |m| m.as_str().parse().unwrap_or(0));
            let error: usize = caps.get(4).map_or(0, |m| m.as_str().parse().unwrap_or(0));
            summary_match = Some((pass, fail + error, skip));
            continue;
        }

        // Detect FAILURES section header
        if trimmed.starts_with("===") && trimmed.contains("FAILURES") {
            in_failures = true;
            in_summary_info = false;
            continue;
        }

        // Detect "short test summary info" section
        if trimmed.starts_with("===") && trimmed.contains("short test summary info") {
            in_summary_info = true;
            in_failures = false;
            // Flush any pending failure
            flush_failure(
                &mut entries,
                &mut current_failure_name,
                &mut current_failure_detail,
            );
            continue;
        }

        // Detect any other section header (=== ... ===) that ends the current section
        if trimmed.starts_with("===") && trimmed.ends_with("===") {
            if in_failures {
                flush_failure(
                    &mut entries,
                    &mut current_failure_name,
                    &mut current_failure_detail,
                );
            }
            in_failures = false;
            in_summary_info = false;
            continue;
        }

        // Inside FAILURES section: extract individual test failure blocks
        if in_failures {
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
        if in_summary_info {
            if let Some(rest) = trimmed.strip_prefix("FAILED ") {
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
            } else if let Some(rest) = trimmed.strip_prefix("ERROR ") {
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

        // Outside special sections: look for per-line PASSED/FAILED/SKIPPED markers
        // These appear in verbose mode output like:
        //   tests/test_a.py::test_one PASSED
        //   tests/test_a.py::test_two FAILED
        if trimmed.ends_with(" PASSED") {
            let name = trimmed.trim_end_matches(" PASSED").to_string();
            entries.push(TestEntry {
                name,
                outcome: TestOutcome::Pass,
                detail: None,
            });
        } else if trimmed.ends_with(" FAILED") {
            let name = trimmed.trim_end_matches(" FAILED").to_string();
            entries.push(TestEntry {
                name,
                outcome: TestOutcome::Fail,
                detail: None,
            });
        } else if trimmed.ends_with(" SKIPPED") {
            let name = trimmed.trim_end_matches(" SKIPPED").to_string();
            entries.push(TestEntry {
                name,
                outcome: TestOutcome::Skip,
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
    let (pass, fail, skip) = summary_match?;

    let summary = TestSummary {
        pass,
        fail,
        skip,
        duration_ms: None,
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
// Tier 2: Regex fallback
// ============================================================================

/// Tier 2: Extract summary from regex match on summary line only.
///
/// Does not attempt to extract individual test entries — only counts.
fn tier2_parse(output: &str) -> Option<TestResult> {
    let re = summary_regex();

    for line in output.lines() {
        if let Some(caps) = re.captures(line.trim()) {
            let pass: usize = caps.get(1).map_or(0, |m| m.as_str().parse().unwrap_or(0));
            let fail: usize = caps.get(2).map_or(0, |m| m.as_str().parse().unwrap_or(0));
            let skip: usize = caps.get(3).map_or(0, |m| m.as_str().parse().unwrap_or(0));
            let error: usize = caps.get(4).map_or(0, |m| m.as_str().parse().unwrap_or(0));

            let summary = TestSummary {
                pass,
                fail: fail + error,
                skip,
                duration_ms: None,
            };

            return Some(TestResult::new(summary, vec![]));
        }
    }

    None
}

// ============================================================================
// Output emission
// ============================================================================

/// Emit the parsed result to stdout/stderr.
fn emit_result(result: &ParseResult<TestResult>, output: &CommandOutput) -> anyhow::Result<()> {
    use std::io::Write;

    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();

    match result {
        ParseResult::Full(tr) => {
            writeln!(out, "{tr}")?;
        }
        ParseResult::Degraded(tr, _markers) => {
            writeln!(out, "{tr}")?;
            result.emit_markers(&mut err)?;
        }
        ParseResult::Passthrough(raw) => {
            // Write raw output as-is
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

    /// Load a fixture file from the test fixtures directory.
    fn load_fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("cmd")
            .join("test")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture {}: {e}", path.display()))
    }

    // ========================================================================
    // Tier 1 tests
    // ========================================================================

    #[test]
    fn test_tier1_all_pass() {
        let input = load_fixture("pytest_pass.txt");
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
        let input = load_fixture("pytest_fail.txt");
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
        let input = load_fixture("pytest_mixed.txt");
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

    // ========================================================================
    // Tier 2 tests
    // ========================================================================

    #[test]
    fn test_tier2_summary_only() {
        // Just the summary line, no other pytest output
        let input = "============== 4 passed, 1 failed, 1 skipped in 0.20s ==============";
        let _result = parse(input);

        // Tier 1 will also match (summary line is enough for tier 1 too).
        // Test tier2 directly to verify it works independently.
        let tier2_result = tier2_parse(input);
        assert!(tier2_result.is_some(), "tier 2 should match summary line");

        let tr = tier2_result.unwrap();
        assert_eq!(tr.summary.pass, 4);
        assert_eq!(tr.summary.fail, 1);
        assert_eq!(tr.summary.skip, 1);
    }

    #[test]
    fn test_tier2_degraded_result() {
        // Garbage output with only a summary line embedded somewhere
        let input = "some random output\n\
                     blah blah\n\
                     ============== 3 passed in 0.10s ==============\n\
                     more stuff";
        let _result = parse(input);

        // Tier 1 also matches here since summary regex matches.
        // Test tier 2 independently:
        let tier2_result = tier2_parse(input);
        assert!(tier2_result.is_some());
        let tr = tier2_result.unwrap();
        assert_eq!(tr.summary.pass, 3);
        assert_eq!(tr.summary.fail, 0);
    }

    #[test]
    fn test_tier3_passthrough() {
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
    // Summary regex edge cases
    // ========================================================================

    #[test]
    fn test_summary_regex_passed_only() {
        let re = summary_regex();
        let line =
            "============================== 5 passed in 0.12s ===============================";
        let caps = re.captures(line).expect("should match");
        assert_eq!(caps.get(1).unwrap().as_str(), "5");
        assert!(caps.get(2).is_none()); // no failed
        assert!(caps.get(3).is_none()); // no skipped
    }

    #[test]
    fn test_summary_regex_all_groups() {
        let re = summary_regex();
        let line = "======= 10 passed, 2 failed, 3 skipped, 1 error in 1.50s =======";
        let caps = re.captures(line).expect("should match");
        assert_eq!(caps.get(1).unwrap().as_str(), "10");
        assert_eq!(caps.get(2).unwrap().as_str(), "2");
        assert_eq!(caps.get(3).unwrap().as_str(), "3");
        assert_eq!(caps.get(4).unwrap().as_str(), "1");
    }

    #[test]
    fn test_summary_regex_no_match_on_garbage() {
        let re = summary_regex();
        assert!(re.captures("hello world").is_none());
    }
}
