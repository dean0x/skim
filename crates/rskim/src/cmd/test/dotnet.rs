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

use super::shared::{ArgPreparation, TestRunnerConfig, run_test_runner};

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
// Constants
// ============================================================================

/// Maximum TRX file size accepted before reading. Files larger than this are
/// rejected to bound peak memory usage (the read + format! produces ~2× the
/// file size in heap).
const TRX_MAX_BYTES: u64 = 50 * 1024 * 1024; // 50 MB

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
        ArgPreparation {
            // Passthrough: prepend "test" subcommand only — no --logger flag.
            passthrough: |a: &[String]| {
                let mut final_args = vec!["test".to_string()];
                final_args.extend_from_slice(a);
                final_args
            },
            // Normal: prepend "test" and inject --logger trx for TRX XML output.
            normal: |a: &[String]| {
                let mut final_args = vec!["test".to_string()];
                final_args.extend_from_slice(a);
                if !user_has_flag(a, &["--logger"]) {
                    final_args.push("--logger".to_string());
                    final_args.push("trx".to_string());
                }
                final_args
            },
        },
        |raw| {
            // TRX detection: if the spawn produced a Results File path, read it
            // and embed the TRX XML so the parser can use Tier-1 (XML) rather
            // than Tier-2 (regex). The path is validated (extension,
            // regular-file, directory bounds) before reading.
            if let Some(trx_path) = extract_trx_path(raw)
                && std::fs::metadata(&trx_path)
                    .map(|m| m.len() <= TRX_MAX_BYTES)
                    .unwrap_or(false)
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
    RE_TRX_PATH.captures(text).and_then(|c| {
        let raw = c[1].trim().to_string();
        validate_trx_path(&raw).then_some(raw)
    })
}

/// Validate a TRX file path before reading it.
///
/// Accepts a path only when ALL of the following hold:
/// 1. The path ends with `.trx` (case-insensitive) — rejects non-TRX files.
/// 2. The file is a regular file — rejects symlinks to prevent symlink-following
///    to arbitrary filesystem locations.
/// 3. The canonical path stays within the `TestResults` subtree or the current
///    working directory tree — prevents path traversal outside the build output.
fn validate_trx_path(path: &str) -> bool {
    use std::path::Path;

    let p = Path::new(path);

    // Rule 1: must end with .trx
    let is_trx = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("trx"))
        .unwrap_or(false);
    if !is_trx {
        return false;
    }

    // Rule 2: must be a regular file (not a symlink)
    let meta = match p.symlink_metadata() {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !meta.is_file() {
        return false;
    }

    // Rule 3: canonical path must be under TestResults or the cwd
    let canonical = match p.canonicalize() {
        Ok(c) => c,
        Err(_) => return false,
    };

    // Allow if any path component is "TestResults" (dotnet convention)
    let in_test_results = canonical
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("TestResults"));

    if in_test_results {
        return true;
    }

    // Allow if the path is within the current working directory tree
    if let Ok(cwd) = std::env::current_dir().and_then(|c| c.canonicalize()) {
        return canonical.starts_with(&cwd);
    }

    false
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

/// Counters parsed from a TRX `<Counters>` element.
struct TrxCounters {
    total: usize,
    passed: usize,
    failed: usize,
}

