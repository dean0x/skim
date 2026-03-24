//! Vitest/Jest parser with three-tier degradation (#48)
//!
//! Parses vitest (and Jest-compatible) JSON output into structured `TestResult`.
//! Supports three degradation tiers:
//!
//! - **Tier 1 (JSON)**: Full JSON parse with brace-balance extraction to handle
//!   pnpm/dotenv prefix noise before the JSON payload.
//! - **Tier 2 (regex)**: Falls back to regex matching on summary lines when JSON
//!   parsing fails.
//! - **Tier 3 (passthrough)**: Returns raw output unchanged when nothing parses.

use std::io::{self, IsTerminal, Read};
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{TestEntry, TestOutcome, TestResult, TestSummary};
use crate::output::ParseResult;
use crate::runner::CommandRunner;

// ============================================================================
// Public entry point
// ============================================================================

/// Run vitest/jest with the given args, or read piped stdin, and parse the output.
///
/// `program` is the runner binary name (e.g. `"vitest"` or `"jest"`), used when
/// stdin is not piped and we need to spawn the process directly.
pub(crate) fn run(program: &str, args: &[String], show_stats: bool) -> anyhow::Result<ExitCode> {
    let raw_output = if stdin_has_data() {
        read_stdin()?
    } else {
        run_vitest(program, args)?
    };

    let result = parse(&raw_output);

    // Emit the result to stdout
    let exit_code = match &result {
        ParseResult::Full(test_result) | ParseResult::Degraded(test_result, _) => {
            println!("{test_result}");
            // Emit degradation markers to stderr
            let stderr = io::stderr();
            let mut handle = stderr.lock();
            let _ = result.emit_markers(&mut handle);

            if test_result.summary.fail > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        ParseResult::Passthrough(raw) => {
            println!("{raw}");
            let stderr = io::stderr();
            let mut handle = stderr.lock();
            let _ = result.emit_markers(&mut handle);
            ExitCode::FAILURE
        }
    };

    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(&raw_output, result.content());
        crate::process::report_token_stats(orig, comp, "");
    }

    // Record analytics (fire-and-forget, non-blocking)
    if crate::analytics::is_analytics_enabled() {
        let cwd = std::env::current_dir()
            .unwrap_or_default()
            .display()
            .to_string();
        crate::analytics::record_fire_and_forget(
            raw_output,
            result.content().to_string(),
            format!("skim test {program} {}", args.join(" ")),
            crate::analytics::CommandType::Test,
            std::time::Duration::ZERO,
            cwd,
            Some(result.tier_name().to_string()),
        );
    }

    Ok(exit_code)
}

// ============================================================================
// Command execution
// ============================================================================

/// Check whether stdin has piped data (not a terminal).
fn stdin_has_data() -> bool {
    !io::stdin().is_terminal()
}

/// Maximum bytes we will read from stdin (64 MiB).
///
/// Mirrors the `MAX_OUTPUT_BYTES` limit in `runner.rs` to prevent unbounded
/// memory growth when a large file is accidentally piped in.
const MAX_STDIN_BYTES: usize = 64 * 1024 * 1024;

/// Read stdin into a String, capped at [`MAX_STDIN_BYTES`].
///
/// Uses chunked reads (8 KiB) instead of `read_to_string` to enforce the size
/// limit incrementally. Non-UTF-8 input is handled via `String::from_utf8_lossy`.
fn read_stdin() -> anyhow::Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8 * 1024];
    let stdin = io::stdin();
    let mut handle = stdin.lock();
    loop {
        let n = handle.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        if buf.len() + n > MAX_STDIN_BYTES {
            anyhow::bail!("stdin exceeded {} byte limit", MAX_STDIN_BYTES);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Execute the test runner with the user's args, injecting `--reporter=json` if
/// not already set.
///
/// `program` is the binary to invoke (e.g. `"vitest"` or `"jest"`).
fn run_vitest(program: &str, args: &[String]) -> anyhow::Result<String> {
    let mut final_args: Vec<String> = args.to_vec();

    if program == "jest" {
        if !user_has_flag(args, &["--json"]) {
            final_args.push("--json".to_string());
        }
    } else if !user_has_flag(args, &["--reporter"]) {
        final_args.push("--reporter=json".to_string());
    }

    let arg_refs: Vec<&str> = final_args.iter().map(|s| s.as_str()).collect();

    let runner = CommandRunner::new(None);
    let output = runner.run(program, &arg_refs).map_err(|e| {
        anyhow::anyhow!(
            "failed to run {program}: {e}\n\
             Hint: Install {program} with: npm install -D {program}"
        )
    })?;

    // Combine stdout and stderr — vitest may emit JSON to either depending on
    // version and configuration.
    let mut combined = output.stdout;
    if !output.stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&output.stderr);
    }

    Ok(combined)
}

