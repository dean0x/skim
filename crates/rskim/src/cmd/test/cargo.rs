//! Cargo test parser with three-tier degradation (#46).
//!
//! Executes `cargo test` and parses the output into a structured `TestResult`.
//! Also supports cargo-nextest output via text state machine parsing.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON NDJSON parsing (nightly `--format json` or piped) or
//!   text state machine (nextest)
//! - **Tier 2 (Degraded)**: Regex on plain text `test result:` summary line
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation
//!
//! NOTE: The libtest JSON format (`{"type":"test",...}`) requires nightly Rust with
//! `-Z unstable-options --format json`. On stable Rust, `cargo test` emits plain text
//! which we parse via tier 2. The JSON tier exists for piped nightly output and future
//! compatibility when libtest stabilizes the JSON format.

use std::collections::HashSet;
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::{
    inject_flag_before_separator, run_parsed_command_with_mode, user_has_flag, OutputFormat,
    ParsedCommandConfig,
};
use crate::output::canonical::{TestEntry, TestOutcome, TestResult, TestSummary};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::shared::{scrape_failures, should_read_stdin, TestKind};

// Static regex patterns compiled once via LazyLock (avoids per-call compilation).
static RE_PASSED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)\s+passed").unwrap());
static RE_FAILED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)\s+failed").unwrap());
static RE_SKIPPED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)\s+skipped").unwrap());
static RE_CARGO_SUMMARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"test result: \w+\.\s+(\d+)\s+passed;\s+(\d+)\s+failed;\s+(\d+)\s+ignored").unwrap()
});

/// Run `skim test cargo [args...]`.
///
/// Builds the cargo command, executes it, and parses the output using
/// three-tier degradation. For nextest, skips JSON injection entirely.
/// For standard cargo test, injects `--message-format=json` to get build
/// artifact JSON on stdout (test results still come as plain text).
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    analytics_enabled: bool,
) -> anyhow::Result<ExitCode> {
    let is_nextest = args.iter().any(|a| a == "nextest");

    // Build command args: start with "test", append all user args
    let mut cmd_args: Vec<String> = vec!["test".to_string()];
    cmd_args.extend(args.iter().cloned());

    // For standard cargo test (not nextest), inject --message-format=json
    // to suppress human-formatted build progress on stdout. This makes the
    // test harness text output cleaner to parse. Skip if user already set it.
    if !is_nextest && !user_has_flag(&cmd_args, &["--message-format"]) {
        inject_flag_before_separator(&mut cmd_args, "--message-format=json");
    }

    // Determine whether to read from stdin or execute the command.
    // Delegates to shared::should_read_stdin for the same guard used by all
    // test parsers: stdin must be piped AND no user args provided.
    let use_stdin = should_read_stdin(args);

    run_parsed_command_with_mode(
        ParsedCommandConfig {
            program: "cargo",
            args: &cmd_args,
            env_overrides: &[("CARGO_TERM_COLOR", "never")],
            install_hint: "Install Rust via https://rustup.rs",
            use_stdin,
            show_stats,
            command_type: crate::analytics::CommandType::Test,
            output_format: OutputFormat::default(),
            analytics_enabled,
            family: "test",
        },
        move |output, _args| parse_impl(output, is_nextest),
    )
}

/// Three-tier parse function.
///
/// Receives the `CommandOutput` and a pre-computed `is_nextest` flag (captured
/// from the original user args in `run()` to avoid re-detecting from modified args).
fn parse_impl(output: &CommandOutput, is_nextest: bool) -> ParseResult<TestResult> {
    if is_nextest {
        // Tier 1: nextest text state machine
        if let Some(result) = try_parse_nextest(&output.stdout) {
            return ParseResult::Full(result);
        }
    } else {
        // Tier 1: JSON NDJSON parsing
        if let Some(result) = try_parse_json(&output.stdout) {
            return ParseResult::Full(result);
        }
    }

    // Tier 2: regex fallback on combined output
    let combined = if output.stderr.is_empty() {
        output.stdout.clone()
    } else {
        format!("{}\n{}", output.stdout, output.stderr)
    };

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["cargo test: no libtest JSON events found, using regex".to_string()],
        );
    }

    // Tier 3: passthrough
    ParseResult::Passthrough(combined)
}

// ============================================================================
// Tier 1: JSON NDJSON parsing (standard cargo test --message-format=json)
// ============================================================================

