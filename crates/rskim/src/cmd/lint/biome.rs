//! Biome JS/TS/CSS formatter+linter parser with three-tier degradation (#133).
//!
//! Executes `biome check`, `biome format`, or `biome lint` and parses the
//! output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON diagnostics (`--reporter=json`) or format file list
//! - **Tier 2 (Degraded)**: Regex on text output (`file:line:col rule`)
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation
//!
//! # AD-24 (2026-04-15) — Biome dual-mode routing
//!
//! Biome's first positional argument is the subcommand: `check`, `format`, or
//! `lint`. `format` mode parses file-list output. `check` and `lint` modes
//! both inject `--reporter=json` for structured diagnostic output (handled by
//! the same `run_check_lint` path).
//!
//! Subcommand detection:
//! - `is_format_mode`: first arg is `"format"`
//! - Otherwise: check/lint mode (default) — handles both `check` and `lint`

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{LintIssue, LintResult, LintSeverity};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, group_issues, LinterConfig};

const CONFIG: LinterConfig<'static> = LinterConfig {
    program: "biome",
    env_overrides: &[("NO_COLOR", "1")],
    install_hint: "Install biome: npm install -g @biomejs/biome",
};

/// AD-21 (2026-04-15) — `.+` captures paths with spaces.
static RE_BIOME_TEXT_DIAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+):(\d+):\d+\s+(\S+)").unwrap());

static RE_BIOME_FORMAT_SUMMARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+) files? are not formatted").unwrap());

static RE_BIOME_FORMAT_SUCCESS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Formatted (\d+) files?").unwrap());

/// Returns true when the first user arg is `"format"`.
fn is_format_mode(args: &[String]) -> bool {
    args.first().is_some_and(|a| a == "format")
}

/// Run `skim lint biome [args...]`.
///
/// # AD-24 (2026-04-15) — Biome dual-mode routing
///
/// Dispatches to `run_format` or `run_check_lint` based on the first argument.
/// `format` → format path. `check`, `lint`, or no subcommand → check/lint path.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    analytics_enabled: bool,
) -> anyhow::Result<std::process::ExitCode> {
    if is_format_mode(args) {
        run_format(args, show_stats, json_output, analytics_enabled)
    } else {
        run_check_lint(args, show_stats, json_output, analytics_enabled)
    }
}

fn run_check_lint(
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
        prepare_check_lint_args,
        parse_check_lint_impl,
    )
}

/// Inject `check` subcommand and `--reporter=json` if not already present.
fn prepare_check_lint_args(cmd_args: &mut Vec<String>) {
    // Ensure a subcommand is present
    let has_subcommand = cmd_args
        .first()
        .is_some_and(|a| matches!(a.as_str(), "check" | "lint" | "format" | "ci"));
    if !has_subcommand {
        cmd_args.insert(0, "check".to_string());
    }

    // Inject --reporter=json if not already present
    if !user_has_flag(cmd_args, &["--reporter"]) {
        cmd_args.push("--reporter=json".to_string());
    }
}

/// Three-tier parse for `biome check` / `biome lint` output.
fn parse_check_lint_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    if let Some(result) = try_parse_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_text_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["biome: JSON parse failed, using text regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

fn run_format(
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
        prepare_format_args,
        parse_format_impl,
    )
}

/// Pass `format` subcommand through; no flag injection needed.
fn prepare_format_args(_cmd_args: &mut Vec<String>) {}

/// Three-tier parse for `biome format` output.
fn parse_format_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_format_structured(&combined) {
        return ParseResult::Full(result);
    }

    // Empty output on exit 0 = all files already formatted
    if output.exit_code == Some(0) && combined.trim().is_empty() {
        return ParseResult::Full(LintResult::formatted("biome".to_string(), 0));
    }

    ParseResult::Passthrough(combined.into_owned())
}

/// Tier 1 (check/lint): parse biome JSON diagnostics.
///
/// `biome check --reporter=json` produces:
/// ```json
/// {"diagnostics":[{"category":"lint/...", "severity":"error", "description":"...",
///   "location":{"file":"...","span":{"start":{"line":N,"column":N}}}}]}
/// ```
fn try_parse_json(stdout: &str) -> Option<LintResult> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let obj = value.as_object()?;

    // Must have "diagnostics" key; null is treated as empty array
    let diag_val = obj.get("diagnostics")?;
    let diag_arr: &[serde_json::Value] = match diag_val {
        serde_json::Value::Null => &[],
        serde_json::Value::Array(arr) => arr.as_slice(),
        _ => return None,
    };

    let mut issues: Vec<LintIssue> = Vec::new();

    for entry in diag_arr {
        let category = entry
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let description = entry
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let severity_str = entry.get("severity").and_then(|v| v.as_str()).unwrap_or("");

        let severity = match severity_str {
            "error" | "fatal" => LintSeverity::Error,
            "warning" => LintSeverity::Warning,
            _ => LintSeverity::Info,
        };

        let location = entry.get("location");
        let file = location
            .and_then(|l| l.get("file"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let line = location
            .and_then(|l| l.get("span"))
            .and_then(|s| s.get("start"))
            .and_then(|s| s.get("line"))
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0);

        issues.push(LintIssue {
            file,
            line,
            rule: category.to_string(),
            message: description.to_string(),
            severity,
        });
    }

    Some(group_issues("biome", issues))
}