// user_has_flag is imported from crate::cmd

// ============================================================================
// Three-tier parser
// ============================================================================

/// Parse vitest/jest output through three degradation tiers.
fn parse(raw: &str) -> ParseResult<TestResult> {
    // Tier 1: Try JSON parse
    if let Some(result) = try_parse_json(raw) {
        return ParseResult::Full(result);
    }

    // Tier 2: Try regex fallback
    if let Some(result) = try_parse_regex(raw) {
        return ParseResult::Degraded(result, vec!["regex fallback".to_string()]);
    }

    // Tier 3: Passthrough
    ParseResult::Passthrough(raw.to_string())
}

// ============================================================================
// Tier 1: JSON parsing with brace-balance extraction
// ============================================================================

/// Vitest JSON output structure (subset we care about).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct VitestJson {
    #[serde(default)]
    num_passed_tests: usize,
    #[serde(default)]
    num_failed_tests: usize,
    #[serde(default)]
    num_pending_tests: usize,
    #[serde(default)]
    test_results: Vec<VitestTestSuite>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct VitestTestSuite {
    #[serde(default)]
    assertion_results: Vec<VitestAssertion>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct VitestAssertion {
    status: String,
    #[serde(default)]
    full_name: String,
    #[serde(default)]
    failure_messages: Vec<String>,
}

/// Try to extract and parse JSON from the raw output.
///
/// Handles pnpm workspace resolution lines, dotenv loading messages, and other
/// prefix noise by using brace-balance counting to find the outermost JSON object.
/// If the first balanced `{...}` candidate is not valid Vitest JSON (e.g., a
/// `{project}` tag in a pnpm log), subsequent candidates are tried.
fn try_parse_json(raw: &str) -> Option<TestResult> {
    let cleaned = crate::output::strip_ansi(raw);

    // Iterate over brace-balanced candidates until one parses as Vitest JSON.
    let mut search_from = 0;
    let parsed: VitestJson = loop {
        let json_str = extract_json_by_brace_balance_from(&cleaned, search_from)?;
        if let Ok(v) = serde_json::from_str::<VitestJson>(json_str) {
            break v;
        }
        // Advance past this candidate's starting `{` and try the next one.
        let candidate_start = cleaned[search_from..]
            .find(json_str)
            .map(|off| search_from + off)
            .unwrap_or(search_from);
        search_from = candidate_start + 1;
    };

    let mut entries = Vec::new();
    for suite in &parsed.test_results {
        for assertion in &suite.assertion_results {
            let outcome = match assertion.status.as_str() {
                "passed" => TestOutcome::Pass,
                "failed" => TestOutcome::Fail,
                "pending" | "skipped" | "todo" => TestOutcome::Skip,
                _ => TestOutcome::Skip,
            };

            let detail = if assertion.failure_messages.is_empty() {
                None
            } else {
                Some(assertion.failure_messages.join("\n"))
            };

            entries.push(TestEntry {
                name: assertion.full_name.clone(),
                outcome,
                detail,
            });
        }
    }

    let summary = TestSummary {
        pass: parsed.num_passed_tests,
        fail: parsed.num_failed_tests,
        skip: parsed.num_pending_tests,
        duration_ms: None,
    };

    Some(TestResult::new(summary, entries))
}

/// Extract a brace-balanced JSON object from the input, starting the scan at
/// byte offset `search_from`.
///
/// Scans for each `{` from `search_from` onward, then counts braces (respecting
/// string literals to avoid being confused by `{` and `}` inside quoted strings)
/// to find the matching closing `}`. If a `{` does not produce a balanced pair,
/// scanning continues from the next `{` candidate.
fn extract_json_by_brace_balance_from(input: &str, search_from: usize) -> Option<&str> {
    let bytes = input.as_bytes();
    let mut pos = search_from;

    while pos < bytes.len() {
        let start = pos + bytes[pos..].iter().position(|&b| b == b'{')?;

        if let Some(end) = find_balanced_end(bytes, start) {
            return Some(&input[start..=end]);
        }

        // This `{` didn't balance -- advance past it and try the next one.
        pos = start + 1;
    }

    None
}

/// Convenience wrapper: extract from the beginning of the input.
#[cfg(test)]
fn extract_json_by_brace_balance(input: &str) -> Option<&str> {
    extract_json_by_brace_balance_from(input, 0)
}

/// Starting from `bytes[start]` (which must be `b'{'`), scan forward using
/// brace-balance counting with string-literal awareness. Returns the index of
/// the matching `}` if found, or `None` if the braces never balance.
fn find_balanced_end(bytes: &[u8], start: usize) -> Option<usize> {
    debug_assert_eq!(bytes[start], b'{');

    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escape_next = false;
    let mut i = start;

    while i < bytes.len() {
        let b = bytes[i];

        if escape_next {
            escape_next = false;
            i += 1;
            continue;
        }

        if in_string {
            match b {
                b'\\' => escape_next = true,
                b'"' => in_string = false,
                _ => {}
            }
            i += 1;
            continue;
        }

        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }

        i += 1;
    }

    None
}

