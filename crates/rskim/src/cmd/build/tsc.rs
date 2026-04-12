//! TypeScript compiler output compression (#51)
//!
//! Three-tier parser for `tsc` output:
//!
//! - **Tier 1 (regex on stderr):** Parse tsc error format
//!   `file(line,col): error TSxxxx: message` from stderr.
//!
//! - **Tier 2 (regex on combined):** Same regex on combined stdout+stderr
//!   in case tsc writes to an unexpected stream.
//!
//! - **Tier 3 (passthrough):** Return raw output when nothing can be parsed.

use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use super::run_parsed_command;
use crate::output::canonical::BuildResult;
use crate::output::ParseResult;
use crate::runner::CommandOutput;

// ============================================================================
// Public entry point
// ============================================================================

/// Run `tsc` with output compression.
///
/// tsc writes errors to stderr in its standard format. No flag injection
/// is needed (unlike cargo).
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    analytics_enabled: bool,
) -> anyhow::Result<ExitCode> {
    run_parsed_command(
        "tsc",
        args,
        &[],
        "npm install -g typescript",
        show_stats,
        analytics_enabled,
        parse_tsc,
    )
}

// ============================================================================
// Three-tier parser
// ============================================================================

/// Parse tsc output through three degradation tiers.
fn parse_tsc(output: &CommandOutput) -> ParseResult<BuildResult> {
    // Tier 1: Regex on stderr (primary tsc output stream)
    if let Some(result) = try_tier1_regex(&output.stderr) {
        return result;
    }

    // Tier 2: Regex on combined stdout+stderr (in case tsc writes to unexpected stream)
    let combined = format!("{}\n{}", output.stdout, output.stderr);
    if let Some(result) = try_tier2_combined(&combined) {
        return result;
    }

    // If both stdout and stderr are empty/whitespace, it's a successful build
    if output.stdout.trim().is_empty() && output.stderr.trim().is_empty() {
        let result = BuildResult::new(true, 0, 0, None, vec![]);
        return ParseResult::Full(result);
    }

    // Tier 3: Passthrough
    let passthrough = if output.stderr.is_empty() {
        output.stdout.clone()
    } else if output.stdout.is_empty() {
        output.stderr.clone()
    } else {
        combined
    };

    ParseResult::Passthrough(passthrough)
}

/// Compiled tsc error line pattern: `file(line,col): error TSxxxx: message`
///
/// Shared between tier 1 and tier 2 parsers. Compiled once via `LazyLock`.
static TSC_ERROR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+)\((\d+),(\d+)\): error (TS\d+): (.+)$").expect("valid regex")
});

/// Tier 1: Parse tsc errors from stderr using regex.
///
/// tsc writes errors in the format:
/// `src/index.ts(10,5): error TS2304: Cannot find name 'foo'.`
///
/// Returns `Full` if any lines match the tsc error pattern.
/// Empty stderr is NOT handled here -- it is checked after tier 2 in `parse_tsc`.
fn try_tier1_regex(stderr: &str) -> Option<ParseResult<BuildResult>> {
    if stderr.trim().is_empty() {
        return None;
    }

    let mut error_count: usize = 0;
    let mut error_messages: Vec<String> = Vec::new();
    let mut any_match = false;

    for line in stderr.lines() {
        if let Some(caps) = TSC_ERROR_RE.captures(line) {
            any_match = true;
            error_count += 1;

            let file = caps.get(1).map_or("", |m| m.as_str());
            let line_num = caps.get(2).map_or("", |m| m.as_str());
            let ts_code = caps.get(4).map_or("", |m| m.as_str());
            let message = caps.get(5).map_or("", |m| m.as_str());

            error_messages.push(format!("{ts_code}: {message} ({file}:{line_num})"));
        }
    }

    if !any_match {
        return None;
    }

    let result = BuildResult::new(false, 0, error_count, None, error_messages);
    Some(ParseResult::Full(result))
}