/// Attempt to parse cargo test JSON (NDJSON) output.
///
/// Looks for lines with `"type": "test"` and `"type": "suite"` events.
/// Returns `None` if no suite summary event is found.
fn try_parse_json(stdout: &str) -> Option<TestResult> {
    let mut entries: Vec<TestEntry> = Vec::new();
    let mut suite_found = false;
    let mut passed: usize = 0;
    let mut failed: usize = 0;
    let mut ignored: usize = 0;
    let mut exec_time_ms: Option<u64> = None;

    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        let Some(type_field) = value.get("type").and_then(|v| v.as_str()) else {
            continue;
        };

        match type_field {
            "test" => {
                let Some(event) = value.get("event").and_then(|v| v.as_str()) else {
                    continue;
                };
                let name = value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unknown>")
                    .to_string();

                let outcome = match event {
                    "ok" => TestOutcome::Pass,
                    "failed" => TestOutcome::Fail,
                    "ignored" => TestOutcome::Skip,
                    _ => continue,
                };

                let detail = if outcome == TestOutcome::Fail {
                    value
                        .get("stdout")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                } else {
                    None
                };

                entries.push(TestEntry {
                    name,
                    outcome,
                    detail,
                });
            }
            "suite" => {
                let Some(event) = value.get("event").and_then(|v| v.as_str()) else {
                    continue;
                };

                // Only process terminal suite events (ok/failed), not "started"
                if event != "ok" && event != "failed" {
                    continue;
                }

                suite_found = true;
                passed = value.get("passed").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                failed = value.get("failed").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                ignored = value.get("ignored").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                exec_time_ms = value
                    .get("exec_time")
                    .and_then(|v| v.as_f64())
                    .map(|s| (s * 1000.0) as u64);
            }
            _ => {}
        }
    }

    if !suite_found {
        return None;
    }

    let summary = TestSummary {
        pass: passed,
        fail: failed,
        skip: ignored,
        duration_ms: exec_time_ms,
    };

    Some(TestResult::new(summary, entries))
}

// ============================================================================
// Tier 1: nextest text state machine
// ============================================================================

