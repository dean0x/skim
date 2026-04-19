//! Go test parser with three-tier degradation (#49)
//!
//! Parses `go test -json` NDJSON output (Tier 1), falls back to regex
//! on `--- PASS/FAIL/SKIP` lines (Tier 2), and passes through raw output
//! when nothing can be parsed (Tier 3).

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::output::canonical::{TestEntry, TestOutcome, TestResult, TestSummary};
use crate::output::ParseResult;
use crate::runner::CommandRunner;

// ============================================================================
// Public entry point
// ============================================================================

/// Execute `go test` with the given args and parse the output.
///
/// Injects `-json` if the user hasn't already set `-json` or `-v`,
/// then runs the command through [`CommandRunner`] and parses output
/// via three-tier degradation.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    analytics_enabled: bool,
) -> anyhow::Result<ExitCode> {
    // Passthrough mode: run `go test` without flag injections and forward raw output.
    if crate::cmd::is_passthrough_mode() {
        let mut raw_args: Vec<String> = vec!["test".to_string()];
        raw_args.extend_from_slice(args);
        let raw_args_ref: Vec<&str> = raw_args.iter().map(|s| s.as_str()).collect();
        let runner = CommandRunner::new(None);
        let output = runner.run("go", &raw_args_ref)?;
        print!("{}", crate::cmd::combine_output(&output));
        let code = output.exit_code.unwrap_or(1).clamp(0, 255) as u8;
        return Ok(ExitCode::from(code));
    }

    let mut go_args: Vec<String> = vec!["test".to_string()];

    // Inject -json before any `--` separator, unless the user already specified
    // -json or -v (verbose mode produces non-JSON output).
    //
    // Go flags use `-flag=false` to explicitly disable a flag, so we use
    // go-specific detection that treats `-v=false` as NOT having `-v`.
    if !go_has_flag(args, "-json") && !go_has_flag(args, "-v") {
        // Find the position of `--` if present, and inject -json before it.
        if let Some(sep_pos) = args.iter().position(|a| a == "--") {
            go_args.extend_from_slice(&args[..sep_pos]);
            go_args.push("-json".to_string());
            go_args.extend_from_slice(&args[sep_pos..]);
        } else {
            go_args.push("-json".to_string());
            go_args.extend_from_slice(args);
        }
    } else {
        go_args.extend_from_slice(args);
    }

    let runner = CommandRunner::new(None);
    let go_args_ref: Vec<&str> = go_args.iter().map(|s| s.as_str()).collect();

    let output = runner.run("go", &go_args_ref).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("failed to execute") {
            anyhow::anyhow!("{}\nHint: install Go from https://go.dev/dl/", msg)
        } else {
            e
        }
    })?;

    // Combine stdout and stderr for parsing (go test writes to both).
    let combined = if output.stderr.is_empty() {
        output.stdout.clone()
    } else {
        format!("{}\n{}", output.stdout, output.stderr)
    };

    let parsed = parse(&combined);

    // Emit the result
    let exit_code = match &parsed {
        ParseResult::Full(result) | ParseResult::Degraded(result, _) => {
            println!("{result}");
            // Emit degradation markers to stderr
            let mut stderr = std::io::stderr().lock();
            let _ = parsed.emit_markers(&mut stderr);

            if result.summary.fail > 0 {
                // Append raw failure context so the agent can see actual error
                // messages without needing to re-run with SKIM_PASSTHROUGH=1.
                use super::shared;
                let stripped = crate::output::strip_ansi(&combined);
                let tail = shared::last_n_lines(&stripped, shared::MAX_FAILURE_CONTEXT_LINES);
                if !tail.is_empty() {
                    println!(
                        "\n--- failure context (last {} lines) ---",
                        tail.lines().count()
                    );
                    println!("{tail}");
                }
                eprintln!(
                    "[skim] compressed output (exit 1). SKIM_PASSTHROUGH=1 for full output."
                );
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        ParseResult::Passthrough(raw) => {
            println!("{raw}");
            let mut stderr = std::io::stderr().lock();
            let _ = parsed.emit_markers(&mut stderr);
            // Mirror the original process exit code
            match output.exit_code {
                Some(0) => ExitCode::SUCCESS,
                _ => ExitCode::FAILURE,
            }
        }
    };

    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(&combined, parsed.content());
        crate::process::report_token_stats(orig, comp, "");
    }

    // Record analytics (fire-and-forget, non-blocking).
    crate::analytics::try_record_command(
        analytics_enabled,
        combined,
        parsed.content().to_string(),
        crate::cmd::format_analytics_label("test", "go", &args.join(" ")),
        crate::analytics::CommandType::Test,
        output.duration,
        Some(parsed.tier_name()),
    );

    Ok(exit_code)
}

