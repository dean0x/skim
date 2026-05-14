//! Swift Package Manager / XCTest parser with three-tier degradation (#118).
//!
//! Parses `swift test` output into structured `TestResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Regex on XCTest format — no JSON reporter in SPM context
//! - **Tier 2 (Degraded)**: `scrape_failures` for failing test names
//! - **Tier 3 (Passthrough)**: Returns raw output unchanged

use std::io;
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::{TestEntry, TestOutcome, TestResult, TestSummary};
use crate::runner::CommandRunner;

use super::shared::{self, try_read_stdin};

// ============================================================================
// Regex patterns
// ============================================================================

/// XCTest passed: `Test Case '-[ClassName methodName]' passed (N.NNN seconds).`
static RE_XCTEST_PASS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Test Case '(.+)' passed \(\d+\.\d+ seconds\)\.$").expect("valid regex")
});

/// XCTest failed: `Test Case '-[ClassName methodName]' failed (N.NNN seconds).`
static RE_XCTEST_FAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Test Case '(.+)' failed \(\d+\.\d+ seconds\)\.$").expect("valid regex")
});

/// SPM test format: `Test Case 'ClassName.methodName' passed.`
static RE_SPM_PASS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Test Case '(\S+\.\S+)' passed").expect("valid regex")
});

/// SPM test format: `Test Case 'ClassName.methodName' failed`
static RE_SPM_FAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Test Case '(\S+\.\S+)' failed").expect("valid regex")
});

/// Summary: `Executed N tests, with N failures (N unexpected) in N.NNN (N.NNN) seconds`
static RE_XCTEST_SUMMARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Executed (\d+) tests?, with (\d+) failure").expect("valid regex")
});

// ============================================================================
// Public entry point
// ============================================================================

/// Run `swift test [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    // Passthrough mode
    if crate::cmd::is_passthrough_mode() {
        return shared::run_passthrough(
            args,
            |a| a.to_vec(),
            |arg_refs| {
                CommandRunner::new(Some(crate::cmd::DEFAULT_CMD_TIMEOUT))
                    .run("swift", arg_refs)
            },
        );
    }

    let start = std::time::Instant::now();

    // Prepend "test" since we're always called with the test subcommand
    let mut full_args = vec!["test".to_string()];
    full_args.extend_from_slice(args);

    let raw_output = if let Some(stdin_content) = try_read_stdin(args)? {
        stdin_content
    } else {
        run_swift_test(&full_args)?
    };

    let result = parse(&raw_output);

    let exit_code = match &result {
        ParseResult::Full(test_result) | ParseResult::Degraded(test_result, _) => {
            println!("{test_result}");
            let stderr = io::stderr();
            let mut handle = stderr.lock();
            let _ = result.emit_markers(&mut handle);

            if test_result.summary.fail > 0 {
                shared::emit_failure_context(&raw_output, 1);
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
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
        crate::cmd::format_analytics_label("test", "swift", &args.join(" ")),
        start.elapsed(),
    );

    Ok(exit_code)
}

fn run_swift_test(args: &[String]) -> anyhow::Result<String> {
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let runner = CommandRunner::new(Some(crate::cmd::DEFAULT_CMD_TIMEOUT));
    let output = runner.run("swift", &arg_refs).map_err(|e| {
        anyhow::anyhow!(
            "failed to run swift: {e}\n\
             Hint: Install Swift from https://swift.org/download/"
        )
    })?;

    let mut combined = output.stdout;
    if !output.stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&output.stderr);
    }

    Ok(combined)
}

// ============================================================================
// Three-tier parser
// ============================================================================

fn parse(raw: &str) -> ParseResult<TestResult> {
    // Tier 1: XCTest/SPM regex (no JSON reporter available)
    if let Some(result) = try_parse_xctest(raw) {
        return ParseResult::Full(result);
    }

    // Tier 2: Scrape failure names only
    if let Some(result) = try_parse_failures_only(raw) {
        return ParseResult::Degraded(
            result,
            vec!["swift: no summary found, scraping failure names".to_string()],
        );
    }

    ParseResult::Passthrough(raw.to_string())
}