/// Attempt to parse cargo-nextest text output.
///
/// Nextest output format:
/// ```text
///     Starting N tests across M test binaries
///         PASS [0.003s] crate::tests::test_name
///         FAIL [0.006s] crate::tests::test_name
///      Summary [0.010s] N tests run: N passed, N failed, N skipped
/// ```
///
/// Nextest prints failures both inline and in a recap section. We deduplicate
/// by tracking `(name, outcome)` pairs.
fn try_parse_nextest(stdout: &str) -> Option<TestResult> {
    let mut entries: Vec<TestEntry> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut summary_found = false;
    let mut total_passed: usize = 0;
    let mut total_failed: usize = 0;
    let mut total_skipped: usize = 0;
    let mut duration_ms: Option<u64> = None;

    // Track stdout capture blocks for failure detail
    let mut current_stdout_capture: Option<(String, String)> = None; // (test_name, accumulated_output)
    let mut in_stdout_block = false;

    for line in stdout.lines() {
        let trimmed = line.trim();

        // Check for STDOUT capture block start: "--- STDOUT:              crate::tests::test_name ---"
        if trimmed.starts_with("--- STDOUT:") && trimmed.ends_with("---") {
            let inner = trimmed
                .strip_prefix("--- STDOUT:")
                .unwrap_or("")
                .strip_suffix("---")
                .unwrap_or("")
                .trim();
            if !inner.is_empty() {
                current_stdout_capture = Some((inner.to_string(), String::new()));
                in_stdout_block = true;
                continue;
            }
        }

        // Check for STDERR capture block start (ends the STDOUT block)
        if trimmed.starts_with("--- STDERR:") && trimmed.ends_with("---") {
            // Finalize any pending STDOUT capture
            if let Some((ref test_name, ref captured)) = current_stdout_capture {
                let detail = captured.trim().to_string();
                if !detail.is_empty() {
                    // Attach detail to the matching entry
                    for entry in &mut entries {
                        if entry.name == *test_name && entry.detail.is_none() {
                            entry.detail = Some(detail.clone());
                            break;
                        }
                    }
                }
            }
            current_stdout_capture = None;
            in_stdout_block = false;
            continue;
        }

        // If we're in a STDOUT capture block, accumulate lines.
        // Use trim_end() instead of trim() to preserve leading whitespace
        // in assertion messages and formatted output.
        if in_stdout_block {
            if let Some((_, ref mut captured)) = current_stdout_capture {
                if !captured.is_empty() {
                    captured.push('\n');
                }
                captured.push_str(line.trim_end());
                continue;
            }
        }

        // Reset stdout block on non-indented lines that aren't part of capture
        if !trimmed.is_empty()
            && !trimmed.starts_with("PASS")
            && !trimmed.starts_with("FAIL")
            && !trimmed.starts_with("SKIP")
            && !trimmed.starts_with("Starting")
            && !trimmed.starts_with("Summary")
            && !trimmed.starts_with("Finished")
            && !trimmed.starts_with("---")
        {
            // Finalize any pending capture
            if let Some((ref test_name, ref captured)) = current_stdout_capture {
                let detail = captured.trim().to_string();
                if !detail.is_empty() {
                    for entry in &mut entries {
                        if entry.name == *test_name && entry.detail.is_none() {
                            entry.detail = Some(detail.clone());
                            break;
                        }
                    }
                }
            }
            current_stdout_capture = None;
            in_stdout_block = false;
        }

        // Parse test result lines: "PASS [0.003s] crate::tests::test_name"
        if let Some(rest) = trimmed.strip_prefix("PASS") {
            if let Some(name) = extract_nextest_name(rest) {
                let key = (name.clone(), "Pass".to_string());
                if seen.insert(key) {
                    entries.push(TestEntry {
                        name,
                        outcome: TestOutcome::Pass,
                        detail: None,
                    });
                }
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("FAIL") {
            if let Some(name) = extract_nextest_name(rest) {
                let key = (name.clone(), "Fail".to_string());
                if seen.insert(key) {
                    entries.push(TestEntry {
                        name,
                        outcome: TestOutcome::Fail,
                        detail: None,
                    });
                }
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("SKIP") {
            if let Some(name) = extract_nextest_name(rest) {
                let key = (name.clone(), "Skip".to_string());
                if seen.insert(key) {
                    entries.push(TestEntry {
                        name,
                        outcome: TestOutcome::Skip,
                        detail: None,
                    });
                }
            }
            continue;
        }

        // Parse summary line: "Summary [0.010s] N tests run: N passed, N failed, N skipped"
        if trimmed.starts_with("Summary") {
            if let Some(summary) = parse_nextest_summary(trimmed) {
                summary_found = true;
                total_passed = summary.0;
                total_failed = summary.1;
                total_skipped = summary.2;
                duration_ms = summary.3;
            }
        }
    }

    // Finalize any pending stdout capture
    if let Some((ref test_name, ref captured)) = current_stdout_capture {
        let detail = captured.trim().to_string();
        if !detail.is_empty() {
            for entry in &mut entries {
                if entry.name == *test_name && entry.detail.is_none() {
                    entry.detail = Some(detail.clone());
                    break;
                }
            }
        }
    }

    if !summary_found {
        return None;
    }

    let summary = TestSummary {
        pass: total_passed,
        fail: total_failed,
        skip: total_skipped,
        duration_ms,
    };

    Some(TestResult::new(summary, entries))
}

/// Extract the test name from a nextest result line remainder.
///
/// Input: ` [0.003s] rskim lib::tests::test_a`
/// Output: `Some("rskim lib::tests::test_a")`
fn extract_nextest_name(rest: &str) -> Option<String> {
    let rest = rest.trim();
    // Skip the duration bracket: [0.003s]
    if let Some(after_bracket) = rest.strip_prefix('[') {
        if let Some(pos) = after_bracket.find(']') {
            let name = after_bracket[pos + 1..].trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Parse a nextest summary line.
///
/// Input: `Summary [   0.010s] 3 tests run: 3 passed, 0 skipped`
/// Returns: `(passed, failed, skipped, duration_ms)`
fn parse_nextest_summary(line: &str) -> Option<(usize, usize, usize, Option<u64>)> {
    let mut passed: usize = 0;
    let mut failed: usize = 0;
    let mut skipped: usize = 0;
    let mut duration_ms: Option<u64> = None;

    // Extract duration from brackets
    if let Some(start) = line.find('[') {
        if let Some(end) = line.find(']') {
            let dur_str = line[start + 1..end].trim();
            if let Some(secs_str) = dur_str.strip_suffix('s') {
                if let Ok(secs) = secs_str.trim().parse::<f64>() {
                    duration_ms = Some((secs * 1000.0) as u64);
                }
            }
        }
    }

    // Parse "N passed", "N failed", "N skipped" from the summary.
    // Uses static LazyLock regexes to avoid per-call compilation.
    let mut any_matched = false;

    if let Some(caps) = RE_PASSED.captures(line) {
        any_matched = true;
        passed = caps[1].parse().unwrap_or(0);
    }
    if let Some(caps) = RE_FAILED.captures(line) {
        any_matched = true;
        failed = caps[1].parse().unwrap_or(0);
    }
    if let Some(caps) = RE_SKIPPED.captures(line) {
        any_matched = true;
        skipped = caps[1].parse().unwrap_or(0);
    }

    // At least one count field must have matched for a valid summary.
    // This correctly handles zero-count summaries (e.g., "0 passed, 0 skipped"
    // when all tests are filtered out) by tracking regex matches, not values.
    if any_matched {
        Some((passed, failed, skipped, duration_ms))
    } else {
        None
    }
}

// ============================================================================
// Tier 2: regex fallback
// ============================================================================

/// Attempt to parse standard cargo test summary lines using regex.
///
/// Cargo runs multiple test binaries, each producing its own summary line:
/// `test result: ok. N passed; N failed; N ignored`
///
/// This function finds ALL such lines and aggregates the totals. When failures
/// are present, individual test names are scraped via [`scrape_failures`] so
/// LLMs receive a list of failing tests (AD-Commit2, 2026-04-11).
fn try_parse_regex(text: &str) -> Option<TestResult> {
    let mut total_passed: usize = 0;
    let mut total_failed: usize = 0;
    let mut total_ignored: usize = 0;
    let mut found = false;

    for caps in RE_CARGO_SUMMARY.captures_iter(text) {
        found = true;
        total_passed += caps[1].parse::<usize>().unwrap_or(0);
        total_failed += caps[2].parse::<usize>().unwrap_or(0);
        total_ignored += caps[3].parse::<usize>().unwrap_or(0);
    }

    if !found {
        return None;
    }

    let summary = TestSummary {
        pass: total_passed,
        fail: total_failed,
        skip: total_ignored,
        duration_ms: None,
    };

    // Scrape failing test names from the full text output so the Tier-2 result
    // includes individual test names rather than an empty entries list.
    let entries = if total_failed > 0 {
        scrape_failures(text, TestKind::Cargo)
    } else {
        vec![]
    };

    Some(TestResult::new(summary, entries))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to load fixture files
    fn fixture_path(name: &str) -> std::path::PathBuf {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests");
        path.push("fixtures");
        path.push("cmd");
        path.push("test");
        path.push(name);
        path
    }

    fn load_fixture(name: &str) -> String {
        std::fs::read_to_string(fixture_path(name))
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    // ========================================================================
    // Tier 1: JSON parsing
    // ========================================================================

    #[test]
    fn test_tier1_all_pass() {
        let input = load_fixture("cargo_pass.json");
        let result = try_parse_json(&input);
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert!(result.summary.pass > 0, "Expected at least one pass");
        assert_eq!(result.summary.fail, 0, "Expected zero failures");
    }

    #[test]
    fn test_tier1_with_failures() {
        let input = load_fixture("cargo_fail.json");
        let result = try_parse_json(&input);
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert!(result.summary.fail > 0, "Expected at least one failure");

        // Check that failure entries have detail
        let fail_entries: Vec<&TestEntry> = result
            .entries
            .iter()
            .filter(|e| e.outcome == TestOutcome::Fail)
            .collect();
        assert!(!fail_entries.is_empty(), "Expected failure entries");
        assert!(
            fail_entries[0].detail.is_some(),
            "Expected failure detail (stdout capture)"
        );
    }

    // ========================================================================
    // Tier 1: nextest parsing
    // ========================================================================

    #[test]
    fn test_tier1_nextest_pass() {
        let input = load_fixture("cargo_nextest_pass.txt");
        let result = try_parse_nextest(&input);
        assert!(result.is_some(), "Expected Tier 1 nextest parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.summary.pass, 3);
        assert_eq!(result.summary.fail, 0);
    }

    #[test]
    fn test_tier1_nextest_dedup() {
        let input = load_fixture("cargo_nextest_fail.txt");
        let result = try_parse_nextest(&input);
        assert!(result.is_some(), "Expected Tier 1 nextest parse to succeed");
        let result = result.unwrap();

        // The fixture has test_b listed twice (inline + recap). Should be deduped.
        let fail_entries: Vec<&TestEntry> = result
            .entries
            .iter()
            .filter(|e| e.outcome == TestOutcome::Fail)
            .collect();
        assert_eq!(
            fail_entries.len(),
            1,
            "Expected exactly 1 failure entry (deduped), got {}",
            fail_entries.len()
        );
    }

    // ========================================================================
    // Tier 2: regex fallback
    // ========================================================================

    #[test]
    fn test_tier2_regex_fallback() {
        let text = "running 10 tests\ntest result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out";
        let result = try_parse_regex(text);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.summary.pass, 10);
        assert_eq!(result.summary.fail, 0);
        assert_eq!(result.summary.skip, 0);
    }

    #[test]
    fn test_tier2_regex_with_failures() {
        let text = "test result: FAILED. 8 passed; 2 failed; 1 ignored; 0 measured";
        let result = try_parse_regex(text);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.summary.pass, 8);
        assert_eq!(result.summary.fail, 2);
        assert_eq!(result.summary.skip, 1);
    }

    // ========================================================================
    // Three-tier integration via parse_impl()
    // ========================================================================

    #[test]
    fn test_parse_json_produces_full() {
        let input = load_fixture("cargo_pass.json");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output, false);
        assert!(
            result.is_full(),
            "Expected Full parse result, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_plain_text_produces_degraded() {
        let output = CommandOutput {
            stdout: "test result: ok. 5 passed; 0 failed; 0 ignored".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output, false);
        assert!(
            result.is_degraded(),
            "Expected Degraded parse result, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_garbage_produces_passthrough() {
        let output = CommandOutput {
            stdout: "completely unparseable output\nno json, no regex match".to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output, false);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }

    // ========================================================================
    // Flag inspection helpers
    // ========================================================================

    #[test]
    fn test_flag_injection_skipped() {
        // When user already has --message-format=json2, we should NOT inject another
        let args = vec![
            "test".to_string(),
            "--message-format=json2".to_string(),
            "--".to_string(),
            "--nocapture".to_string(),
        ];
        assert!(
            user_has_flag(&args, &["--message-format"]),
            "Should detect existing --message-format flag"
        );
    }

    #[test]
    fn test_flag_injection_not_triggered_for_different_flag() {
        let args = vec!["test".to_string(), "--release".to_string()];
        assert!(
            !user_has_flag(&args, &["--message-format"]),
            "Should not detect --message-format when only --release is present"
        );
    }

    #[test]
    fn test_separator_args_preserved() {
        let mut args = vec![
            "test".to_string(),
            "--".to_string(),
            "--nocapture".to_string(),
        ];
        inject_flag_before_separator(&mut args, "--message-format=json");
        // --message-format=json should be before --
        assert_eq!(args[1], "--message-format=json");
        assert_eq!(args[2], "--");
        assert_eq!(args[3], "--nocapture");
    }

    #[test]
    fn test_inject_flag_no_separator() {
        let mut args = vec!["test".to_string(), "--release".to_string()];
        inject_flag_before_separator(&mut args, "--message-format=json");
        // Should be appended at the end
        assert_eq!(args.last().unwrap(), "--message-format=json");
    }

    // ========================================================================
    // Tier-2 scrape_failures integration (AD-Commit2)
    // ========================================================================

    #[test]
    fn test_tier2_regex_scrapes_failing_test_names() {
        let input = load_fixture("cargo_regex_fail.txt");
        let result = try_parse_regex(&input);
        assert!(
            result.is_some(),
            "regex parse must succeed on failure fixture"
        );
        let result = result.unwrap();
        assert!(result.summary.fail > 0, "must have failures");
        // Tier-2 entries should now include failing test names.
        assert!(
            !result.entries.is_empty(),
            "Tier-2 must list failing test names, got empty entries"
        );
        assert!(
            result.entries.iter().any(|e| e.name.contains("test_foo")),
            "must contain test_foo: {:?}",
            result.entries
        );
    }

    #[test]
    fn test_tier2_regression_tier1_json_still_populates_entries() {
        // Tier-1 JSON path must still populate entries (regression guard).
        let input = load_fixture("cargo_fail.json");
        let result = try_parse_json(&input);
        assert!(result.is_some(), "Tier-1 JSON parse must succeed");
        let result = result.unwrap();
        assert!(
            !result.entries.is_empty(),
            "Tier-1 entries must not be empty after Tier-2 changes"
        );
    }
}
