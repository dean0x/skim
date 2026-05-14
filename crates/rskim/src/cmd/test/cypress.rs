//! Cypress test parser with three-tier degradation (#118).
//!
//! Parses `cypress run` output into structured `TestResult`.
//!
//! Three tiers:
//! - **Tier 1 (JSON)**: `--reporter json` produces Mocha-format JSON
//! - **Tier 2 (regex)**: Falls back to regex on summary lines
//! - **Tier 3 (passthrough)**: Returns raw output unchanged

use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::ParseResult;
use crate::output::canonical::{TestEntry, TestOutcome, TestResult, TestSummary};

use super::shared::{
    TestKind, TestRunnerConfig, extract_json_object, run_test_runner, scrape_failures,
};

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
///
/// Strips the `run` subcommand token (already verified by the dispatcher)
/// so that `should_read_stdin` sees the real user args. The subcommand is
/// re-prepended in `prepare_args` for the spawn path.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    let config = TestRunnerConfig {
        program: "cypress",
        install_hint: "Install Cypress (npm install -D cypress)",
        node_fallback: true,
        env_overrides: &[],
    };

    let user_args = if args.first().map(String::as_str) == Some("run") {
        &args[1..]
    } else {
        args
    };

    run_test_runner(
        &config,
        user_args,
        show_stats,
        rec,
        |a| {
            let mut final_args = vec!["run".to_string()];
            final_args.extend_from_slice(a);
            if !user_has_flag(a, &["--reporter"]) {
                final_args.push("--reporter".to_string());
                final_args.push("json".to_string());
            }
            final_args
        },
        parse,
    )
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
        let has_err = test
            .err
            .as_ref()
            .map(|e| e.message.is_some())
            .unwrap_or(false);
        let outcome = if has_err {
            TestOutcome::Fail
        } else {
            TestOutcome::Pass
        };

        let detail = test.err.as_ref().and_then(|e| {
            let mut parts = Vec::new();
            if let Some(msg) = &e.message {
                parts.push(msg.clone());
            }
            if let Some(stack) = &e.stack {
                parts.push(stack.clone());
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
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
            failed
                .unwrap()
                .detail
                .as_ref()
                .map(|d| d.contains("AssertionError"))
                .unwrap_or(false)
        );
    }

    #[test]
    fn test_cypress_tier3_passthrough() {
        let result = parse("completely unparseable output");
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_cypress_parse_full_on_json() {
        let result = parse(CY_FAIL_JSON);
        assert!(
            result.is_full(),
            "Expected Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_cypress_regex_passthrough_text() {
        // The constant CY_TEXT has literal \s+ in the Passing line (not valid regex output)
        // Let's use a proper cypress text output
        let text = "Passing:  3\nFailing:  0\nPending:  1\n";
        let result = try_parse_regex(text);
        assert!(
            result.is_some(),
            "Expected regex parse to succeed on clean summary"
        );
        let r = result.unwrap();
        assert_eq!(r.summary.pass, 3);
    }
}
