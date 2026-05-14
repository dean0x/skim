//! .NET `dotnet test` parser with three-tier degradation (#118).
//!
//! Parses `dotnet test` output into structured `TestResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Inject `--logger trx`, parse TRX XML file detected
//!   from stdout, extract counters and test results.
//! - **Tier 2 (Degraded)**: Regex on console summary lines.
//! - **Tier 3 (Passthrough)**: Returns raw output unchanged.

use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::ParseResult;
use crate::output::canonical::{TestEntry, TestOutcome, TestResult, TestSummary};

use super::shared::{TestRunnerConfig, run_test_runner};

// ============================================================================
// Regex patterns
// ============================================================================

/// Detect TRX file path from dotnet test stdout:
/// `Results File: /path/to/TestResults/xxx.trx`
static RE_TRX_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Results File:\s+(.+\.trx)").expect("valid regex"));

/// Console summary: `Passed! - Failed: 0, Passed: 5, Skipped: 0, Total: 5, Duration: 2s`
static RE_DOTNET_SUMMARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Failed:\s*(\d+),\s*Passed:\s*(\d+),\s*(?:Skipped:\s*(\d+),\s*)?Total:\s*(\d+)")
        .expect("valid regex")
});

/// Failed test: `  Failed ClassName.MethodName [Nms]`
static RE_DOTNET_FAILED_TEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+Failed\s+(\S+)\s*\[").expect("valid regex"));

// ============================================================================
// Public entry point
// ============================================================================

/// Run `dotnet test [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    let config = TestRunnerConfig {
        program: "dotnet",
        install_hint: "Install .NET SDK from https://dotnet.microsoft.com/download",
        node_fallback: false,
        env_overrides: &[("DOTNET_CLI_UI_LANGUAGE", "en-US")],
    };

    run_test_runner(
        &config,
        args,
        show_stats,
        rec,
        |a| {
            // Prepend "test" and inject --logger trx for TRX XML output.
            let mut final_args = vec!["test".to_string()];
            final_args.extend_from_slice(a);
            if !user_has_flag(a, &["--logger"]) {
                final_args.push("--logger".to_string());
                final_args.push("trx".to_string());
            }
            final_args
        },
        |raw| {
            // TRX detection: if the spawn produced a Results File path, read it
            // and embed the TRX XML so the parser can use Tier-1 (XML) rather
            // than Tier-2 (regex).
            if let Some(trx_path) = extract_trx_path(raw)
                && let Ok(trx_content) = std::fs::read_to_string(&trx_path)
            {
                let with_trx = format!("__TRX_CONTENT__\n{trx_content}\n__END_TRX__\n{raw}");
                parse(&with_trx)
            } else {
                parse(raw)
            }
        },
    )
}

fn extract_trx_path(text: &str) -> Option<String> {
    RE_TRX_PATH.captures(text).map(|c| c[1].trim().to_string())
}

// ============================================================================
// Three-tier parser
// ============================================================================