/// Tier 2 (check/lint): regex on biome text output.
///
/// Biome text format: `file:line:col rule ━━━...`
fn try_parse_text_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();

    for line in text.lines() {
        if let Some(caps) = RE_BIOME_TEXT_DIAG.captures(line) {
            let file = caps[1].to_string();
            let line_num: u32 = caps[2].parse().unwrap_or(0);
            let rule = caps[3].to_string();

            // Skip lines that look like context (indented with spaces, start with numbers)
            // The rule field starts with "lint/" or similar category prefix
            if !rule.contains('/') && !rule.starts_with("lint") && !rule.starts_with("format") {
                continue;
            }

            issues.push(LintIssue {
                file,
                line: line_num,
                rule,
                message: String::new(),
                severity: LintSeverity::Warning,
            });
        }
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("biome", issues))
}

/// Tier 1 (format): parse biome format output.
///
/// `biome format --write` may emit `Formatted N files in Xms`.
/// `biome format` (check mode) emits file paths followed by summary.
fn try_parse_format_structured(text: &str) -> Option<LintResult> {
    // Check for success pattern
    if let Some(caps) = RE_BIOME_FORMAT_SUCCESS.captures_iter(text).next() {
        let n: usize = caps[1].parse().unwrap_or(0);
        return Some(LintResult::formatted("biome".to_string(), n));
    }

    // Check for "N files are not formatted" pattern (check mode failure)
    let has_not_formatted = RE_BIOME_FORMAT_SUMMARY.is_match(text);

    // Collect unformatted file paths (lines before the summary)
    let mut issues: Vec<LintIssue> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        // Skip summary/diagnostic lines
        if trimmed.contains("files in ")
            || trimmed.contains("are not formatted")
            || trimmed.contains("Compared")
        {
            break;
        }
        // File path lines have no spaces and look like paths
        if !trimmed.contains(' ') && (trimmed.contains('/') || trimmed.contains('.')) {
            issues.push(LintIssue {
                file: trimmed.to_string(),
                line: 0,
                rule: "formatting".to_string(),
                message: "file is not formatted".to_string(),
                severity: LintSeverity::Warning,
            });
        }
    }

    if !issues.is_empty() {
        return Some(group_issues("biome", issues));
    }

    if has_not_formatted {
        // We saw the summary but couldn't parse individual files
        return Some(group_issues("biome", vec![]));
    }

    None
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

    /// biome_check_fail.json: generated from biome v1.7.0 on 2026-04-15.
    #[test]
    fn test_tier1_json_fail() {
        let input = load_fixture("biome_check_fail.json");
        let result = try_parse_json(&input);
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.errors, 1, "Expected 1 error");
        assert_eq!(result.warnings, 1, "Expected 1 warning");
    }

    #[test]
    fn test_tier1_json_empty_diagnostics() {
        let input = r#"{"diagnostics":[]}"#;
        let result = try_parse_json(input);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.errors, 0);
        assert_eq!(result.warnings, 0);
        assert!(result.as_ref().contains("LINT OK"));
    }

    #[test]
    fn test_tier1_json_null_diagnostics() {
        let input = r#"{"diagnostics":null}"#;
        let result = try_parse_json(input);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.errors, 0);
        assert_eq!(result.warnings, 0);
    }

    #[test]
    fn test_tier2_text_regex() {
        let input = load_fixture("biome_check_text.txt");
        let result = try_parse_text_regex(&input);
        assert!(result.is_some(), "Expected Tier 2 text regex to succeed");
        let result = result.unwrap();
        assert!(
            result.errors + result.warnings >= 2,
            "Expected at least 2 issues"
        );
    }

    #[test]
    fn test_tier1_format_file_list() {
        let input = load_fixture("biome_format_fail.txt");
        let result = try_parse_format_structured(&input);
        assert!(result.is_some(), "Expected Tier 1 format parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.warnings, 2, "Expected 2 unformatted files");
    }

    #[test]
    fn test_tier1_format_success() {
        let input = "Formatted 3 files in 45ms\n";
        let result = try_parse_format_structured(input);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.files_formatted, Some(3));
    }

    #[test]
    fn test_parse_check_impl_json_produces_full() {
        let input = load_fixture("biome_check_fail.json");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_check_lint_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full tier, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_check_impl_text_produces_degraded() {
        let input = load_fixture("biome_check_text.txt");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_check_lint_impl(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded from text input, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_check_impl_garbage_passthrough() {
        let output = CommandOutput {
            stdout: "random garbage not biome output".to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_check_lint_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for garbage input"
        );
    }

    #[test]
    fn test_is_format_mode() {
        let args: Vec<String> = vec!["format".to_string(), "--write".to_string()];
        assert!(is_format_mode(&args));
    }

    #[test]
    fn test_is_format_mode_false_for_check() {
        let args: Vec<String> = vec!["check".to_string()];
        assert!(!is_format_mode(&args));
    }

    #[test]
    fn test_parse_format_impl_empty_exit0() {
        let output = CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_format_impl(&output);
        assert!(result.is_full(), "Expected Full for empty exit 0");
        if let ParseResult::Full(r) = result {
            assert!(r.as_ref().contains("LINT OK"));
        }
    }
}
