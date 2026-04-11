//! golangci-lint parser with three-tier degradation (#104).
//!
//! Executes `golangci-lint run` and parses the output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON object parsing (`--out-format json`)
//! - **Tier 2 (Degraded)**: Regex on default text output
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{LintIssue, LintResult, LintSeverity};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, group_issues, LinterConfig};

const CONFIG: LinterConfig<'static> = LinterConfig {
    program: "golangci-lint",
    env_overrides: &[("NO_COLOR", "1")],
    install_hint: "Install golangci-lint: https://golangci-lint.run/welcome/install/",
};

// Static regex pattern compiled once via LazyLock.
static RE_GOLANGCI_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+):(\d+)(?::\d+)?:\s+(.+)\s+\((\S+)\)$").unwrap());

/// Run `skim lint golangci [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<std::process::ExitCode> {
    super::run_linter(
        CONFIG,
        args,
        show_stats,
        json_output,
        prepare_args,
        parse_impl,
    )
}

/// Ensure "run" subcommand is present and inject `--out-format json`.
fn prepare_args(cmd_args: &mut Vec<String>) {
    // Ensure "run" subcommand is present if args don't start with it
    if cmd_args.first().is_none_or(|a| a != "run") {
        cmd_args.insert(0, "run".to_string());
    }

    // Inject --out-format json if not already present
    if !user_has_flag(cmd_args, &["--out-format"]) {
        cmd_args.push("--out-format".to_string());
        cmd_args.push("json".to_string());
    }
}

/// Three-tier parse function for golangci-lint output.
fn parse_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    if let Some(result) = try_parse_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["golangci-lint: JSON parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
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
            // golangci-lint defaults to warning when Severity is empty/absent/unknown
            _ => LintSeverity::Warning,
        };

        issues.push(LintIssue {
            file: filename.to_string(),
            line: u32::try_from(line).unwrap_or(u32::MAX),
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
///
/// **Severity heuristic:** The text format does not include an explicit severity
/// field. We apply a best-effort heuristic: if the message text contains the word
/// `"error"` (case-insensitive) or the linter name is known to produce only
/// compile-level errors (e.g., `typecheck`, `staticcheck`), the issue is classified
/// as `Error`; otherwise `Warning`. Accurate severity requires Tier 1 JSON parsing.
fn try_parse_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();

    for line in text.lines() {
        if let Some(caps) = RE_GOLANGCI_LINE.captures(line) {
            let file = caps[1].to_string();
            let line_num: u32 = caps[2].parse().unwrap_or(0);
            let message = caps[3].to_string();
            let linter = caps[4].to_string();

            let severity = infer_severity_from_text(&message, &linter);

            issues.push(LintIssue {
                file,
                line: line_num,
                rule: linter,
                message,
                severity,
            });
        }
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("golangci", issues))
}

/// Infer `LintSeverity` from message text and linter name when no explicit
/// severity field is available (Tier-2 text format).
///
/// Classifies as `Error` when:
/// - The message contains "error" (case-insensitive), OR
/// - The linter is known to produce only compile/type errors (`typecheck`,
///   `staticcheck` when flagged as error, `govet`).
///
/// All other issues default to `Warning`.
fn infer_severity_from_text(message: &str, linter: &str) -> LintSeverity {
    if message.to_lowercase().contains("error") {
        return LintSeverity::Error;
    }
    // Known error-only linters in golangci-lint
    if matches!(linter, "typecheck") {
        return LintSeverity::Error;
    }
    LintSeverity::Warning
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
        // Fixture has 4 issues total; line 3 has "Error return value" which the
        // severity heuristic promotes to Error. The remaining 3 are Warnings.
        assert_eq!(
            result.errors + result.warnings,
            4,
            "Must have 4 total issues (errors+warnings)"
        );
        // At least 1 issue must be classified as Error due to "error" keyword heuristic.
        assert!(
            result.errors >= 1,
            "At least 1 issue must be Error (errcheck 'Error return value')"
        );
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
    fn test_parse_impl_text_produces_degraded() {
        let input = load_fixture("golangci_text.txt");
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

    /// Tier-2 severity heuristic: "typecheck" linter must produce Error.
    #[test]
    fn test_tier2_severity_typecheck_is_error() {
        let input = "main.go:5:10: undefined: Foo (typecheck)\n";
        let result = try_parse_regex(input).expect("must parse");
        assert_eq!(result.errors, 1, "typecheck issues must be Error severity");
        assert_eq!(result.warnings, 0);
    }

    /// Tier-2 severity heuristic: message containing "error" is classified as Error.
    #[test]
    fn test_tier2_severity_error_keyword_in_message() {
        let input = "pkg/api.go:12: cannot use x as type error (govet)\n";
        let result = try_parse_regex(input).expect("must parse");
        assert_eq!(
            result.errors, 1,
            "Message with 'error' keyword must be Error severity"
        );
    }

    /// Tier-2 severity heuristic: normal warning message stays as Warning.
    #[test]
    fn test_tier2_severity_normal_message_is_warning() {
        let input = "pkg/api.go:12: unused variable x (deadcode)\n";
        let result = try_parse_regex(input).expect("must parse");
        assert_eq!(result.warnings, 1, "Normal messages must remain Warning");
        assert_eq!(result.errors, 0);
    }
}
