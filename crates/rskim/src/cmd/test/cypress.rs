//! Cypress test parser with three-tier degradation (#118).
//!
//! Parses `cypress run` output into structured `TestResult`.
//!
//! Three tiers:
//! - **Tier 1 (JSON)**: `--reporter json` produces Mocha-format JSON
//! - **Tier 2 (regex)**: Falls back to regex on summary lines
//! - **Tier 3 (passthrough)**: Returns raw output unchanged

use std::io;
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::ParseResult;
use crate::output::canonical::{TestEntry, TestOutcome, TestResult, TestSummary};
use crate::runner::CommandRunner;

use super::shared::{self, TestKind, scrape_failures, try_read_stdin};

// ============================================================================
// Tier-2 regex patterns
// ============================================================================

static RE_CY_PASSING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Passing:\s+(\d+)").expect("valid regex"));
static RE_CY_FAILING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Failing:\s+(\d+)").expect("valid regex"));
static RE_CY_PENDING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Pending:\s+(\d+)").expect("valid regex"));

// ============================================================================
// Public entry point
// ============================================================================

/// Run `cypress run [args...]`.
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
                    .run_with_node_fallback("cypress", arg_refs)
            },
        );
    }

    let start = std::time::Instant::now();
    let raw_output = if let Some(stdin_content) = try_read_stdin(args)? {
        stdin_content
    } else {
        run_cypress(args)?
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
        crate::cmd::format_analytics_label("test", "cypress", &args.join(" ")),
        start.elapsed(),
    );

    Ok(exit_code)
}

