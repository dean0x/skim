//! golangci-lint parser with three-tier degradation (#104).
//!
//! Executes `golangci-lint run` and parses the output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON object parsing (`--out-format json`)
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

use super::{group_issues, LintJsonConfig};

// Static regex pattern compiled once via LazyLock.
static RE_GOLANGCI_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+):(\d+)(?::\d+)?:\s+(.+)\s+\((\S+)\)$").unwrap());

/// Run `skim lint golangci [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<ExitCode> {
    let mut cmd_args: Vec<String> = Vec::new();

    // Ensure "run" subcommand is present if args don't start with it
    let needs_run = args.first().is_none_or(|a| a != "run");
    if needs_run {
        cmd_args.push("run".to_string());
    }

    cmd_args.extend(args.iter().cloned());

    // Inject --out-format json if not already present
    if !user_has_flag(&cmd_args, &["--out-format"]) {
        cmd_args.push("--out-format".to_string());
        cmd_args.push("json".to_string());
    }

    let use_stdin = !std::io::stdin().is_terminal() && args.is_empty();

    if json_output {
        return run_json_mode(&cmd_args, use_stdin, show_stats);
    }

    run_parsed_command_with_mode(
        ParsedCommandConfig {
            program: "golangci-lint",
            args: &cmd_args,
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install golangci-lint: https://golangci-lint.run/welcome/install/",
            use_stdin,
            show_stats,
            command_type: crate::analytics::CommandType::Lint,
        },
        |output, _args| parse_impl(output),
    )
}

/// Run in `--json` mode: delegate to shared lint JSON helper.
fn run_json_mode(
    cmd_args: &[String],
    use_stdin: bool,
    show_stats: bool,
) -> anyhow::Result<ExitCode> {
    super::run_lint_json_mode(
        LintJsonConfig {
            program: "golangci-lint",
            cmd_args,
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install golangci-lint: https://golangci-lint.run/welcome/install/",
            use_stdin,
            show_stats,
        },
        parse_impl,
    )
}

/// Three-tier parse function for golangci-lint output.
fn parse_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    // Tier 1: JSON parsing
    if let Some(result) = try_parse_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Tier 2: regex fallback
    let combined = if output.stderr.is_empty() {
        output.stdout.clone()
    } else {
        format!("{}\n{}", output.stdout, output.stderr)
    };

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(result, vec!["regex fallback".to_string()]);
    }

    // Tier 3: passthrough
    ParseResult::Passthrough(combined)
}

// ============================================================================
// Tier 1: JSON parsing
// ============================================================================

/// Parse golangci-lint JSON output format.
///
/// golangci-lint `--out-format json` produces:
/// ```json
/// {"Issues": [{"FromLinter": "govet", "Text": "...", "Pos": {"Filename": "...", "Line": 42}}]}
/// ```
fn try_parse_json(stdout: &str) -> Option<LintResult> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let obj = value.as_object()?;

    // Must have "Issues" key (golangci-lint JSON always has this)
    let issues_val = obj.get("Issues")?;

    // Issues can be null (no issues) or an array
    let issues_arr = match issues_val {
        serde_json::Value::Null => &[] as &[serde_json::Value],
        serde_json::Value::Array(arr) => arr.as_slice(),
        _ => return None,
    };

    let mut issues: Vec<LintIssue> = Vec::new();

    for entry in issues_arr {
        let linter = entry
            .get("FromLinter")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let text = entry.get("Text").and_then(|v| v.as_str()).unwrap_or("");
        let Some(pos) = entry.get("Pos") else {
            continue;
        };
        let Some(filename) = pos.get("Filename").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(line) = pos.get("Line").and_then(|v| v.as_u64()) else {
            continue;
        };

        let severity_str = entry.get("Severity").and_then(|v| v.as_str()).unwrap_or("");
        let severity = match severity_str {
            "error" => LintSeverity::Error,
            "warning" => LintSeverity::Warning,
            _ => LintSeverity::Warning, // Default to warning when empty or unknown
        };

        issues.push(LintIssue {
            file: filename.to_string(),
            line: line as u32,
            rule: linter.to_string(),
            message: text.to_string(),
            severity,
        });
    }

    Some(group_issues("golangci", issues))
}

// ============================================================================
// Tier 2: regex fallback
// ============================================================================

/// Parse golangci-lint default text output via regex.
///
/// Format: `file:line:col: message (linter)`
fn try_parse_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();

    for line in text.lines() {
        if let Some(caps) = RE_GOLANGCI_LINE.captures(line) {
            let file = caps[1].to_string();
            let line_num: u32 = caps[2].parse().unwrap_or(0);
            let message = caps[3].to_string();
            let linter = caps[4].to_string();

            issues.push(LintIssue {
                file,
                line: line_num,
                rule: linter,
                message,
                severity: LintSeverity::Warning,
            });
        }
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("golangci", issues))
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
    fn test_tier1_golangci_fail() {
        let input = load_fixture("golangci_fail.json");
        let result = try_parse_json(&input);
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        // 4 issues total: 1 error (staticcheck), 3 warnings (govet x2 + errcheck)
        assert_eq!(result.errors, 1);
        assert_eq!(result.warnings, 3);
    }

    #[test]
    fn test_tier1_golangci_null_issues() {
        let input = r#"{"Issues": null}"#;
        let result = try_parse_json(input);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.errors, 0);
        assert_eq!(result.warnings, 0);
        assert!(result.as_ref().contains("LINT OK"));
    }

    #[test]
    fn test_tier2_golangci_regex() {
        let input = load_fixture("golangci_text.txt");
        let result = try_parse_regex(&input);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.warnings, 4);
    }

    #[test]
    fn test_parse_impl_json_produces_full() {
        let input = load_fixture("golangci_fail.json");
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
