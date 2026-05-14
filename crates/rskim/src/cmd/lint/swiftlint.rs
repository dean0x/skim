//! SwiftLint parser with three-tier degradation (#118).
//!
//! Executes `swiftlint` and parses the output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON array parsing (`--reporter json`)
//! - **Tier 2 (Degraded)**: Regex on default formatter output
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::ParseResult;
use crate::output::canonical::{LintIssue, LintResult, LintSeverity};
use crate::runner::CommandOutput;

use super::{LinterConfig, combine_stdout_stderr, group_issues};

const CONFIG: LinterConfig<'static> = LinterConfig {
    program: "swiftlint",
    env_overrides: &[],
    install_hint: "Install SwiftLint: brew install swiftlint",
};

/// `file.swift:line:col: warning: message (rule_id)`
static RE_SWIFTLINT_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+):(\d+):\d+: (warning|error): (.+?) \((.+)\)$").expect("valid regex")
});

/// Run `skim swiftlint [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    super::run_linter(CONFIG, args, ctx, prepare_args, parse_impl)
}

/// Inject `--reporter json` unless the user already specified a reporter or fix flags.
fn prepare_args(cmd_args: &mut Vec<String>) {
    if !user_has_flag(cmd_args, &["--reporter", "--fix", "--autocorrect"]) {
        cmd_args.insert(0, "json".to_string());
        cmd_args.insert(0, "--reporter".to_string());
    }
}

/// Three-tier parse function for swiftlint output.
fn parse_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    if let Some(result) = try_parse_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["swiftlint: JSON parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// Tier 1: JSON parsing
// ============================================================================

/// Parse SwiftLint JSON output format.
///
/// SwiftLint `--reporter json` produces an array:
/// ```json
/// [{"file": "...", "line": N, "rule_id": "...", "type": "warning", "reason": "..."}]
/// ```
fn try_parse_json(stdout: &str) -> Option<LintResult> {
    let arr: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).ok()?;
    let mut issues: Vec<LintIssue> = Vec::new();

    for entry in &arr {
        let file = entry.get("file").and_then(|v| v.as_str()).unwrap_or("");
        let line = entry
            .get("line")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let rule_id = entry
            .get("rule_id")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let type_str = entry
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("warning");
        let reason = entry
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let severity = match type_str {
            "error" => LintSeverity::Error,
            "warning" => LintSeverity::Warning,
            _ => LintSeverity::Info,
        };

        issues.push(LintIssue {
            file: file.to_string(),
            line: u32::try_from(line).unwrap_or(u32::MAX),
            rule: rule_id.to_string(),
            message: reason.to_string(),
            severity,
        });
    }

    Some(group_issues("swiftlint", issues))
}

// ============================================================================
// Tier 2: regex fallback
// ============================================================================

/// Parse SwiftLint default formatter output via regex.
fn try_parse_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();

    for line in text.lines() {
        if let Some(caps) = RE_SWIFTLINT_LINE.captures(line) {
            let file = caps[1].to_string();
            let line_num: u32 = caps[2].parse().unwrap_or(0);
            let type_str = &caps[3];
            let message = caps[4].to_string();
            let rule_id = caps[5].to_string();

            let severity = match type_str {
                "error" => LintSeverity::Error,
                "warning" => LintSeverity::Warning,
                _ => LintSeverity::Info,
            };

            issues.push(LintIssue {
                file,
                line: line_num,
                rule: rule_id,
                message,
                severity,
            });
        }
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("swiftlint", issues))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::CommandOutput;
    use std::time::Duration;

    fn make_output(stdout: &str, stderr: &str, exit_code: Option<i32>) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code,
            duration: Duration::ZERO,
        }
    }

    const SWIFTLINT_PASS_JSON: &str = r#"[]"#;

    const SWIFTLINT_FAIL_JSON: &str = r#"[{"character":null,"file":"/Users/dev/MyApp/Sources/ContentView.swift","line":10,"reason":"Line should be 120 characters or less; currently it is 125 characters","rule_id":"line_length","severity":"Warning","type":"warning"},{"character":5,"file":"/Users/dev/MyApp/Sources/ContentView.swift","line":20,"reason":"Function body should span 40 lines or less; currently spans 50 lines","rule_id":"function_body_length","severity":"Error","type":"error"}]"#;

    const SWIFTLINT_TEXT: &str = "/Users/dev/MyApp/Sources/ContentView.swift:10:1: warning: Line should be 120 characters or less; currently it is 125 characters (line_length)\n/Users/dev/MyApp/Sources/ContentView.swift:20:5: error: Function body should span 40 lines or less; currently spans 50 lines (function_body_length)\n";

    #[test]
    fn test_swiftlint_tier1_pass() {
        let result = try_parse_json(SWIFTLINT_PASS_JSON);
        assert!(result.is_some(), "Expected JSON parse to succeed on empty array");
        let r = result.unwrap();
        assert_eq!(r.errors, 0);
        assert_eq!(r.warnings, 0);
        assert!(r.as_ref().contains(" OK"));
    }

    #[test]
    fn test_swiftlint_tier1_fail() {
        let result = try_parse_json(SWIFTLINT_FAIL_JSON);
        assert!(result.is_some(), "Expected JSON parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.errors, 1);
        assert_eq!(r.warnings, 1);
        assert_eq!(r.groups.len(), 2);
    }

    #[test]
    fn test_swiftlint_tier2_regex() {
        let result = try_parse_regex(SWIFTLINT_TEXT);
        assert!(result.is_some(), "Expected regex parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.errors, 1);
        assert_eq!(r.warnings, 1);
    }

    #[test]
    fn test_swiftlint_tier3_passthrough() {
        let output = make_output("completely unparseable output", "", Some(1));
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_json_produces_full() {
        let output = make_output(SWIFTLINT_FAIL_JSON, "", Some(1));
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_text_produces_degraded() {
        let output = make_output(SWIFTLINT_TEXT, "", Some(1));
        let result = parse_impl(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_flag_injection_skipped_when_reporter_present() {
        let args = vec!["--reporter".to_string(), "emoji".to_string()];
        assert!(user_has_flag(&args, &["--reporter", "--fix", "--autocorrect"]));
    }
}