// ============================================================================
// Flag detection
// ============================================================================

/// Go-specific flag detection that respects `-flag=false` semantics.
///
/// Unlike the shared `crate::cmd::user_has_flag`, Go CLI flags use
/// `-flag=false` to explicitly disable a boolean flag. This function
/// treats `-v=false` as the flag NOT being set, which is required for
/// correct `-json` injection logic. The shared version does not handle
/// this because `=false` is not a convention outside Go tooling.
fn go_has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| {
        if a == flag {
            return true;
        }
        if let Some(value) = a.strip_prefix(&format!("{flag}=")) {
            // -v=false means the flag is NOT set
            return value != "false";
        }
        false
    })
}

// ============================================================================
// Three-tier parser
// ============================================================================

/// Parse go test output through three-tier degradation.
///
/// - Tier 1: NDJSON (`go test -json`) → `ParseResult::Full`
/// - Tier 2: Regex fallback on `--- PASS/FAIL/SKIP` lines → `ParseResult::Degraded`
/// - Tier 3: Raw passthrough → `ParseResult::Passthrough`
fn parse(output: &str) -> ParseResult<TestResult> {
    // Tier 1: Try NDJSON parsing
    if let Some(result) = try_parse_ndjson(output) {
        return ParseResult::Full(result);
    }

    // Tier 2: Regex fallback
    if let Some(result) = try_parse_regex(output) {
        return ParseResult::Degraded(
            result,
            vec!["go test: JSON parse failed, using regex".to_string()],
        );
    }

    // Tier 3: Passthrough
    ParseResult::Passthrough(output.to_string())
}

// ============================================================================
// Tier 1: NDJSON parser
// ============================================================================

/// Parse NDJSON output from `go test -json`.
///
/// Each line is a JSON object with Action, Package, Test (optional),
/// Output (optional), and Elapsed (optional) fields. We track test
/// outcomes by (Package, Test) key and collect output for failed tests.
fn try_parse_ndjson(output: &str) -> Option<TestResult> {
    let mut test_outcomes: HashMap<String, TestOutcome> = HashMap::new();
    let mut test_outputs: HashMap<String, Vec<String>> = HashMap::new();
    let mut package_elapsed: HashMap<String, f64> = HashMap::new();
    let mut found_any_event = false;

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let action = match event.get("Action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => continue,
        };

        let package = event.get("Package").and_then(|v| v.as_str()).unwrap_or("");

        let test_name = event.get("Test").and_then(|v| v.as_str());

        // Only track test-level events (not package-level) for test entries
        if let Some(name) = test_name {
            let key = format!("{package}::{name}");

            match action {
                "pass" => {
                    found_any_event = true;
                    test_outcomes.insert(key, TestOutcome::Pass);
                }
                "fail" => {
                    found_any_event = true;
                    test_outcomes.insert(key, TestOutcome::Fail);
                }
                "skip" => {
                    found_any_event = true;
                    test_outcomes.insert(key, TestOutcome::Skip);
                }
                "output" => {
                    // Output is bounded by CommandRunner's 64 MiB cap on total
                    // process output. Within that, per-test accumulation is acceptable.
                    if let Some(text) = event.get("Output").and_then(|v| v.as_str()) {
                        test_outputs.entry(key).or_default().push(text.to_string());
                    }
                }
                // "run", "pause", "cont", "bench" — ignore
                _ => {}
            }
        } else {
            // Package-level events — track elapsed for duration
            if let Some(elapsed) = event.get("Elapsed").and_then(|v| v.as_f64()) {
                if matches!(action, "pass" | "fail") {
                    found_any_event = true;
                    package_elapsed.insert(package.to_string(), elapsed);
                }
            } else if matches!(action, "pass" | "fail") {
                found_any_event = true;
            }
        }
    }

    if !found_any_event {
        return None;
    }

    // Build test entries
    let mut entries: Vec<TestEntry> = Vec::new();
    let mut pass_count: usize = 0;
    let mut fail_count: usize = 0;
    let mut skip_count: usize = 0;

    // Sort keys for deterministic output
    let mut keys: Vec<String> = test_outcomes.keys().cloned().collect();
    keys.sort();

    for key in &keys {
        let outcome = test_outcomes.get(key).unwrap().clone();
        let detail = if outcome == TestOutcome::Fail {
            // Collect output lines for failed tests, trimming trailing whitespace
            test_outputs.get(key).map(|lines| {
                lines
                    .iter()
                    .map(|l| l.trim_end().to_string())
                    .filter(|l| !l.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        } else {
            None
        };

        match outcome {
            TestOutcome::Pass => pass_count += 1,
            TestOutcome::Fail => fail_count += 1,
            TestOutcome::Skip => skip_count += 1,
        }

        entries.push(TestEntry {
            name: key.clone(),
            outcome,
            detail,
        });
    }

    // Compute total duration from package elapsed times
    let total_elapsed_secs: f64 = package_elapsed.values().sum();
    let duration_ms = if total_elapsed_secs > 0.0 {
        Some((total_elapsed_secs * 1000.0) as u64)
    } else {
        None
    };

    let summary = TestSummary {
        pass: pass_count,
        fail: fail_count,
        skip: skip_count,
        duration_ms,
    };

    Some(TestResult::new(summary, entries))
}

// ============================================================================
// Tier 2: Regex fallback
// ============================================================================

/// Matches `--- PASS/FAIL/SKIP: TestName` lines in go test verbose output.
static TEST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"---\s+(PASS|FAIL|SKIP):\s+(\S+)").unwrap());