fn try_parse_xctest(raw: &str) -> Option<TestResult> {
    let cleaned = crate::output::strip_ansi(raw);

    let mut entries: Vec<TestEntry> = Vec::new();

    // Collect all pass/fail from per-test lines
    for line in cleaned.lines() {
        if let Some(caps) = RE_XCTEST_PASS.captures(line).or_else(|| RE_SPM_PASS.captures(line)) {
            entries.push(TestEntry {
                name: caps[1].to_string(),
                outcome: TestOutcome::Pass,
                detail: None,
            });
        } else if let Some(caps) = RE_XCTEST_FAIL.captures(line).or_else(|| RE_SPM_FAIL.captures(line)) {
            entries.push(TestEntry {
                name: caps[1].to_string(),
                outcome: TestOutcome::Fail,
                detail: None,
            });
        }
    }

    // Parse summary
    let summary_caps = RE_XCTEST_SUMMARY.captures(&cleaned)?;
    let total: usize = summary_caps[1].parse().ok()?;
    let failures: usize = summary_caps[2].parse().ok()?;
    let passed = total.saturating_sub(failures);

    let summary = TestSummary {
        pass: passed,
        fail: failures,
        skip: 0,
        duration_ms: None,
    };

    Some(TestResult::new(summary, entries))
}

fn try_parse_failures_only(raw: &str) -> Option<TestResult> {
    let cleaned = crate::output::strip_ansi(raw);
    let mut entries: Vec<TestEntry> = Vec::new();

    for line in cleaned.lines() {
        if let Some(caps) = RE_XCTEST_FAIL.captures(line).or_else(|| RE_SPM_FAIL.captures(line)) {
            entries.push(TestEntry {
                name: caps[1].to_string(),
                outcome: TestOutcome::Fail,
                detail: None,
            });
        }
    }

    if entries.is_empty() {
        return None;
    }

    let fail = entries.len();
    let summary = TestSummary {
        pass: 0,
        fail,
        skip: 0,
        duration_ms: None,
    };
    Some(TestResult::new(summary, entries))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const SWIFT_PASS: &str = "Test Case '-[MyAppTests.UserTests testCreateUser]' passed (0.003 seconds).\nTest Case '-[MyAppTests.UserTests testDeleteUser]' passed (0.002 seconds).\nExecuted 2 tests, with 0 failures (0 unexpected) in 0.005 (0.010) seconds\n";

    const SWIFT_FAIL: &str = "Test Case '-[MyAppTests.UserTests testCreateUser]' passed (0.003 seconds).\nTest Case '-[MyAppTests.UserTests testDeleteUser]' failed (0.008 seconds).\n/src/UserTests.swift:42: error: testDeleteUser : XCTAssertEqual failed: (\"0\") is not equal to (\"1\")\nExecuted 2 tests, with 1 failure (1 unexpected) in 0.011 (0.020) seconds\n";

    const SWIFT_SPM: &str = "Test Case 'MyApp.testBasicOperation' passed.\nTest Case 'MyApp.testEdgeCase' failed.\nExecuted 2 tests, with 1 failure (1 unexpected) in 0.003 (0.003) seconds\n";

    #[test]
    fn test_swift_tier1_pass() {
        let result = try_parse_xctest(SWIFT_PASS);
        assert!(result.is_some(), "Expected XCTest parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.summary.fail, 0);
        assert_eq!(r.summary.pass, 2);
    }

    #[test]
    fn test_swift_tier1_fail() {
        let result = try_parse_xctest(SWIFT_FAIL);
        assert!(result.is_some(), "Expected XCTest parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.summary.fail, 1);
        assert_eq!(r.summary.pass, 1);
        let failed = r.entries.iter().find(|e| e.outcome == TestOutcome::Fail);
        assert!(failed.is_some());
        assert!(failed.unwrap().name.contains("testDeleteUser"));
    }

    #[test]
    fn test_swift_spm_format() {
        let result = try_parse_xctest(SWIFT_SPM);
        assert!(result.is_some(), "Expected SPM parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.summary.fail, 1);
        assert_eq!(r.summary.pass, 1);
    }

    #[test]
    fn test_swift_tier3_passthrough() {
        let result = parse("completely unparseable output");
        assert!(result.is_passthrough(), "Expected Passthrough, got {}", result.tier_name());
    }

    #[test]
    fn test_swift_parse_full_on_xctest() {
        let result = parse(SWIFT_PASS);
        assert!(result.is_full(), "Expected Full, got {}", result.tier_name());
    }
}
