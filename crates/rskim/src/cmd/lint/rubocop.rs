//! RuboCop parser with three-tier degradation (#118).
//!
//! Executes `rubocop` and parses the output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON array parsing (`--format json`)
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
    program: "rubocop",
    env_overrides: &[("NO_COLOR", "1")],
    install_hint: "Install RuboCop: gem install rubocop",
};

/// `file:line:col: S: CopName: message`
/// Letter codes: C=convention, W=warning, E=error, F=fatal
static RE_RUBOCOP_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+):(\d+):\d+: ([CWEF]): (.+?): (.+)$").expect("valid regex")
});

/// Run `skim rubocop [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    super::run_linter(CONFIG, args, ctx, prepare_args, parse_impl)
}

/// Inject `--format json` unless the user already specified a format or
/// is using auto-correct flags (which change the output structure).
fn prepare_args(cmd_args: &mut Vec<String>) {
    if !user_has_flag(
        cmd_args,
        &["--format", "-f", "-a", "-A", "--autocorrect", "--autocorrect-all"],
    ) {
        cmd_args.insert(0, "json".to_string());
        cmd_args.insert(0, "--format".to_string());
    }
}

/// Three-tier parse function for rubocop output.
fn parse_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    if let Some(result) = try_parse_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["rubocop: JSON parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// Tier 1: JSON parsing
// ============================================================================

/// Parse RuboCop JSON output format.
///
/// RuboCop `--format json` produces:
/// ```json
/// {"files": [{"path": "...", "offenses": [{"cop_name": "...", "severity": "convention", "message": "...", "location": {"start_line": N}}]}]}
/// ```
fn try_parse_json(stdout: &str) -> Option<LintResult> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let files = v.get("files")?.as_array()?;

    let mut issues: Vec<LintIssue> = Vec::new();

    for file_entry in files {
        let file_path = file_entry.get("path")?.as_str()?;
        let offenses = file_entry.get("offenses")?.as_array()?;

        for offense in offenses {
            let cop_name = offense
                .get("cop_name")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            let severity_str = offense
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("convention");
            let message = offense
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let start_line = offense
                .get("location")
                .and_then(|l| l.get("start_line"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let severity = severity_to_enum(severity_str);

            issues.push(LintIssue {
                file: file_path.to_string(),
                line: u32::try_from(start_line).unwrap_or(u32::MAX),
                rule: cop_name.to_string(),
                message: message.to_string(),
                severity,
            });
        }
    }

    Some(group_issues("rubocop", issues))
}

fn severity_to_enum(s: &str) -> LintSeverity {
    match s {
        "error" | "fatal" => LintSeverity::Error,
        "warning" => LintSeverity::Warning,
        _ => LintSeverity::Info, // convention, refactor, etc.
    }
}

// ============================================================================
// Tier 2: regex fallback
// ============================================================================

/// Parse RuboCop default formatter output via regex.
///
/// Format: `file.rb:10:5: C: Style/StringLiterals: Prefer single-quoted strings`
fn try_parse_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();

    for line in text.lines() {
        if let Some(caps) = RE_RUBOCOP_LINE.captures(line) {
            let file = caps[1].to_string();
            let line_num: u32 = caps[2].parse().unwrap_or(0);
            let letter = &caps[3];
            let cop_name = caps[4].to_string();
            let message = caps[5].to_string();

            let severity = match letter {
                "C" | "W" => LintSeverity::Warning,
                "E" | "F" => LintSeverity::Error,
                _ => LintSeverity::Info,
            };

            issues.push(LintIssue {
                file,
                line: line_num,
                rule: cop_name,
                message,
                severity,
            });
        }
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("rubocop", issues))
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

    const RUBOCOP_PASS_JSON: &str = r#"{"metadata":{"rubocop_version":"1.65.0","ruby_engine":"ruby","ruby_version":"3.3.0","ruby_patchlevel":"0","ruby_platform":"arm64-darwin23"},"files":[{"path":"app/models/user.rb","offenses":[]}],"summary":{"offense_count":0,"target_file_count":1,"inspected_file_count":1}}"#;

    const RUBOCOP_FAIL_JSON: &str = r#"{"metadata":{"rubocop_version":"1.65.0","ruby_engine":"ruby","ruby_version":"3.3.0","ruby_patchlevel":"0","ruby_platform":"arm64-darwin23"},"files":[{"path":"app/models/user.rb","offenses":[{"severity":"convention","message":"Use single-quoted strings when you don't need string interpolation or special symbols.","cop_name":"Style/StringLiterals","correctable":true,"location":{"start_line":5,"start_column":10,"last_line":5,"last_column":20,"length":11,"line":5,"column":10}},{"severity":"error","message":"Useless assignment to variable - `foo`.","cop_name":"Lint/UselessAssignment","correctable":false,"location":{"start_line":12,"start_column":5,"last_line":12,"last_column":7,"length":3,"line":12,"column":5}}]}],"summary":{"offense_count":2,"target_file_count":1,"inspected_file_count":1}}"#;

    const RUBOCOP_TEXT: &str = "Inspecting 1 file\nW\n\nOffenses:\n\napp/models/user.rb:5:10: C: Style/StringLiterals: Use single-quoted strings.\napp/models/user.rb:12:5: E: Lint/UselessAssignment: Useless assignment to `foo`.\n\n1 file inspected, 2 offenses detected, 1 offense auto-correctable\n";

    #[test]
    fn test_rubocop_tier1_pass() {
        let result = try_parse_json(RUBOCOP_PASS_JSON);
        assert!(result.is_some(), "Expected JSON parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.errors, 0);
        assert_eq!(r.warnings, 0);
        assert!(r.as_ref().contains(" OK"));
    }

    #[test]
    fn test_rubocop_tier1_fail() {
        let result = try_parse_json(RUBOCOP_FAIL_JSON);
        assert!(result.is_some(), "Expected JSON parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.errors, 1);
        // Convention → Info, not counted as warning
        assert_eq!(r.warnings, 0);
        assert_eq!(r.groups.len(), 2);
    }

    #[test]
    fn test_rubocop_tier2_regex() {
        let result = try_parse_regex(RUBOCOP_TEXT);
        assert!(result.is_some(), "Expected regex parse to succeed");
        let r = result.unwrap();
        assert_eq!(r.errors, 1);
        // C letter → Warning
        assert_eq!(r.warnings, 1);
    }

    #[test]
    fn test_rubocop_tier3_passthrough() {
        let output = make_output("completely unparseable output\nno json no regex", "", Some(1));
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_json_produces_full() {
        let output = make_output(RUBOCOP_FAIL_JSON, "", Some(1));
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_text_produces_degraded() {
        let output = make_output(RUBOCOP_TEXT, "", Some(1));
        let result = parse_impl(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_flag_injection_skipped_when_format_present() {
        let args = vec!["--format".to_string(), "progress".to_string()];
        assert!(user_has_flag(&args, &["--format", "-f", "-a", "-A", "--autocorrect", "--autocorrect-all"]));
    }

    #[test]
    fn test_flag_injection_skipped_when_autocorrect_present() {
        let args = vec!["-a".to_string()];
        assert!(user_has_flag(&args, &["--format", "-f", "-a", "-A", "--autocorrect", "--autocorrect-all"]));
    }
}