/// Matches `ok  package/name  0.123s` summary lines.
static SUMMARY_OK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ok\s+(\S+)\s+([\d.]+)s").unwrap());

/// Parse go test output using regex patterns on `--- PASS/FAIL/SKIP` lines.
///
/// Falls back from NDJSON when the output is plain text (e.g., user ran
/// `go test` without `-json`, or piped non-JSON input).
///
/// Known limitations compared to Tier 1:
/// - Test names are not package-prefixed because `--- PASS/FAIL/SKIP` lines
///   do not include package info. Package is extracted from `ok` summary lines
///   when available and prepended.
/// - Failure details are not collected. Tier 2 cannot reliably extract failure
///   output from plain text.
fn try_parse_regex(output: &str) -> Option<TestResult> {
    let mut entries: Vec<TestEntry> = Vec::new();
    let mut pass_count: usize = 0;
    let mut fail_count: usize = 0;
    let mut skip_count: usize = 0;
    let mut total_duration_secs: f64 = 0.0;
    // Extract package name from `ok` summary lines for prefixing test names.
    let mut package_name: Option<String> = None;

    for line in output.lines() {
        if let Some(caps) = TEST_RE.captures(line) {
            let outcome_str = caps.get(1).unwrap().as_str();
            let name = caps.get(2).unwrap().as_str().to_string();

            let outcome = match outcome_str {
                "PASS" => {
                    pass_count += 1;
                    TestOutcome::Pass
                }
                "FAIL" => {
                    fail_count += 1;
                    TestOutcome::Fail
                }
                "SKIP" => {
                    skip_count += 1;
                    TestOutcome::Skip
                }
                _ => continue,
            };

            entries.push(TestEntry {
                name,
                outcome,
                // Tier 2 cannot reliably extract failure details from plain text.
                detail: None,
            });
        }

        // Extract duration and package name from summary line
        if let Some(caps) = SUMMARY_OK_RE.captures(line) {
            if package_name.is_none() {
                package_name = Some(caps.get(1).unwrap().as_str().to_string());
            }
            if let Ok(secs) = caps.get(2).unwrap().as_str().parse::<f64>() {
                total_duration_secs += secs;
            }
        }
    }

    if entries.is_empty() {
        return None;
    }

    // Prefix test names with package when available (matching Tier 1 format).
    if let Some(ref pkg) = package_name {
        for entry in &mut entries {
            entry.name = format!("{pkg}::{}", entry.name);
        }
    }

    let duration_ms = if total_duration_secs > 0.0 {
        Some((total_duration_secs * 1000.0) as u64)
    } else {
        None
    };

    let summary = TestSummary {
        pass: pass_count,
        fail: fail_count,
        skip: skip_count,
        duration_ms,
    };

    Some(TestResult::new(summary, entries))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path(name: &str) -> String {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        format!("{manifest_dir}/tests/fixtures/go_test/{name}")
    }

    fn read_fixture(name: &str) -> String {
        std::fs::read_to_string(fixture_path(name))
            .unwrap_or_else(|e| panic!("Failed to read fixture {name}: {e}"))
    }

    // ========================================================================
    // Tier 1: NDJSON tests
    // ========================================================================

    #[test]
    fn test_tier1_all_pass() {
        let input = read_fixture("go_test_pass.json");
        let result = parse(&input);

        assert!(
            result.is_full(),
            "expected Full, got {}",
            result.tier_name()
        );

        if let ParseResult::Full(test_result) = &result {
            assert_eq!(test_result.summary.pass, 2, "expected 2 passing tests");
            assert_eq!(test_result.summary.fail, 0, "expected 0 failing tests");
            assert_eq!(test_result.summary.skip, 0, "expected 0 skipped tests");
            assert_eq!(test_result.entries.len(), 2, "expected 2 test entries");

            // Verify test names are prefixed with package
            assert!(
                test_result
                    .entries
                    .iter()
                    .all(|e| e.name.starts_with("example.com/pkg::")),
                "expected all test names to be prefixed with package, got: {:?}",
                test_result
                    .entries
                    .iter()
                    .map(|e| &e.name)
                    .collect::<Vec<_>>()
            );

            // Verify duration was extracted
            assert!(
                test_result.summary.duration_ms.is_some(),
                "expected duration to be present"
            );
        }
    }

    #[test]
    fn test_tier1_with_failures() {
        let input = read_fixture("go_test_fail.json");
        let result = parse(&input);

        assert!(
            result.is_full(),
            "expected Full, got {}",
            result.tier_name()
        );

        if let ParseResult::Full(test_result) = &result {
            assert_eq!(test_result.summary.pass, 1, "expected 1 passing test");
            assert_eq!(test_result.summary.fail, 1, "expected 1 failing test");

            // Find the failing entry and verify detail is present
            let failed = test_result
                .entries
                .iter()
                .find(|e| e.outcome == TestOutcome::Fail)
                .expect("expected a failing test entry");

            assert!(
                failed.name.contains("TestDiv"),
                "expected failing test name to contain TestDiv, got: {}",
                failed.name
            );

            assert!(
                failed.detail.is_some(),
                "expected detail to be present for failed test"
            );

            let detail = failed.detail.as_ref().unwrap();
            assert!(
                detail.contains("expected 0, got 1"),
                "expected detail to contain error message, got: {detail}"
            );
        }
    }

    #[test]
    fn test_tier1_multi_package() {
        let input = r#"{"Time":"2024-01-01T00:00:00Z","Action":"run","Package":"pkg/a","Test":"TestA"}
{"Time":"2024-01-01T00:00:00Z","Action":"pass","Package":"pkg/a","Test":"TestA","Elapsed":0.001}
{"Time":"2024-01-01T00:00:00Z","Action":"pass","Package":"pkg/a","Elapsed":0.002}
{"Time":"2024-01-01T00:00:00Z","Action":"run","Package":"pkg/b","Test":"TestB"}
{"Time":"2024-01-01T00:00:00Z","Action":"pass","Package":"pkg/b","Test":"TestB","Elapsed":0.001}
{"Time":"2024-01-01T00:00:00Z","Action":"pass","Package":"pkg/b","Elapsed":0.003}
"#;

        let result = parse(input);
        assert!(
            result.is_full(),
            "expected Full, got {}",
            result.tier_name()
        );

        if let ParseResult::Full(test_result) = &result {
            assert_eq!(test_result.summary.pass, 2, "expected 2 passing tests");
            assert_eq!(test_result.entries.len(), 2, "expected 2 test entries");

            // Verify both packages are represented in test names
            let names: Vec<&str> = test_result
                .entries
                .iter()
                .map(|e| e.name.as_str())
                .collect();
            assert!(
                names.contains(&"pkg/a::TestA"),
                "expected pkg/a::TestA in entries, got: {names:?}"
            );
            assert!(
                names.contains(&"pkg/b::TestB"),
                "expected pkg/b::TestB in entries, got: {names:?}"
            );
        }
    }

    // ========================================================================
    // Tier 2: Regex fallback tests
    // ========================================================================

    #[test]
    fn test_tier2_regex_fallback() {
        let input = read_fixture("go_test_text.txt");
        let result = parse(&input);

        assert!(
            result.is_degraded(),
            "expected Degraded, got {}",
            result.tier_name()
        );

        if let ParseResult::Degraded(test_result, markers) = &result {
            assert_eq!(test_result.summary.pass, 2, "expected 2 passing tests");
            assert_eq!(test_result.summary.fail, 0, "expected 0 failing tests");
            assert_eq!(test_result.entries.len(), 2, "expected 2 test entries");

            // Verify marker indicates regex fallback
            assert!(
                markers.contains(&"go test: JSON parse failed, using regex".to_string()),
                "expected 'go test: JSON parse failed, using regex' marker, got: {markers:?}"
            );

            // Verify duration was extracted from summary line
            assert!(
                test_result.summary.duration_ms.is_some(),
                "expected duration to be present from ok line"
            );

            // Verify test names are package-prefixed from ok summary line
            assert!(
                test_result
                    .entries
                    .iter()
                    .all(|e| e.name.starts_with("example.com/pkg::")),
                "expected all Tier 2 test names to be package-prefixed, got: {:?}",
                test_result
                    .entries
                    .iter()
                    .map(|e| &e.name)
                    .collect::<Vec<_>>()
            );
        }
    }

    // ========================================================================
    // Tier 3: Passthrough test
    // ========================================================================

    #[test]
    fn test_tier3_passthrough() {
        let input = "some random output\nwith no test patterns\nat all\n";
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
    fn test_flag_injection_skipped_with_v() {
        let args = vec!["-v".to_string(), "./...".to_string()];
        assert!(go_has_flag(&args, "-v"), "expected -v to be detected");
        assert!(
            !go_has_flag(&args, "-json"),
            "expected -json to NOT be detected"
        );
    }

    #[test]
    fn test_flag_injection_skipped_with_json() {
        let args = vec!["-json".to_string(), "./...".to_string()];
        assert!(go_has_flag(&args, "-json"), "expected -json to be detected");
        assert!(!go_has_flag(&args, "-v"), "expected -v to NOT be detected");
    }

    #[test]
    fn test_user_has_flag_with_equals() {
        let args = vec!["-json=true".to_string()];
        assert!(
            go_has_flag(&args, "-json"),
            "expected -json=true to match -json"
        );
    }

    #[test]
    fn test_user_has_flag_no_match() {
        let args = vec![
            "./...".to_string(),
            "-run".to_string(),
            "TestFoo".to_string(),
        ];
        assert!(
            !go_has_flag(&args, "-json"),
            "expected -json to NOT be detected"
        );
        assert!(!go_has_flag(&args, "-v"), "expected -v to NOT be detected");
    }

    // ========================================================================
    // Edge cases
    // ========================================================================

    #[test]
    fn test_empty_input() {
        let result = parse("");
        assert!(
            result.is_passthrough(),
            "expected Passthrough for empty input, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_ndjson_with_only_package_events() {
        // Package-level pass/fail without any Test-level events should still
        // produce a Full result (found_any_event = true) but with no entries.
        let input = r#"{"Time":"2024-01-01T00:00:00Z","Action":"output","Package":"example.com/pkg","Output":"ok  \texample.com/pkg\t0.005s\n"}
{"Time":"2024-01-01T00:00:00Z","Action":"pass","Package":"example.com/pkg","Elapsed":0.005}
"#;

        let result = parse(input);
        assert!(
            result.is_full(),
            "expected Full for package-only events, got {}",
            result.tier_name()
        );

        if let ParseResult::Full(test_result) = &result {
            assert_eq!(
                test_result.entries.len(),
                0,
                "expected 0 test entries for package-only events"
            );
            assert_eq!(
                test_result.summary.pass, 0,
                "expected 0 pass (no test-level events)"
            );
        }
    }

    #[test]
    fn test_ndjson_skip_action() {
        let input = r#"{"Time":"2024-01-01T00:00:00Z","Action":"run","Package":"pkg","Test":"TestSkipped"}
{"Time":"2024-01-01T00:00:00Z","Action":"skip","Package":"pkg","Test":"TestSkipped","Elapsed":0.0}
"#;

        let result = parse(input);
        assert!(
            result.is_full(),
            "expected Full, got {}",
            result.tier_name()
        );

        if let ParseResult::Full(test_result) = &result {
            assert_eq!(test_result.summary.skip, 1, "expected 1 skipped test");
            assert_eq!(test_result.entries[0].outcome, TestOutcome::Skip);
        }
    }

    #[test]
    fn test_ndjson_malformed_lines_skipped() {
        let input = "not json\n{\"Action\":\"pass\",\"Package\":\"pkg\",\"Test\":\"TestA\"}\nalso not json\n";
        let result = parse(input);
        assert!(
            result.is_full(),
            "expected Full (valid NDJSON mixed with garbage), got {}",
            result.tier_name()
        );

        if let ParseResult::Full(test_result) = &result {
            assert_eq!(
                test_result.summary.pass, 1,
                "expected 1 passing test from valid line"
            );
        }
    }

    #[test]
    fn test_regex_with_skip_outcome() {
        let input = "=== RUN   TestSkipped\n--- SKIP: TestSkipped (0.00s)\nok      example.com/pkg 0.003s\n";
        let result = parse(input);
        // This should be Tier 2 (no NDJSON), Degraded
        // Actually, there's no valid JSON so Tier 1 fails, then Tier 2 regex finds it
        assert!(
            result.is_degraded(),
            "expected Degraded, got {}",
            result.tier_name()
        );

        if let ParseResult::Degraded(test_result, _) = &result {
            assert_eq!(test_result.summary.skip, 1, "expected 1 skipped test");
            // Verify package prefix from ok summary line
            assert_eq!(
                test_result.entries[0].name, "example.com/pkg::TestSkipped",
                "expected package-prefixed name"
            );
        }
    }

    // ========================================================================
    // `--` separator and flag edge cases
    // ========================================================================

    #[test]
    fn test_separator_flag_injection() {
        // When `--` is present, `-json` must be injected before it so the
        // Go toolchain sees the flag, while args after `--` pass through.
        let args = vec![
            "./...".to_string(),
            "--".to_string(),
            "-run".to_string(),
            "TestFoo".to_string(),
        ];

        let mut go_args: Vec<String> = vec!["test".to_string()];
        if !go_has_flag(&args, "-json") && !go_has_flag(&args, "-v") {
            if let Some(sep_pos) = args.iter().position(|a| a == "--") {
                go_args.extend_from_slice(&args[..sep_pos]);
                go_args.push("-json".to_string());
                go_args.extend_from_slice(&args[sep_pos..]);
            } else {
                go_args.push("-json".to_string());
                go_args.extend_from_slice(&args);
            }
        } else {
            go_args.extend_from_slice(&args);
        }

        // -json should appear before `--`
        let json_pos = go_args.iter().position(|a| a == "-json").unwrap();
        let sep_pos = go_args.iter().position(|a| a == "--").unwrap();
        assert!(
            json_pos < sep_pos,
            "expected -json (pos {json_pos}) before -- (pos {sep_pos}), got: {go_args:?}"
        );
    }

    #[test]
    fn test_v_equals_false_still_injects_json() {
        // `-v=false` explicitly disables verbose mode, so -json should be injected.
        let args = vec!["-v=false".to_string(), "./...".to_string()];
        assert!(
            !go_has_flag(&args, "-v"),
            "expected -v=false to NOT be detected as -v"
        );
    }

    #[test]
    fn test_v_equals_true_skips_json_injection() {
        // `-v=true` enables verbose mode, so -json should NOT be injected.
        let args = vec!["-v=true".to_string(), "./...".to_string()];
        assert!(
            go_has_flag(&args, "-v"),
            "expected -v=true to be detected as -v"
        );
    }
}
