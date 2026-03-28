//! mypy parser with three-tier degradation (#104).
//!
//! Executes `mypy` and parses the output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: NDJSON parsing (`--output json`)
//! - **Tier 2 (Degraded)**: Regex on default text output
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation

use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::{run_parsed_command_with_mode, user_has_flag, ParsedCommandConfig};
use crate::output::canonical::{LintIssue, LintResult, LintSeverity};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, group_issues, LintJsonConfig};

const PROGRAM: &str = "mypy";
const ENV_OVERRIDES: &[(&str, &str)] = &[("NO_COLOR", "1"), ("MYPY_FORCE_COLOR", "0")];
const INSTALL_HINT: &str = "Install mypy: pip install mypy";

// Static regex pattern compiled once via LazyLock.
static RE_MYPY_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+):(\d+):\s+(error|warning|note):\s+(.+?)(?:\s+\[(\S+)\])?$").unwrap()
});

/// Run `skim lint mypy [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<ExitCode> {
    let mut cmd_args: Vec<String> = args.to_vec();

    // Inject --output json if not already present
    if !user_has_flag(&cmd_args, &["--output"]) {
        cmd_args.insert(0, "json".to_string());
        cmd_args.insert(0, "--output".to_string());
    }

    let use_stdin = !std::io::stdin().is_terminal() && args.is_empty();

    if json_output {
        return super::run_lint_json_mode(
            LintJsonConfig {
                program: PROGRAM,
                cmd_args: &cmd_args,
                env_overrides: ENV_OVERRIDES,
                install_hint: INSTALL_HINT,
                use_stdin,
                show_stats,
            },
            parse_impl,
        );
    }

    run_parsed_command_with_mode(
        ParsedCommandConfig {
            program: PROGRAM,
            args: &cmd_args,
            env_overrides: ENV_OVERRIDES,
            install_hint: INSTALL_HINT,
            use_stdin,
            show_stats,
            command_type: crate::analytics::CommandType::Lint,
        },
        |output, _args| parse_impl(output),
    )
}

/// Three-tier parse function for mypy output.
fn parse_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    if let Some(result) = try_parse_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(result, vec!["regex fallback".to_string()]);
    }

    ParseResult::Passthrough(combined)
}

// ============================================================================
// Tier 1: NDJSON parsing
// ============================================================================

/// Parse mypy NDJSON output format.
///
/// mypy `--output json` produces one JSON object per line (NDJSON):
/// ```json
/// {"file": "...", "line": 10, "column": 5, "message": "...", "code": "...", "severity": "error"}
/// ```
fn try_parse_json(stdout: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();
    let mut any_parsed = false;

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };

        // Must have at least "file" and "line" fields to be a valid mypy JSON entry
        let Some(file) = value.get("file").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(line_num) = value.get("line").and_then(|v| v.as_u64()) else {
            continue;
        };

        any_parsed = true;

        let severity_str = value
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        let severity = match severity_str {
            "error" => LintSeverity::Error,
            "warning" => LintSeverity::Warning,
            "note" => LintSeverity::Info,
            _ => LintSeverity::Error,
        };

        let code = value
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let message = value.get("message").and_then(|v| v.as_str()).unwrap_or("");

        issues.push(LintIssue {
            file: file.to_string(),
            line: u32::try_from(line_num).unwrap_or(u32::MAX),
            rule: code.to_string(),
            message: message.to_string(),
            severity,
        });
    }

    if !any_parsed {
        return None;
    }

    Some(group_issues("mypy", issues))
}

// ============================================================================
// Tier 2: regex fallback
// ============================================================================

/// Parse mypy default text output via regex.
///
/// Format: `file:line: error: message [code]`
fn try_parse_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();

    for line in text.lines() {
        if let Some(caps) = RE_MYPY_LINE.captures(line) {
            let file = caps[1].to_string();
            let line_num: u32 = caps[2].parse().unwrap_or(0);
            let severity = match &caps[3] {
                "error" => LintSeverity::Error,
                "warning" => LintSeverity::Warning,
                "note" => LintSeverity::Info,
                _ => LintSeverity::Error,
            };
            let message = caps[4].to_string();
            let code = caps
                .get(5)
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| "(unknown)".to_string());

            issues.push(LintIssue {
                file,
                line: line_num,
                rule: code,
                message,
                severity,
            });
        }
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("mypy", issues))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn load_fixture(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/cmd/lint");
        path.push(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    #[test]
    fn test_tier1_mypy_fail() {
        let input = load_fixture("mypy_fail.json");
        let result = try_parse_json(&input);
        assert!(result.is_some(), "Expected Tier 1 NDJSON parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.errors, 3);
        assert_eq!(result.warnings, 0);
    }

    #[test]
    fn test_tier2_mypy_regex() {
        let input = load_fixture("mypy_text.txt");
        let result = try_parse_regex(&input);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.errors, 3);
    }

    #[test]
    fn test_parse_impl_json_produces_full() {
        let input = load_fixture("mypy_fail.json");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(result.is_full());
    }

    #[test]
    fn test_parse_impl_text_produces_degraded() {
        let input = load_fixture("mypy_text.txt");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded parse result, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_garbage_produces_passthrough() {
        let output = CommandOutput {
            stdout: "random garbage".to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(result.is_passthrough());
    }
}
