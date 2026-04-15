//! oxlint JS/TS linter parser with three-tier degradation (#133).
//!
//! Executes `oxlint` and parses the output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON array parsing (`--format=json`, ESLint-compatible schema)
//! - **Tier 2 (Degraded)**: Regex on fancy text output (Rust-style `╭─[file:line:col]`)
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{LintIssue, LintResult, LintSeverity};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, group_issues, LinterConfig};

const CONFIG: LinterConfig<'static> = LinterConfig {
    program: "oxlint",
    env_overrides: &[("NO_COLOR", "1")],
    install_hint: "Install oxlint: npm install -g oxlint",
};

/// Matches Rust-style location markers in oxlint fancy output.
/// AD-21 (2026-04-15) — `.+` captures paths with spaces.
static RE_OXLINT_LOCATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"╭─\[(.+):(\d+):\d+\]").unwrap());

/// Matches rule name lines in oxlint fancy output (`  × rule-name` or `  ⚠ rule-name`).
static RE_OXLINT_RULE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^  [×⚠] (.+)$").unwrap());

/// Run `skim lint oxlint [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    analytics_enabled: bool,
) -> anyhow::Result<std::process::ExitCode> {
    super::run_linter(
        CONFIG,
        args,
        show_stats,
        json_output,
        analytics_enabled,
        prepare_args,
        parse_impl,
    )
}

/// Inject `--format=json` if no `--format` flag is present.
fn prepare_args(cmd_args: &mut Vec<String>) {
    if !user_has_flag(cmd_args, &["--format"]) {
        cmd_args.push("--format=json".to_string());
    }
}

/// Three-tier parse function for oxlint output.
fn parse_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    if let Some(result) = try_parse_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_fancy_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["oxlint: JSON parse failed, using text regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

/// Tier 1: parse oxlint JSON output (ESLint-compatible schema).
///
/// Format: `[{"filePath": "...", "messages": [{"ruleId": "...", "severity": 2,
///            "message": "...", "line": N, "column": N}]}]`
///
/// `severity` values: 2 = error, 1 = warning, 0 = off.
fn try_parse_json(stdout: &str) -> Option<LintResult> {
    let arr: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).ok()?;

    // An empty array is valid — means clean run
    let mut issues: Vec<LintIssue> = Vec::new();

    for file_entry in &arr {
        let file_path = file_entry
            .get("filePath")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let messages = file_entry
            .get("messages")
            .and_then(|v| v.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(&[]);

        for msg in messages {
            let rule_id = msg
                .get("ruleId")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)")
                .to_string();
            let message = msg
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let line = msg
                .get("line")
                .and_then(|v| v.as_u64())
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0);
            let severity_code = msg.get("severity").and_then(|v| v.as_u64()).unwrap_or(1);

            let severity = match severity_code {
                2 => LintSeverity::Error,
                1 => LintSeverity::Warning,
                _ => LintSeverity::Info,
            };

            issues.push(LintIssue {
                file: file_path.clone(),
                line,
                rule: rule_id,
                message,
                severity,
            });
        }
    }

    Some(group_issues("oxlint", issues))
}

/// Tier 2: regex on oxlint fancy (Rust-style) text output.
///
/// Pairs `  × rule-name` / `  ⚠ rule-name` lines with the following
/// `╭─[file:line:col]` location marker.
fn try_parse_fancy_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        // Look for a rule name line
        if let Some(rule_caps) = RE_OXLINT_RULE.captures(lines[i]) {
            let rule = rule_caps[1].trim().to_string();
            // Look ahead for the location marker
            let severity = if lines[i].contains('×') {
                LintSeverity::Error
            } else {
                LintSeverity::Warning
            };

            let mut found_location = false;
            let lookahead_end = lines.len().min(i + 4);
            let lookahead = &lines[(i + 1)..lookahead_end];
            for (offset, &lookahead_line) in lookahead.iter().enumerate() {
                if let Some(loc_caps) = RE_OXLINT_LOCATION.captures(lookahead_line) {
                    let file = loc_caps[1].to_string();
                    let line_num: u32 = loc_caps[2].parse().unwrap_or(0);
                    issues.push(LintIssue {
                        file,
                        line: line_num,
                        rule: rule.clone(),
                        message: String::new(),
                        severity,
                    });
                    found_location = true;
                    i += offset + 2;
                    break;
                }
            }
            if !found_location {
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("oxlint", issues))
}

#[cfg(test)]
mod tests {
    //! # AD-25 (2026-04-15) — fixture sourcing
    //!
    //! Fixtures are loaded from `tests/fixtures/cmd/lint/` relative to the
    //! crate manifest directory. JSON fixtures are documented in test function
    //! doc comments (no inline comments allowed in JSON).
    use super::*;

    fn load_fixture(name: &str) -> String {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cmd/lint")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    /// oxlint_fail.json: generated from oxlint v0.3.0 on 2026-04-15.
    /// Contains 3 messages: 1 error (no-unused-vars) + 1 warning (eqeqeq) in app.ts,
    /// 1 warning (no-console) in utils.ts.
    #[test]
    fn test_tier1_json_fail() {
        let input = load_fixture("oxlint_fail.json");
        let result = try_parse_json(&input);
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.errors, 1, "Expected 1 error");
        assert_eq!(result.warnings, 2, "Expected 2 warnings");
    }

    /// oxlint_pass.json: empty array — clean run.
    #[test]
    fn test_tier1_json_pass() {
        let input = load_fixture("oxlint_pass.json");
        let result = try_parse_json(&input);
        assert!(
            result.is_some(),
            "Expected Tier 1 to succeed on empty array"
        );
        let result = result.unwrap();
        assert_eq!(result.errors, 0);
        assert_eq!(result.warnings, 0);
        assert!(result.as_ref().contains("LINT OK"));
    }

    #[test]
    fn test_tier2_fancy_regex() {
        let input = load_fixture("oxlint_text.txt");
        let result = try_parse_fancy_regex(&input);
        assert!(result.is_some(), "Expected Tier 2 regex to succeed");
        let result = result.unwrap();
        assert_eq!(
            result.errors + result.warnings,
            2,
            "Expected 2 total issues"
        );
    }

    #[test]
    fn test_parse_impl_json_produces_full() {
        let input = load_fixture("oxlint_fail.json");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full tier, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_text_produces_degraded() {
        let input = load_fixture("oxlint_text.txt");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded from text input, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_garbage_passthrough() {
        let output = CommandOutput {
            stdout: "random garbage not oxlint output".to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for garbage input"
        );
    }

    #[test]
    fn test_json_severity_mapping() {
        // severity 2 = Error, severity 1 = Warning
        let input = r#"[{"filePath":"a.ts","messages":[{"ruleId":"foo","severity":2,"message":"err","line":1,"column":1},{"ruleId":"bar","severity":1,"message":"warn","line":2,"column":1}]}]"#;
        let result = try_parse_json(input).expect("must parse");
        assert_eq!(result.errors, 1);
        assert_eq!(result.warnings, 1);
    }

    #[test]
    fn test_path_with_spaces() {
        // AD-21: location markers with spaces in paths
        let input = "  × no-unused-vars\n    ╭─[src/my file.ts:5:7]\n";
        let result = try_parse_fancy_regex(input);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.groups[0].locations[0], "src/my file.ts:5");
    }
}
