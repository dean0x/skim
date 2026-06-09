//! Playwright test parser with three-tier degradation (#118).
//!
//! Parses `playwright test` output into structured `TestResult`.
//!
//! Three tiers:
//! - **Tier 1 (JSON)**: `--reporter json` produces a JSON report
//! - **Tier 2 (regex)**: Falls back to regex on summary lines
//! - **Tier 3 (passthrough)**: Returns raw output unchanged

use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::ParseResult;
use crate::output::canonical::{TestEntry, TestOutcome, TestResult, TestSummary};

use super::shared::{
    ArgPreparation, MAX_ENTRIES, TestRunnerConfig, extract_json_object, run_test_runner,
};

// ============================================================================
// Tier-2 regex patterns
// ============================================================================

/// Playwright summary: `3 passed (5s)` or `2 failed`
static RE_PW_PASSED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s+passed").expect("valid regex"));
static RE_PW_FAILED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s+failed").expect("valid regex"));
static RE_PW_SKIPPED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s+skipped").expect("valid regex"));

// ============================================================================
// Failure regex for scrape (Tier 2)
// ============================================================================

static RE_PW_FAIL_LINE: LazyLock<Regex> = LazyLock::new(|| {
    // Playwright marks failures with `✘` or `×` prefixed lines
    Regex::new(r"^\s*[✘×]\s+(.+?)(?:\s+\(\d+ms\))?$").expect("valid regex")
});

// ============================================================================
// Public entry point
// ============================================================================