/// Tier 2: Same regex on combined stdout+stderr.
///
/// Fallback in case tsc output goes to an unexpected stream.
fn try_tier2_combined(combined: &str) -> Option<ParseResult<BuildResult>> {
    let mut error_count: usize = 0;
    let mut error_messages: Vec<String> = Vec::new();

    for line in combined.lines() {
        if let Some(caps) = TSC_ERROR_RE.captures(line) {
            error_count += 1;

            let file = caps.get(1).map_or("", |m| m.as_str());
            let line_num = caps.get(2).map_or("", |m| m.as_str());
            let ts_code = caps.get(4).map_or("", |m| m.as_str());
            let message = caps.get(5).map_or("", |m| m.as_str());

            error_messages.push(format!("{ts_code}: {message} ({file}:{line_num})"));
        }
    }

    if error_count == 0 {
        return None;
    }

    let result = BuildResult::new(false, 0, error_count, None, error_messages);
    Some(ParseResult::Degraded(
        result,
        vec!["tsc: structured parse failed, using combined stdout+stderr".to_string()],
    ))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cmd/build")
    }

    fn load_fixture(name: &str) -> String {
        std::fs::read_to_string(fixtures_dir().join(name))
            .unwrap_or_else(|e| panic!("Failed to load fixture {name}: {e}"))
    }

    fn make_output(stdout: &str, stderr: &str, exit_code: Option<i32>) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code,
            duration: Duration::from_millis(100),
        }
    }

    // ========================================================================
    // Tier 1: Regex on stderr
    // ========================================================================

    #[test]
    fn test_tier1_tsc_errors() {
        let stderr = load_fixture("tsc_errors.txt");
        let output = make_output("", &stderr, Some(2));
        let result = parse_tsc(&output);

        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(build_result) = &result {
            assert_eq!(build_result.errors, 3, "expected 3 errors");
            assert!(!build_result.success, "expected failure");
        }
    }

    #[test]
    fn test_tier1_tsc_success() {
        // Empty stdout+stderr means successful compilation
        let output = make_output("", "", Some(0));
        let result = parse_tsc(&output);

        assert!(
            result.is_full(),
            "expected Full for empty output, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(build_result) = &result {
            assert!(build_result.success, "expected success");
            assert_eq!(build_result.errors, 0, "expected 0 errors");
        }
    }

    #[test]
    fn test_tsc_group_by_file() {
        let stderr = load_fixture("tsc_errors.txt");
        let output = make_output("", &stderr, Some(2));
        let result = parse_tsc(&output);

        if let ParseResult::Full(build_result) = &result {
            // Verify errors reference different files
            let src_index_errors: Vec<_> = build_result
                .error_messages
                .iter()
                .filter(|m| m.contains("src/index.ts"))
                .collect();
            let src_utils_errors: Vec<_> = build_result
                .error_messages
                .iter()
                .filter(|m| m.contains("src/utils.ts"))
                .collect();

            assert_eq!(
                src_index_errors.len(),
                2,
                "expected 2 errors from src/index.ts"
            );
            assert_eq!(
                src_utils_errors.len(),
                1,
                "expected 1 error from src/utils.ts"
            );
        } else {
            panic!("expected Full result");
        }
    }

    // ========================================================================
    // Tier 2: Combined fallback
    // ========================================================================

    #[test]
    fn test_tier2_tsc_errors_on_stdout() {
        // tsc output on stdout instead of stderr (unusual but possible)
        let tsc_output = "src/main.ts(5,1): error TS2304: Cannot find name 'x'.\n";
        let output = make_output(tsc_output, "", Some(2));
        let result = parse_tsc(&output);

        assert!(
            result.is_degraded(),
            "expected Degraded for stdout-only tsc errors, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Degraded(build_result, markers) = &result {
            assert_eq!(build_result.errors, 1, "expected 1 error");
            assert!(markers.iter().any(|m| m.contains("combined")));
        }
    }

    // ========================================================================
    // Tier 3: Passthrough
    // ========================================================================

    #[test]
    fn test_tier3_passthrough() {
        // Non-tsc output that doesn't match any pattern
        let output = make_output("", "some random error text\n", Some(1));
        let result = parse_tsc(&output);

        assert!(
            result.is_passthrough(),
            "expected Passthrough, got {:?}",
            result.tier_name()
        );
    }
}