fn parse(raw: &str) -> ParseResult<TestResult> {
    // Tier 1: TRX XML (if available in raw output)
    if let Some(result) = try_parse_trx(raw) {
        return ParseResult::Full(result);
    }

    // Tier 2: Regex on console output
    if let Some(result) = try_parse_regex(raw) {
        return ParseResult::Degraded(
            result,
            vec!["dotnet: TRX parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(raw.to_string())
}

/// Parse TRX XML content embedded in the raw output.
fn try_parse_trx(raw: &str) -> Option<TestResult> {
    // Extract TRX content from the embedded marker
    let trx_content = if raw.contains("__TRX_CONTENT__") {
        let start = raw.find("__TRX_CONTENT__\n")? + "__TRX_CONTENT__\n".len();
        let end = raw.find("\n__END_TRX__")?;
        &raw[start..end]
    } else {
        // Also handle direct TRX XML (for stdin piping)
        if raw.trim_start().starts_with("<?xml") || raw.contains("<TestRun") {
            raw
        } else {
            return None;
        }
    };

    parse_trx_xml(trx_content)
}

/// Parse TRX XML using quick-xml.
///
/// TRX format (subset):
/// ```xml
/// <TestRun>
///   <ResultSummary outcome="Completed">
///     <Counters total="5" executed="5" passed="4" failed="1" />
///   </ResultSummary>
///   <Results>
///     <UnitTestResult testName="MyTest" outcome="Passed" duration="00:00:00.5" />
///     <UnitTestResult testName="FailTest" outcome="Failed" duration="00:00:01.0">
///       <Output><ErrorInfo><Message>Expected 1 but was 2</Message></ErrorInfo></Output>
///     </UnitTestResult>
///   </Results>
/// </TestRun>
/// ```
fn parse_trx_xml(xml: &str) -> Option<TestResult> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut total = 0usize;
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut entries: Vec<TestEntry> = Vec::new();
    let mut current_test_name: Option<String> = None;
    let mut current_outcome: Option<TestOutcome> = None;
    let mut current_error: Option<String> = None;
    let mut in_error_message = false;
    let mut found_counters = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                match e.name().as_ref() {
                    b"Counters" => {
                        // Parse the Counters element attributes
                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"total" => {
                                    total = std::str::from_utf8(&attr.value)
                                        .ok()
                                        .and_then(|s| s.parse().ok())
                                        .unwrap_or(0);
                                    found_counters = true;
                                }
                                b"passed" => {
                                    passed = std::str::from_utf8(&attr.value)
                                        .ok()
                                        .and_then(|s| s.parse().ok())
                                        .unwrap_or(0);
                                }
                                b"failed" => {
                                    failed = std::str::from_utf8(&attr.value)
                                        .ok()
                                        .and_then(|s| s.parse().ok())
                                        .unwrap_or(0);
                                }
                                _ => {}
                            }
                        }
                        skipped = total.saturating_sub(passed + failed);
                    }
                    b"UnitTestResult" => {
                        let mut name = None;
                        let mut outcome_str = None;
                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"testName" => {
                                    name = std::str::from_utf8(&attr.value)
                                        .ok()
                                        .map(|s| s.to_string());
                                }
                                b"outcome" => {
                                    outcome_str = std::str::from_utf8(&attr.value)
                                        .ok()
                                        .map(|s| s.to_string());
                                }
                                _ => {}
                            }
                        }
                        current_test_name = name;
                        current_outcome = outcome_str.as_deref().map(|s| match s {
                            "Passed" => TestOutcome::Pass,
                            "Failed" => TestOutcome::Fail,
                            _ => TestOutcome::Skip,
                        });
                        current_error = None;
                    }
                    b"Message" => {
                        in_error_message = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                if in_error_message {
                    current_error = e.unescape().ok().map(|s| s.into_owned());
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"UnitTestResult" => {
                    if let (Some(name), Some(outcome)) =
                        (current_test_name.take(), current_outcome.take())
                    {
                        entries.push(TestEntry {
                            name,
                            outcome,
                            detail: current_error.take(),
                        });
                    }
                }
                b"Message" => {
                    in_error_message = false;
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
    }

    if !found_counters && entries.is_empty() {
        return None;
    }

    // If counters were not found, derive from entries
    if !found_counters {
        passed = entries
            .iter()
            .filter(|e| e.outcome == TestOutcome::Pass)
            .count();
        failed = entries
            .iter()
            .filter(|e| e.outcome == TestOutcome::Fail)
            .count();
        // total is used for skipped above; here it stays 0 since we can't derive it
    }

    let summary = TestSummary {
        pass: passed,
        fail: failed,
        skip: skipped,
        duration_ms: None,
    };

    Some(TestResult::new(summary, entries))
}

// ============================================================================
// Tier 2: Regex fallback
// ============================================================================

fn try_parse_regex(raw: &str) -> Option<TestResult> {
    let cleaned = crate::output::strip_ansi(raw);

    let caps = RE_DOTNET_SUMMARY.captures(&cleaned)?;
    let fail: usize = caps[1].parse().ok()?;
    let pass: usize = caps[2].parse().ok()?;
    let skip: usize = caps
        .get(3)
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0);

    // Scrape failing test names
    let entries: Vec<TestEntry> = if fail > 0 {
        cleaned
            .lines()
            .filter_map(|line| {
                RE_DOTNET_FAILED_TEST.captures(line).map(|c| TestEntry {
                    name: c[1].to_string(),
                    outcome: TestOutcome::Fail,
                    detail: None,
                })
            })
            .take(100)
            .collect()
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

    const TRX_PASS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<TestRun id="abc" name="Test Run" xmlns="http://microsoft.com/schemas/VisualStudio/TeamTest/2010">
  <ResultSummary outcome="Completed">
    <Counters total="3" executed="3" passed="3" failed="0" error="0" timeout="0" aborted="0" inconclusive="0" passedButRunAborted="0" notRunnable="0" notExecuted="0" disconnected="0" warning="0" completed="0" inProgress="0" pending="0" />
  </ResultSummary>
  <Results>
    <UnitTestResult testName="MyTests.TestAdd" outcome="Passed" duration="00:00:00.0100000" />
    <UnitTestResult testName="MyTests.TestSubtract" outcome="Passed" duration="00:00:00.0050000" />
    <UnitTestResult testName="MyTests.TestMultiply" outcome="Passed" duration="00:00:00.0080000" />
  </Results>
</TestRun>"#;

    const TRX_FAIL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<TestRun id="abc" name="Test Run" xmlns="http://microsoft.com/schemas/VisualStudio/TeamTest/2010">
  <ResultSummary outcome="Failed">
    <Counters total="2" executed="2" passed="1" failed="1" error="0" timeout="0" aborted="0" inconclusive="0" passedButRunAborted="0" notRunnable="0" notExecuted="0" disconnected="0" warning="0" completed="0" inProgress="0" pending="0" />
  </ResultSummary>
  <Results>
    <UnitTestResult testName="MyTests.TestAdd" outcome="Passed" duration="00:00:00.0100000" />
    <UnitTestResult testName="MyTests.TestDivide" outcome="Failed" duration="00:00:00.0150000">
      <Output><ErrorInfo><Message>Expected 2 but was 3</Message></ErrorInfo></Output>
    </UnitTestResult>
  </Results>
</TestRun>"#;

    const DOTNET_CONSOLE_SUMMARY: &str = "Test run for /bin/MyProject.dll (.NETCoreApp,Version=v8.0)\nMicrosoft (R) Test Execution Command Line Tool Version 17.0\n\nPassed! - Failed: 0, Passed: 5, Skipped: 0, Total: 5, Duration: 2s - MyProject.dll (net8.0)\n";

    #[test]
    fn test_dotnet_trx_pass() {
        let result = parse_trx_xml(TRX_PASS);
        assert!(result.is_some(), "Expected TRX parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.summary.fail, 0);
        assert_eq!(r.summary.pass, 3);
    }

    #[test]
    fn test_dotnet_trx_fail() {
        let result = parse_trx_xml(TRX_FAIL);
        assert!(result.is_some(), "Expected TRX parse to succeed");
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
                .map(|d| d.contains("Expected 2"))
                .unwrap_or(false),
            "Detail should contain error message"
        );
    }

    #[test]
    fn test_dotnet_tier2_regex() {
        let result = try_parse_regex(DOTNET_CONSOLE_SUMMARY);
        assert!(result.is_some(), "Expected regex parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.summary.pass, 5);
        assert_eq!(r.summary.fail, 0);
    }

    #[test]
    fn test_dotnet_tier3_passthrough() {
        let result = parse("completely unparseable output");
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_dotnet_parse_full_on_trx_stdin() {
        // When TRX XML is piped as stdin
        let result = parse(TRX_PASS);
        assert!(
            result.is_full(),
            "Expected Full on direct TRX XML, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_dotnet_parse_degraded_on_console() {
        let result = parse(DOTNET_CONSOLE_SUMMARY);
        assert!(
            result.is_degraded(),
            "Expected Degraded on console output, got {}",
            result.tier_name()
        );
    }
}