/// Run `playwright test [args...]`.
///
/// Strips the `test` subcommand token (already verified by the dispatcher)
/// so that `should_read_stdin` sees the real user args. The subcommand is
/// re-prepended in `prepare_args` for the spawn path.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    let config = TestRunnerConfig {
        program: "playwright",
        install_hint: "Install Playwright (npm install -D @playwright/test)",
        node_fallback: true,
        env_overrides: &[],
    };

    let user_args = if args.first().map(String::as_str) == Some("test") {
        &args[1..]
    } else {
        args
    };

    run_test_runner(
        &config,
        user_args,
        show_stats,
        rec,
        ArgPreparation {
            // Passthrough: prepend "test" subcommand only — no reporter flag.
            passthrough: |a: &[String]| {
                let mut final_args = vec!["test".to_string()];
                final_args.extend_from_slice(a);
                final_args
            },
            // Normal: prepend "test" and inject the JSON reporter flag.
            normal: |a: &[String]| {
                let mut final_args = vec!["test".to_string()];
                final_args.extend_from_slice(a);
                if !user_has_flag(a, &["--reporter"]) {
                    final_args.push("--reporter=json".to_string());
                }
                final_args
            },
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
            vec!["playwright: JSON parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(raw.to_string())
}

// ============================================================================
// Tier 1: JSON parsing
// ============================================================================

/// Playwright JSON report structure (subset).
#[derive(serde::Deserialize)]
struct PlaywrightReport {
    #[serde(default)]
    suites: Vec<PlaywrightSuite>,
    stats: Option<PlaywrightStats>,
}

#[derive(serde::Deserialize, Default)]
struct PlaywrightStats {
    #[serde(default)]
    expected: usize,
    #[serde(default)]
    unexpected: usize,
    #[serde(default)]
    flaky: usize,
    #[serde(default)]
    skipped: usize,
    #[serde(default)]
    duration: f64,
}

#[derive(serde::Deserialize)]
struct PlaywrightSuite {
    #[serde(default)]
    suites: Vec<PlaywrightSuite>,
    #[serde(default)]
    specs: Vec<PlaywrightSpec>,
}

#[derive(serde::Deserialize)]
struct PlaywrightSpec {
    title: Option<String>,
    #[serde(default)]
    tests: Vec<PlaywrightTest>,
}

#[derive(serde::Deserialize)]
struct PlaywrightTest {
    #[serde(default)]
    results: Vec<PlaywrightResult>,
}

#[derive(serde::Deserialize)]
struct PlaywrightResult {
    status: Option<String>,
    error: Option<PlaywrightError>,
}

#[derive(serde::Deserialize)]
struct PlaywrightError {
    message: Option<String>,
    snippet: Option<String>,
}

fn try_parse_json(raw: &str) -> Option<TestResult> {
    let cleaned = crate::output::strip_ansi_cow(raw);

    // Find the JSON object via brace balance
    let json_str = extract_json_object(&cleaned)?;
    let report: PlaywrightReport = serde_json::from_str(json_str).ok()?;

    let mut entries: Vec<TestEntry> = Vec::new();
    collect_entries_from_suites(&report.suites, &mut entries);

    let (pass, fail, skip) = if let Some(stats) = &report.stats {
        (
            stats.expected + stats.flaky,
            stats.unexpected,
            stats.skipped,
        )
    } else {
        // Compute from entries
        let pass = entries
            .iter()
            .filter(|e| e.outcome == TestOutcome::Pass)
            .count();
        let fail = entries
            .iter()
            .filter(|e| e.outcome == TestOutcome::Fail)
            .count();
        let skip = entries
            .iter()
            .filter(|e| e.outcome == TestOutcome::Skip)
            .count();
        (pass, fail, skip)
    };

    let duration_ms = report.stats.as_ref().map(|s| s.duration as u64);

    let summary = TestSummary {
        pass,
        fail,
        skip,
        duration_ms,
    };

    Some(TestResult::new(summary, entries))
}

/// Recursively collect test entries from nested suites.
///
/// `depth` tracks call depth; recursion stops at `MAX_SUITE_DEPTH` to prevent
/// stack overflows from pathologically-deep or adversarial JSON payloads.
/// `MAX_ENTRIES` (from `shared`) caps total entries collected to match the
/// Tier-2 regex path and prevent unbounded accumulation from wide payloads.
const MAX_SUITE_DEPTH: usize = 64;

fn collect_entries_from_suites(suites: &[PlaywrightSuite], entries: &mut Vec<TestEntry>) {
    collect_entries_from_suites_inner(suites, entries, 0);
}

fn collect_entries_from_suites_inner(
    suites: &[PlaywrightSuite],
    entries: &mut Vec<TestEntry>,
    depth: usize,
) {
    if depth >= MAX_SUITE_DEPTH || entries.len() >= MAX_ENTRIES {
        return;
    }
    for suite in suites {
        if entries.len() >= MAX_ENTRIES {
            return;
        }
        collect_entries_from_suites_inner(&suite.suites, entries, depth + 1);
        for spec in &suite.specs {
            if entries.len() >= MAX_ENTRIES {
                return;
            }
            for test in &spec.tests {
                // Only use the first result per test
                if let Some(result) = test.results.first() {
                    let status = result.status.as_deref().unwrap_or("unknown");
                    let outcome = match status {
                        "expected" | "flaky" => TestOutcome::Pass,
                        "unexpected" | "timedOut" => TestOutcome::Fail,
                        _ => TestOutcome::Skip,
                    };

                    let detail = result.error.as_ref().map(|e| {
                        let mut parts = Vec::new();
                        if let Some(msg) = &e.message {
                            parts.push(msg.clone());
                        }
                        if let Some(snippet) = &e.snippet {
                            parts.push(snippet.clone());
                        }
                        parts.join("\n")
                    });

                    entries.push(TestEntry {
                        name: spec.title.clone().unwrap_or_default(),
                        outcome,
                        detail,
                    });
                }
            }
        }
    }
}

// ============================================================================
// Tier 2: Regex fallback
// ============================================================================

fn try_parse_regex(raw: &str) -> Option<TestResult> {
    let cleaned = crate::output::strip_ansi_cow(raw);

    let pass = RE_PW_PASSED
        .captures(&cleaned)
        .and_then(|c| c[1].parse::<usize>().ok())
        .unwrap_or(0);
    let fail = RE_PW_FAILED
        .captures(&cleaned)
        .and_then(|c| c[1].parse::<usize>().ok())
        .unwrap_or(0);
    let skip = RE_PW_SKIPPED
        .captures(&cleaned)
        .and_then(|c| c[1].parse::<usize>().ok())
        .unwrap_or(0);

    // Only proceed if we found at least one count
    if pass == 0 && fail == 0 && skip == 0 {
        return None;
    }

    let entries = if fail > 0 {
        // Scrape failure names from playwright output
        let mut entries = Vec::new();
        for line in cleaned.lines() {
            if let Some(caps) = RE_PW_FAIL_LINE.captures(line) {
                let name = caps[1].trim().to_string();
                if !name.is_empty() {
                    entries.push(TestEntry {
                        name,
                        outcome: TestOutcome::Fail,
                        detail: None,
                    });
                }
            }
            if entries.len() >= 100 {
                break;
            }
        }
        entries
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

    const PW_PASS_JSON: &str = r#"{"version":"1.40.0","stats":{"startTime":"2026-01-01T00:00:00.000Z","duration":1234,"expected":3,"unexpected":0,"flaky":0,"skipped":0},"suites":[{"title":"home","file":"tests/home.spec.ts","suites":[],"specs":[{"title":"has title","ok":true,"tests":[{"timeout":30000,"annotations":[],"expectedStatus":"passed","projectId":"chromium","results":[{"status":"expected","duration":500,"error":null}]}]}]},{"title":"login","file":"tests/login.spec.ts","suites":[],"specs":[{"title":"can login","ok":true,"tests":[{"timeout":30000,"annotations":[],"expectedStatus":"passed","projectId":"chromium","results":[{"status":"expected","duration":600,"error":null}]},{"timeout":30000,"annotations":[],"expectedStatus":"passed","projectId":"firefox","results":[{"status":"expected","duration":700,"error":null}]}]}]}]}"#;

    const PW_FAIL_JSON: &str = r#"{"version":"1.40.0","stats":{"startTime":"2026-01-01T00:00:00.000Z","duration":2345,"expected":1,"unexpected":1,"flaky":0,"skipped":0},"suites":[{"title":"home","file":"tests/home.spec.ts","suites":[],"specs":[{"title":"has title","ok":true,"tests":[{"timeout":30000,"annotations":[],"expectedStatus":"passed","projectId":"chromium","results":[{"status":"expected","duration":500,"error":null}]}]},{"title":"should work","ok":false,"tests":[{"timeout":30000,"annotations":[],"expectedStatus":"passed","projectId":"chromium","results":[{"status":"unexpected","duration":800,"error":{"message":"Expected 'Hello' to equal 'World'","snippet":"  123 |   await expect(page).toHaveTitle('World');\n  > 124 |   expect(title).toBe('World');\n        |                 ^\n  125 | }"}}]}]}]}]}"#;

    const PW_REGEX_TEXT: &str = "Running 5 tests using 2 workers\n  ✘ tests/login.spec.ts:10:5 › login › should login\n  ✓ tests/home.spec.ts:5:5 › home › has title\n\n  3 passed (2.3s)\n  1 failed\n";

    #[test]
    fn test_playwright_tier1_pass() {
        let result = try_parse_json(PW_PASS_JSON);
        assert!(result.is_some(), "Expected JSON parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.summary.fail, 0);
        assert_eq!(r.summary.pass, 3);
    }

    #[test]
    fn test_playwright_tier1_fail() {
        let result = try_parse_json(PW_FAIL_JSON);
        assert!(result.is_some(), "Expected JSON parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.summary.fail, 1);
        assert_eq!(r.summary.pass, 1);
        // Verify error detail is preserved
        let failed = r.entries.iter().find(|e| e.outcome == TestOutcome::Fail);
        assert!(failed.is_some(), "Should have a failed entry");
        let f = failed.unwrap();
        assert!(
            f.detail.as_ref().is_some_and(|d| d.contains("Expected")),
            "Detail should contain error message, got: {:?}",
            f.detail
        );
    }

    #[test]
    fn test_playwright_tier2_regex() {
        let result = try_parse_regex(PW_REGEX_TEXT);
        assert!(result.is_some(), "Expected regex parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.summary.pass, 3);
        assert_eq!(r.summary.fail, 1);
    }

    #[test]
    fn test_playwright_tier3_passthrough() {
        let result = parse("completely unparseable output with no test info");
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_playwright_parse_produces_full_on_json() {
        let result = parse(PW_FAIL_JSON);
        assert!(
            result.is_full(),
            "Expected Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_playwright_parse_produces_degraded_on_text() {
        let result = parse(PW_REGEX_TEXT);
        assert!(
            result.is_degraded(),
            "Expected Degraded, got {}",
            result.tier_name()
        );
    }

    /// Regression: a wide JSON payload with more than MAX_ENTRIES suites at depth 1
    /// must not accumulate more than MAX_ENTRIES entries (unbounded accumulation guard).
    #[test]
    fn test_collect_entries_capped_at_max_entries() {
        // Build 200 suites each with one passing spec — well above MAX_ENTRIES.
        let spec = r#"{"title":"t","ok":true,"tests":[{"timeout":30000,"annotations":[],"expectedStatus":"passed","projectId":"chromium","results":[{"status":"expected","duration":10,"error":null}]}]}"#;
        let suite_body = format!(r#"{{"title":"s","file":"f.ts","suites":[],"specs":[{spec}]}}"#);
        let suites_array = std::iter::repeat_n(suite_body.as_str(), 200)
            .collect::<Vec<_>>()
            .join(",");
        let json = format!(
            r#"{{"version":"1.40.0","stats":{{"startTime":"2026-01-01T00:00:00.000Z","duration":1000,"expected":200,"unexpected":0,"flaky":0,"skipped":0}},"suites":[{suites_array}]}}"#
        );

        let result = try_parse_json(&json);
        assert!(result.is_some(), "JSON parse should succeed");
        let r = result.unwrap();
        assert!(
            r.entries.len() <= MAX_ENTRIES,
            "entries must not exceed MAX_ENTRIES={MAX_ENTRIES}, got {}",
            r.entries.len()
        );
    }
}