/// Parse the `total`, `passed`, and `failed` attributes from a `<Counters>`
/// element. Returns `None` if the element carries no recognisable attributes.
fn parse_counters_element<'a>(
    attrs: impl Iterator<Item = quick_xml::events::attributes::Attribute<'a>>,
) -> Option<TrxCounters> {
    let mut total = 0usize;
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut found = false;

    for attr in attrs {
        match attr.key.as_ref() {
            b"total" => {
                total = std::str::from_utf8(&attr.value)
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                found = true;
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

    found.then_some(TrxCounters {
        total,
        passed,
        failed,
    })
}

/// Parse `testName` and `outcome` attributes from a `<UnitTestResult>` element.
///
/// Returns `(name, outcome)` — either field may be `None` if the attribute is
/// absent or unrecognised.
fn parse_unit_test_result_attrs<'a>(
    attrs: impl Iterator<Item = quick_xml::events::attributes::Attribute<'a>>,
) -> (Option<String>, Option<TestOutcome>) {
    let mut name: Option<String> = None;
    let mut outcome: Option<TestOutcome> = None;

    for attr in attrs {
        match attr.key.as_ref() {
            b"testName" => {
                name = std::str::from_utf8(&attr.value).ok().map(|s| s.to_string());
            }
            b"outcome" => {
                outcome = std::str::from_utf8(&attr.value).ok().map(|s| match s {
                    "Passed" => TestOutcome::Pass,
                    "Failed" => TestOutcome::Fail,
                    _ => TestOutcome::Skip,
                });
            }
            _ => {}
        }
    }

    (name, outcome)
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
///
/// Element parsing is delegated to focused helpers:
/// - [`parse_counters_element`] — extracts totals from `<Counters>` attributes
/// - [`parse_unit_test_result_attrs`] — extracts name/outcome from `<UnitTestResult>`
fn parse_trx_xml(xml: &str) -> Option<TestResult> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut counters: Option<TrxCounters> = None;
    let mut entries: Vec<TestEntry> = Vec::new();
    let mut current_test_name: Option<String> = None;
    let mut current_outcome: Option<TestOutcome> = None;
    let mut current_error: Option<String> = None;
    let mut in_error_message = false;

    loop {
        match reader.read_event() {
            Ok(Event::Empty(e)) => match e.name().as_ref() {
                // Self-closing elements have no End event — handle them immediately.
                b"Counters" => {
                    counters = parse_counters_element(e.attributes().flatten());
                }
                b"UnitTestResult" => {
                    // Self-closing: push the entry now (no End event will follow).
                    let (name, outcome) = parse_unit_test_result_attrs(e.attributes().flatten());
                    if let (Some(name), Some(outcome)) = (name, outcome) {
                        entries.push(TestEntry {
                            name,
                            outcome,
                            detail: None,
                        });
                    }
                }
                _ => {}
            },
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"Counters" => {
                    counters = parse_counters_element(e.attributes().flatten());
                }
                b"UnitTestResult" => {
                    // Non-self-closing: accumulate pending state; entry pushed in End.
                    let (name, outcome) = parse_unit_test_result_attrs(e.attributes().flatten());
                    current_test_name = name;
                    current_outcome = outcome;
                    current_error = None;
                }
                b"Message" => {
                    in_error_message = true;
                }
                _ => {}
            },
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

    if counters.is_none() && entries.is_empty() {
        return None;
    }

    let (passed, failed, skipped) = match &counters {
        Some(c) => (
            c.passed,
            c.failed,
            c.total.saturating_sub(c.passed + c.failed),
        ),
        None => {
            // Derive from entries when no <Counters> element was found
            let p = entries
                .iter()
                .filter(|e| e.outcome == TestOutcome::Pass)
                .count();
            let f = entries
                .iter()
                .filter(|e| e.outcome == TestOutcome::Fail)
                .count();
            (p, f, 0)
        }
    };

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
    let cleaned = crate::output::strip_ansi_cow(raw);

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

    // --- validate_trx_path tests ---

    #[test]
    fn test_validate_trx_path_rejects_non_trx_extension() {
        // A path that ends with .xml should be rejected regardless of filesystem state
        assert!(
            !validate_trx_path("/some/TestResults/results.xml"),
            "non-.trx extension must be rejected"
        );
    }

    #[test]
    fn test_validate_trx_path_rejects_no_extension() {
        assert!(
            !validate_trx_path("/some/TestResults/results"),
            "path with no extension must be rejected"
        );
    }

    #[test]
    fn test_validate_trx_path_rejects_nonexistent_file() {
        // A well-formed path that does not exist on disk must be rejected
        // (symlink_metadata will fail).
        assert!(
            !validate_trx_path("/nonexistent/TestResults/run.trx"),
            "nonexistent path must be rejected"
        );
    }

    #[test]
    fn test_validate_trx_path_accepts_real_trx_in_test_results() {
        // Create a real temporary .trx file inside a TestResults directory and
        // verify validate_trx_path accepts it.
        let dir = std::env::temp_dir().join("TestResults");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("run_accept.trx");
        std::fs::write(&path, b"<TestRun/>").expect("write temp trx");
        let path_str = path.to_string_lossy().to_string();
        let result = validate_trx_path(&path_str);
        let _ = std::fs::remove_file(&path);
        assert!(
            result,
            "real .trx in TestResults directory must be accepted"
        );
    }

    #[test]
    fn test_validate_trx_path_rejects_path_outside_cwd_and_test_results() {
        // A .trx file in /tmp (not within a TestResults ancestor and not under
        // cwd) should be rejected when the cwd is elsewhere. This guards against
        // path traversal via a crafted "Results File:" line in process output.
        let tmp_path = std::env::temp_dir().join("escape_guard.trx");
        let _ = std::fs::write(&tmp_path, b"<TestRun/>");
        let cwd = std::env::current_dir().unwrap_or_default();
        let has_test_results = tmp_path
            .components()
            .any(|c| c.as_os_str().eq_ignore_ascii_case("TestResults"));
        if !tmp_path.starts_with(&cwd) && !has_test_results {
            let path_str = tmp_path.to_string_lossy().to_string();
            assert!(
                !validate_trx_path(&path_str),
                "path outside cwd and TestResults must be rejected"
            );
        }
        let _ = std::fs::remove_file(&tmp_path);
    }

    // --- parse_counters_element tests ---

    #[test]
    fn test_parse_counters_element_empty_returns_none() {
        let result = parse_counters_element(std::iter::empty());
        assert!(result.is_none(), "empty attributes must return None");
    }

    // --- parse_trx_xml without Counters element ---

    #[test]
    fn test_parse_trx_xml_derives_counts_from_entries_when_no_counters() {
        // TRX without a <Counters> element — counts must be derived from entries.
        // The xmlns attribute is intentionally omitted here: quick-xml uses raw
        // (non-namespace-aware) byte matching on element names, and the existing
        // TRX fixtures with xmlns work because Counters drives the summary there.
        // When there is no Counters element, entries are the only source — this
        // test validates that fallback path without triggering namespace quirks.
        let xml = r#"<?xml version="1.0"?>
<TestRun>
  <Results>
    <UnitTestResult testName="A" outcome="Passed" duration="00:00:00.01" />
    <UnitTestResult testName="B" outcome="Failed" duration="00:00:00.02">
      <Output><ErrorInfo><Message>Oops</Message></ErrorInfo></Output>
    </UnitTestResult>
  </Results>
</TestRun>"#;
        let result = parse_trx_xml(xml);
        assert!(result.is_some(), "should parse without Counters element");
        let r = result.unwrap();
        assert_eq!(r.summary.pass, 1);
        assert_eq!(r.summary.fail, 1);
        assert_eq!(r.summary.skip, 0);
    }
}
