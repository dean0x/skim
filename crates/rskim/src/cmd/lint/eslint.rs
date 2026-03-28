//! ESLint parser with three-tier degradation (#104).
//!
//! Executes `eslint` and parses the output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON array parsing (`--format json`)
//! - **Tier 2 (Degraded)**: Regex on default formatter output
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

const PROGRAM: &str = "eslint";
const ENV_OVERRIDES: &[(&str, &str)] = &[("NO_COLOR", "1")];
const INSTALL_HINT: &str = "Install eslint via npm: npm install -g eslint";

// Static regex patterns compiled once via LazyLock.
static RE_ESLINT_LINE: LazyLock<Regex> = LazyLock::new(|| {
    // Matches: "  12:7  warning  'x' is defined but never used  no-unused-vars"
    Regex::new(r"^\s+(\d+):\d+\s+(error|warning)\s+(.+?)\s{2,}(\S+)\s*$").unwrap()
});

static RE_ESLINT_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(/[^\s]+|[A-Z]:\\[^\s]+)$").unwrap());

/// Run `skim lint eslint [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<ExitCode> {
    let mut cmd_args: Vec<String> = args.to_vec();

    // Inject --format json if not already present
    if !user_has_flag(&cmd_args, &["--format", "-f"]) {
        cmd_args.insert(0, "json".to_string());
        cmd_args.insert(0, "--format".to_string());
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

/// Three-tier parse function for eslint output.
fn parse_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    if let Some(result) = try_parse_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(result, vec!["regex fallback".to_string()]);
    }

    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// Tier 1: JSON parsing
// ============================================================================

/// Parse eslint JSON output format.
///
/// ESLint `--format json` produces an array of file results:
/// ```json
/// [{"filePath": "...", "messages": [{"ruleId": "...", "severity": 1, "message": "...", "line": 12}]}]
/// ```
fn try_parse_json(stdout: &str) -> Option<LintResult> {
    let arr: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).ok()?;

    let mut issues: Vec<LintIssue> = Vec::new();

    for file_entry in &arr {
        let Some(file_path) = file_entry.get("filePath").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(messages) = file_entry.get("messages").and_then(|v| v.as_array()) else {
            continue;
        };

        for msg in messages {
            let Some(severity_num) = msg.get("severity").and_then(|v| v.as_u64()) else {
                continue;
            };
            let severity = match severity_num {
                2 => LintSeverity::Error,
                1 => LintSeverity::Warning,
                _ => LintSeverity::Info,
            };
            let rule_id = msg
                .get("ruleId")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            let Some(message) = msg.get("message").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(line) = msg.get("line").and_then(|v| v.as_u64()) else {
                continue;
            };

            issues.push(LintIssue {
                file: file_path.to_string(),
                line: u32::try_from(line).unwrap_or(u32::MAX),
                rule: rule_id.to_string(),
                message: message.to_string(),
                severity,
            });
        }
    }

    Some(group_issues("eslint", issues))
}

// ============================================================================
// Tier 2: regex fallback
// ============================================================================

/// Parse eslint default formatter output via regex.
///
/// Format:
/// ```text
/// /path/to/file.ts
///   12:7  warning  'x' is defined but never used  no-unused-vars
/// ```
fn try_parse_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();
    let mut current_file = String::new();

    for line in text.lines() {
        // Try to match a file path line
        if RE_ESLINT_FILE.is_match(line.trim()) {
            current_file = line.trim().to_string();
            continue;
        }

        // Try to match an issue line
        if let Some(caps) = RE_ESLINT_LINE.captures(line) {
            let line_num: u32 = caps[1].parse().unwrap_or(0);
            let severity = match &caps[2] {
                "error" => LintSeverity::Error,
                "warning" => LintSeverity::Warning,
                _ => LintSeverity::Info,
            };
            let message = caps[3].to_string();
            let rule = caps[4].to_string();

            issues.push(LintIssue {
                file: current_file.clone(),
                line: line_num,
                rule,
                message,
                severity,
            });
        }
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("eslint", issues))
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
    fn test_tier1_eslint_pass() {
        let input = load_fixture("eslint_pass.json");
        let result = try_parse_json(&input);
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.errors, 0);
        assert_eq!(result.warnings, 0);
        assert!(result.as_ref().contains("LINT OK"));
    }

    #[test]
    fn test_tier1_eslint_fail() {
        let input = load_fixture("eslint_fail.json");
        let result = try_parse_json(&input);
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.errors, 2);
        assert_eq!(result.warnings, 3);
        assert!(result.groups.len() >= 2, "Expected at least 2 rule groups");
    }

    #[test]
    fn test_tier2_eslint_regex() {
        let input = load_fixture("eslint_text.txt");
        let result = try_parse_regex(&input);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.errors, 2);
        assert_eq!(result.warnings, 2);
    }

    #[test]
    fn test_parse_impl_json_produces_full() {
        let input = load_fixture("eslint_fail.json");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full parse result, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_text_produces_degraded() {
        let input = load_fixture("eslint_text.txt");
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
            stdout: "completely unparseable output\nno json, no regex match".to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }
}
