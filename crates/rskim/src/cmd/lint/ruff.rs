//! Ruff linter parser with three-tier degradation (#104).
//!
//! Executes `ruff check` and parses the output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON array parsing (`--output-format json`)
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

const PROGRAM: &str = "ruff";
const ENV_OVERRIDES: &[(&str, &str)] = &[("NO_COLOR", "1")];
const INSTALL_HINT: &str = "Install ruff: pip install ruff";

// Static regex pattern compiled once via LazyLock.
static RE_RUFF_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+):(\d+):\d+:\s+(\S+)\s+(.+)").unwrap());

/// Run `skim lint ruff [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<ExitCode> {
    let mut cmd_args: Vec<String> = Vec::new();

    // Ensure "check" subcommand is present if args don't start with it
    let needs_check = args.first().is_none_or(|a| a != "check");
    if needs_check {
        cmd_args.push("check".to_string());
    }

    cmd_args.extend(args.iter().cloned());

    // Inject --output-format json if not already present
    if !user_has_flag(&cmd_args, &["--output-format"]) {
        cmd_args.push("--output-format".to_string());
        cmd_args.push("json".to_string());
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

/// Three-tier parse function for ruff output.
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
// Tier 1: JSON parsing
// ============================================================================

/// Parse ruff JSON output format.
///
/// Ruff `--output-format json` produces an array:
/// ```json
/// [{"code": "F401", "message": "...", "filename": "...", "location": {"row": 1}}]
/// ```
fn try_parse_json(stdout: &str) -> Option<LintResult> {
    let arr: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).ok()?;

    // An empty array is valid — means clean run
    let mut issues: Vec<LintIssue> = Vec::new();

    for entry in &arr {
        let Some(code) = entry.get("code").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(message) = entry.get("message").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(filename) = entry.get("filename").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(location) = entry.get("location") else {
            continue;
        };
        let Some(row) = location.get("row").and_then(|v| v.as_u64()) else {
            continue;
        };

        issues.push(LintIssue {
            file: filename.to_string(),
            line: u32::try_from(row).unwrap_or(u32::MAX),
            rule: code.to_string(),
            message: message.to_string(),
            // Ruff issues are all errors by default (no severity field)
            severity: LintSeverity::Error,
        });
    }

    Some(group_issues("ruff", issues))
}

// ============================================================================
// Tier 2: regex fallback
// ============================================================================

/// Parse ruff default text output via regex.
///
/// Format: `file:line:col: CODE message`
fn try_parse_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();

    for line in text.lines() {
        if let Some(caps) = RE_RUFF_LINE.captures(line) {
            let file = caps[1].to_string();
            let line_num: u32 = caps[2].parse().unwrap_or(0);
            let code = caps[3].to_string();
            let message = caps[4].to_string();

            issues.push(LintIssue {
                file,
                line: line_num,
                rule: code,
                message,
                severity: LintSeverity::Error,
            });
        }
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("ruff", issues))
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
    fn test_tier1_ruff_fail() {
        let input = load_fixture("ruff_fail.json");
        let result = try_parse_json(&input);
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.errors, 3);
        assert_eq!(result.warnings, 0);
    }

    #[test]
    fn test_tier1_ruff_clean() {
        let result = try_parse_json("[]");
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.errors, 0);
        assert_eq!(result.warnings, 0);
        assert!(result.as_ref().contains("LINT OK"));
    }

    #[test]
    fn test_tier2_ruff_regex() {
        let input = load_fixture("ruff_text.txt");
        let result = try_parse_regex(&input);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.errors, 3);
    }

    #[test]
    fn test_parse_impl_json_produces_full() {
        let input = load_fixture("ruff_fail.json");
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
        let input = load_fixture("ruff_text.txt");
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