// ============================================================================
// Tier 2: Regex fallback
// ============================================================================

/// Vitest pipe-format summary regex (compiled once).
///
/// Matches: "Tests  3 passed | 0 failed | 3 total"
static PIPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Tests\s+(\d+)\s+passed\s*\|\s*(\d+)\s+failed\s*\|\s*(\d+)\s+total")
        .expect("PIPE_RE is a valid regex")
});

/// Jest comma-format summary regex (compiled once).
///
/// Matches: "Tests: 5 passed, 2 failed, 7 total"
static COMMA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Tests:\s+(\d+)\s+passed(?:,\s+(\d+)\s+failed)?(?:,\s+(\d+)\s+total)?")
        .expect("COMMA_RE is a valid regex")
});

/// Try to parse test summary from plain text output using regex patterns.
fn try_parse_regex(raw: &str) -> Option<TestResult> {
    let cleaned = crate::output::strip_ansi(raw);

    // Pattern 1: "Tests  3 passed | 0 failed | 3 total"
    if let Some(caps) = PIPE_RE.captures(&cleaned) {
        let pass: usize = caps[1].parse().ok()?;
        let fail: usize = caps[2].parse().ok()?;
        let total: usize = caps[3].parse().ok()?;
        let skip = total.saturating_sub(pass + fail);

        let summary = TestSummary {
            pass,
            fail,
            skip,
            duration_ms: None,
        };
        return Some(TestResult::new(summary, vec![]));
    }

    // Pattern 2: "Tests:\s+N passed(?:,\s+N failed)?(?:,\s+N total)?"
    if let Some(caps) = COMMA_RE.captures(&cleaned) {
        let pass: usize = caps[1].parse().ok()?;
        let fail: usize = caps
            .get(2)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);
        let total: usize = caps
            .get(3)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(pass + fail);
        let skip = total.saturating_sub(pass + fail);

        let summary = TestSummary {
            pass,
            fail,
            skip,
            duration_ms: None,
        };
        return Some(TestResult::new(summary, vec![]));
    }

    None
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests");
        path.push("fixtures");
        path.push("vitest");
        path.push(name);
        path
    }

    fn read_fixture(name: &str) -> String {
        std::fs::read_to_string(fixture_path(name))
            .unwrap_or_else(|e| panic!("Failed to read fixture {name}: {e}"))
    }

    // ========================================================================
    // Tier 1: JSON parsing tests
    // ========================================================================

    #[test]
    fn test_tier1_clean_json() {
        let input = read_fixture("vitest_pass.json");
        let result = parse(&input);

        assert!(
            result.is_full(),
            "expected Full, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(test_result) = result {
            assert_eq!(test_result.summary.pass, 3);
            assert_eq!(test_result.summary.fail, 0);
            assert_eq!(test_result.summary.skip, 0);
            assert_eq!(test_result.entries.len(), 3);
        }
    }

    #[test]
    fn test_tier1_with_failures() {
        let input = read_fixture("vitest_fail.json");
        let result = parse(&input);

        assert!(
            result.is_full(),
            "expected Full, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(test_result) = result {
            assert_eq!(test_result.summary.pass, 1);
            assert_eq!(test_result.summary.fail, 1);
            assert_eq!(test_result.summary.skip, 1);

            // Check failure details
            let failed_entry = test_result
                .entries
                .iter()
                .find(|e| e.outcome == TestOutcome::Fail)
                .expect("should have a failed entry");
            assert_eq!(failed_entry.name, "math > divides");
            assert!(
                failed_entry
                    .detail
                    .as_ref()
                    .unwrap()
                    .contains("Expected 0, received Infinity"),
                "failure detail should contain the error message"
            );

            // Check skip/pending
            let skipped_entry = test_result
                .entries
                .iter()
                .find(|e| e.outcome == TestOutcome::Skip)
                .expect("should have a skipped entry");
            assert_eq!(skipped_entry.name, "math > todo test");
        }
    }

    #[test]
    fn test_tier1_pnpm_prefix_noise() {
        let input = read_fixture("vitest_pnpm_prefix.json");
        let result = parse(&input);

        assert!(
            result.is_full(),
            "expected Full despite pnpm prefix noise, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(test_result) = result {
            assert_eq!(test_result.summary.pass, 2);
            assert_eq!(test_result.summary.fail, 0);
        }
    }

    // ========================================================================
    // Brace balance extraction tests
    // ========================================================================

    #[test]
    fn test_brace_balance_nested_objects() {
        let input = r#"noise {"outer": {"inner": {"deep": 1}}, "key": "val"} trailing"#;
        let extracted = extract_json_by_brace_balance(input).unwrap();
        assert_eq!(
            extracted,
            r#"{"outer": {"inner": {"deep": 1}}, "key": "val"}"#
        );

        // Verify it's valid JSON
        let parsed: serde_json::Value = serde_json::from_str(extracted).unwrap();
        assert!(parsed.is_object());
    }

    #[test]
    fn test_brace_balance_string_with_braces() {
        let input = r#"prefix {"value": "has { and } chars", "num": 42} suffix"#;
        let extracted = extract_json_by_brace_balance(input).unwrap();
        assert_eq!(extracted, r#"{"value": "has { and } chars", "num": 42}"#);

        // Verify it's valid JSON
        let parsed: serde_json::Value = serde_json::from_str(extracted).unwrap();
        assert_eq!(parsed["num"], 42);
    }

    #[test]
    fn test_brace_balance_escaped_quotes_in_string() {
        let input = r#"{"msg": "she said \"hello {world}\""}"#;
        let extracted = extract_json_by_brace_balance(input).unwrap();
        assert_eq!(extracted, input);
    }

    #[test]
    fn test_brace_balance_no_json() {
        let input = "no json here at all";
        assert!(extract_json_by_brace_balance(input).is_none());
    }

    #[test]
    fn test_brace_balance_unclosed_brace() {
        let input = r#"{"key": "value"#;
        assert!(extract_json_by_brace_balance(input).is_none());
    }

    #[test]
    fn test_brace_balance_deeply_nested() {
        // 20 levels of nesting -- verifies brace counter handles deep structures
        let mut json = String::new();
        for _ in 0..20 {
            json.push_str(r#"{"d":"#);
        }
        json.push('1');
        for _ in 0..20 {
            json.push('}');
        }

        let input = format!("prefix noise {json} trailing");
        let extracted = extract_json_by_brace_balance(&input).unwrap();
        assert_eq!(extracted, json);

        // Verify it's valid JSON
        let parsed: serde_json::Value = serde_json::from_str(extracted).unwrap();
        assert!(parsed.is_object());
    }

    #[test]
    fn test_brace_balance_prefix_noise_with_unbalanced_brace() {
        // Prefix contains a truly unbalanced `{` (no matching `}` before the JSON).
        // The retry logic should skip past the unbalanced brace and find the real JSON.
        let input = "some output { incomplete prefix\n\
                     {\"numPassedTests\":1,\"numFailedTests\":0,\"numPendingTests\":0,\"testResults\":[]}";
        let extracted = extract_json_by_brace_balance(input).unwrap();
        assert!(
            extracted.starts_with(r#"{"numPassedTests""#),
            "should skip the unbalanced prefix brace and find the JSON object, got: {extracted}"
        );

        // Verify it's valid JSON
        let parsed: serde_json::Value = serde_json::from_str(extracted).unwrap();
        assert_eq!(parsed["numPassedTests"], 1);
    }

    #[test]
    fn test_brace_balance_prefix_balanced_non_json() {
        // Prefix contains a balanced `{...}` that is not valid JSON (e.g., `{project}`).
        // The brace extractor returns it since it balances, but serde rejects it.
        // The `try_parse_json` retry loop then finds the real JSON as the next candidate.
        let input = r#"{project} output
{"numTotalTestSuites":1,"numPassedTestSuites":1,"numFailedTestSuites":0,"numPassedTests":1,"numFailedTests":0,"numPendingTests":0,"testResults":[]}"#;

        // At the brace-balance level, `{project}` is the first balanced match.
        let extracted = extract_json_by_brace_balance(input).unwrap();
        assert_eq!(extracted, "{project}");

        // Through the full parser, serde rejects `{project}` and the retry loop
        // advances to find the real JSON as the next candidate.
        let result = parse(input);
        assert!(
            result.is_full(),
            "expected Full after serde rejects prefix, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(test_result) = result {
            assert_eq!(test_result.summary.pass, 1);
            assert_eq!(test_result.summary.fail, 0);
        }
    }

    #[test]
    fn test_tier1_ansi_encoded_json() {
        // JSON wrapped in ANSI escape codes (e.g., vitest with colored output)
        let input = "\x1b[1m\x1b[32m{\"numTotalTestSuites\":1,\"numPassedTestSuites\":1,\"numFailedTestSuites\":0,\"numPassedTests\":2,\"numFailedTests\":0,\"numPendingTests\":0,\"testResults\":[{\"assertionResults\":[{\"status\":\"passed\",\"fullName\":\"test_a\",\"failureMessages\":[]},{\"status\":\"passed\",\"fullName\":\"test_b\",\"failureMessages\":[]}]}]}\x1b[0m";
        let result = parse(input);

        assert!(
            result.is_full(),
            "expected Full for ANSI-wrapped JSON, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(test_result) = result {
            assert_eq!(test_result.summary.pass, 2);
            assert_eq!(test_result.summary.fail, 0);
            assert_eq!(test_result.entries.len(), 2);
        }
    }

    // ========================================================================
    // Tier 2: Regex fallback tests
    // ========================================================================

    #[test]
    fn test_tier2_regex_fallback_pipe_format() {
        let input = "Tests  3 passed | 0 failed | 3 total";
        let result = parse(input);

        assert!(
            result.is_degraded(),
            "expected Degraded, got {}",
            result.tier_name()
        );
        if let ParseResult::Degraded(test_result, markers) = result {
            assert_eq!(test_result.summary.pass, 3);
            assert_eq!(test_result.summary.fail, 0);
            assert_eq!(test_result.summary.skip, 0);
            assert!(markers.contains(&"regex fallback".to_string()));
        }
    }

    #[test]
    fn test_tier2_regex_fallback_comma_format() {
        let input = "Tests: 5 passed, 2 failed, 7 total";
        let result = parse(input);

        assert!(
            result.is_degraded(),
            "expected Degraded, got {}",
            result.tier_name()
        );
        if let ParseResult::Degraded(test_result, _) = result {
            assert_eq!(test_result.summary.pass, 5);
            assert_eq!(test_result.summary.fail, 2);
            assert_eq!(test_result.summary.skip, 0);
        }
    }

    #[test]
    fn test_tier2_regex_within_larger_output() {
        let input = "\
Some vitest output here
Running tests...
Tests  10 passed | 2 failed | 12 total
Duration: 1.5s";
        let result = parse(input);

        assert!(
            result.is_degraded(),
            "expected Degraded, got {}",
            result.tier_name()
        );
        if let ParseResult::Degraded(test_result, _) = result {
            assert_eq!(test_result.summary.pass, 10);
            assert_eq!(test_result.summary.fail, 2);
            assert_eq!(test_result.summary.skip, 0);
        }
    }

    // ========================================================================
    // Tier 3: Passthrough tests
    // ========================================================================

    #[test]
    fn test_tier3_passthrough() {
        let input = "completely unparseable output with no test info";
        let result = parse(input);

        assert!(
            result.is_passthrough(),
            "expected Passthrough, got {}",
            result.tier_name()
        );
    }

    // ========================================================================
    // Flag injection tests
    // ========================================================================

    #[test]
    fn test_flag_injection_skipped_when_reporter_present() {
        let args = vec![
            "--reporter=verbose".to_string(),
            "--run".to_string(),
            "math".to_string(),
        ];
        assert!(
            user_has_flag(&args, &["--reporter"]),
            "should detect --reporter=verbose"
        );
    }

    #[test]
    fn test_flag_injection_skipped_bare_flag() {
        let args = vec!["--reporter".to_string(), "json".to_string()];
        assert!(
            user_has_flag(&args, &["--reporter"]),
            "should detect bare --reporter"
        );
    }

    #[test]
    fn test_flag_injection_needed_when_no_reporter() {
        let args = vec!["--run".to_string(), "math".to_string()];
        assert!(
            !user_has_flag(&args, &["--reporter"]),
            "should not detect --reporter when absent"
        );
    }
}