fn run_cypress(args: &[String]) -> anyhow::Result<String> {
    let mut final_args: Vec<String> = args.to_vec();

    // Inject --reporter json unless already specified
    if !user_has_flag(args, &["--reporter"]) {
        final_args.push("--reporter".to_string());
        final_args.push("json".to_string());
    }

    let arg_refs: Vec<&str> = final_args.iter().map(String::as_str).collect();
    let runner = CommandRunner::new(Some(crate::cmd::DEFAULT_CMD_TIMEOUT));
    let output = runner.run_with_node_fallback("cypress", &arg_refs).map_err(|e| {
        anyhow::anyhow!(
            "failed to run cypress: {e}\n\
             Hint: Install Cypress (npm install -D cypress)"
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
    if let Some(result) = try_parse_json(raw) {
        return ParseResult::Full(result);
    }

    if let Some(result) = try_parse_regex(raw) {
        return ParseResult::Degraded(
            result,
            vec!["cypress: JSON parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(raw.to_string())
}

// ============================================================================
// Tier 1: JSON parsing (Mocha format)
// ============================================================================

/// Mocha JSON reporter structure.
#[derive(serde::Deserialize)]
struct MochaReport {
    stats: Option<MochaStats>,
    #[serde(default)]
    tests: Vec<MochaTest>,
}

#[derive(serde::Deserialize, Default)]
struct MochaStats {
    #[serde(default)]
    passes: usize,
    #[serde(default)]
    failures: usize,
    #[serde(default)]
    pending: usize,
    duration: Option<f64>,
}

#[derive(serde::Deserialize)]
struct MochaTest {
    #[serde(rename = "fullTitle")]
    full_title: Option<String>,
    err: Option<MochaError>,
}

#[derive(serde::Deserialize)]
struct MochaError {
    message: Option<String>,
    stack: Option<String>,
}

fn try_parse_json(raw: &str) -> Option<TestResult> {
    let cleaned = crate::output::strip_ansi(raw);

    // Find balanced JSON object
    let json_str = extract_json_object(&cleaned)?;
    let report: MochaReport = serde_json::from_str(json_str).ok()?;

    let stats = report.stats.unwrap_or_default();

    let mut entries: Vec<TestEntry> = Vec::new();
    for test in &report.tests {
        let has_err = test.err.as_ref().map(|e| e.message.is_some()).unwrap_or(false);
        let outcome = if has_err { TestOutcome::Fail } else { TestOutcome::Pass };

        let detail = test.err.as_ref().and_then(|e| {
            let mut parts = Vec::new();
            if let Some(msg) = &e.message {
                parts.push(msg.clone());
            }
            if let Some(stack) = &e.stack {
                parts.push(stack.clone());
            }
            if parts.is_empty() { None } else { Some(parts.join("\n")) }
        });

        entries.push(TestEntry {
            name: test.full_title.clone().unwrap_or_default(),
            outcome,
            detail,
        });
    }

    let summary = TestSummary {
        pass: stats.passes,
        fail: stats.failures,
        skip: stats.pending,
        duration_ms: stats.duration.map(|d| d as u64),
    };

    Some(TestResult::new(summary, entries))
}

/// Extract first balanced JSON object from text.
fn extract_json_object(text: &str) -> Option<&str> {
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

// ============================================================================
// Tier 2: Regex fallback
// ============================================================================

fn try_parse_regex(raw: &str) -> Option<TestResult> {
    let cleaned = crate::output::strip_ansi(raw);

    let pass = RE_CY_PASSING
        .captures(&cleaned)
        .and_then(|c| c[1].parse::<usize>().ok())
        .unwrap_or(0);
    let fail = RE_CY_FAILING
        .captures(&cleaned)
        .and_then(|c| c[1].parse::<usize>().ok())
        .unwrap_or(0);
    let skip = RE_CY_PENDING
        .captures(&cleaned)
        .and_then(|c| c[1].parse::<usize>().ok())
        .unwrap_or(0);

    if pass == 0 && fail == 0 && skip == 0 {
        return None;
    }

    let entries = if fail > 0 {
        scrape_failures(&cleaned, TestKind::Cypress)
    } else {
        vec![]
    };

    let summary = TestSummary {
        pass,
        fail,
        skip,
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

    const CY_PASS_JSON: &str = r#"{"stats":{"suites":1,"tests":3,"passes":3,"pending":0,"failures":0,"start":"2026-01-01T00:00:00.000Z","end":"2026-01-01T00:00:05.000Z","duration":5000},"tests":[{"fullTitle":"Login › should display form","duration":1200,"err":{}},{"fullTitle":"Login › should login successfully","duration":1800,"err":{}},{"fullTitle":"Login › should show error on invalid credentials","duration":2000,"err":{}}]}"#;

    const CY_FAIL_JSON: &str = r#"{"stats":{"suites":1,"tests":2,"passes":1,"pending":0,"failures":1,"start":"2026-01-01T00:00:00.000Z","end":"2026-01-01T00:00:05.000Z","duration":5000},"tests":[{"fullTitle":"Login › should display form","duration":1200,"err":{}},{"fullTitle":"Login › should login successfully","duration":1800,"err":{"message":"AssertionError: expected 'dashboard' to equal 'home'","stack":"AssertionError: expected 'dashboard' to equal 'home'\n    at Context.<anonymous>"}}]}"#;

    const CY_TEXT: &str = "  Running:  cypress/e2e/login.cy.js                                          (1 of 1)\n\n  Login\n    1) should login successfully\n    ✓ should display form\n\n\n  1 passing (5s)\n  1 failing\n\nPassing: 1\nFailing: 1\n";

    #[test]
    fn test_cypress_tier1_pass() {
        let result = try_parse_json(CY_PASS_JSON);
        assert!(result.is_some(), "Expected JSON parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.summary.fail, 0);
        assert_eq!(r.summary.pass, 3);
    }

    #[test]
    fn test_cypress_tier1_fail() {
        let result = try_parse_json(CY_FAIL_JSON);
        assert!(result.is_some(), "Expected JSON parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.summary.fail, 1);
        assert_eq!(r.summary.pass, 1);
        let failed = r.entries.iter().find(|e| e.outcome == TestOutcome::Fail);
        assert!(failed.is_some());
        assert!(
            failed.unwrap().detail.as_ref().map(|d| d.contains("AssertionError")).unwrap_or(false)
        );
    }

    #[test]
    fn test_cypress_tier3_passthrough() {
        let result = parse("completely unparseable output");
        assert!(result.is_passthrough(), "Expected Passthrough, got {}", result.tier_name());
    }

    #[test]
    fn test_cypress_parse_full_on_json() {
        let result = parse(CY_FAIL_JSON);
        assert!(result.is_full(), "Expected Full, got {}", result.tier_name());
    }

    #[test]
    fn test_cypress_regex_passthrough_text() {
        // The constant CY_TEXT has literal \s+ in the Passing line (not valid regex output)
        // Let's use a proper cypress text output
        let text = "Passing:  3\nFailing:  0\nPending:  1\n";
        let result = try_parse_regex(text);
        assert!(result.is_some(), "Expected regex parse to succeed on clean summary");
        let r = result.unwrap();
        assert_eq!(r.summary.pass, 3);
    }
}
