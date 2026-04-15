//! Ruff linter parser with three-tier degradation (#104).
//!
//! Executes `ruff check` and parses the output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON array parsing (`--output-format json`)
//! - **Tier 2 (Degraded)**: Regex on default text output
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation
//!
//! # AD-20 (2026-04-15) — check/format split for ruff
//!
//! `ruff check` and `ruff format --check` produce structured check output.
//! `ruff format` (without `--check`) reformats files and emits `Would reformat: <path>`
//! lines followed by a summary. These are handled by separate `run_check` and
//! `run_format` paths dispatched via `is_format_mode`.

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{LintIssue, LintResult, LintSeverity};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, group_issues, LinterConfig};

const CONFIG: LinterConfig<'static> = LinterConfig {
    program: "ruff",
    env_overrides: &[("NO_COLOR", "1")],
    install_hint: "Install ruff: pip install ruff",
};

// Static regex patterns compiled once via LazyLock.
static RE_RUFF_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+):(\d+):\d+:\s+(\S+)\s+(.+)").unwrap());

/// AD-20 (2026-04-15) — check/format split: `ruff format` "Would reformat: <path>" line.
static RE_RUFF_FORMAT_WOULD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Would reformat:\s+(.+)$").unwrap());

/// AD-20 (2026-04-15) — check/format split: `ruff format` pass summary line.
///
/// Matches both `"5 files already formatted"` and `"3 files left unchanged"`.
static RE_RUFF_FORMAT_UNCHANGED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+) files? (?:already formatted|left unchanged)").unwrap());

/// Returns true when the first user argument is `"format"`.
///
/// This distinguishes `ruff format [--check] ...` from `ruff check ...`.
fn is_format_mode(args: &[String]) -> bool {
    args.first().is_some_and(|a| a == "format")
}

/// Run `skim lint ruff [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    analytics_enabled: bool,
) -> anyhow::Result<std::process::ExitCode> {
    if is_format_mode(args) {
        run_format(args, show_stats, json_output, analytics_enabled)
    } else {
        run_check(args, show_stats, json_output, analytics_enabled)
    }
}

// ============================================================================
// Check mode (existing behaviour, unchanged)
// ============================================================================

fn run_check(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    analytics_enabled: bool,
) -> anyhow::Result<std::process::ExitCode> {
    // Strip the consumed "check" subcommand so that stdin is detected when no
    // file args remain (e.g., `cat output.txt | skim lint ruff check`).
    // `prepare_check_args` re-injects "check" unconditionally when absent.
    let remaining: Vec<String> = args.iter().skip(
        usize::from(args.first().is_some_and(|a| a == "check"))
    ).cloned().collect();
    super::run_linter(
        CONFIG,
        &remaining,
        show_stats,
        json_output,
        analytics_enabled,
        prepare_check_args,
        parse_check_impl,
    )
}

/// Ensure "check" subcommand is present and inject `--output-format json`.
fn prepare_check_args(cmd_args: &mut Vec<String>) {
    // Ensure "check" subcommand is present if args don't start with it
    if cmd_args.first().is_none_or(|a| a != "check") {
        cmd_args.insert(0, "check".to_string());
    }

    // Inject --output-format json if not already present
    if !user_has_flag(cmd_args, &["--output-format"]) {
        cmd_args.push("--output-format".to_string());
        cmd_args.push("json".to_string());
    }
}

/// Three-tier parse function for ruff check output.
fn parse_check_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    if let Some(result) = try_parse_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["ruff: JSON parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// Format mode (AD-20)
// ============================================================================

fn run_format(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    analytics_enabled: bool,
) -> anyhow::Result<std::process::ExitCode> {
    // Strip the consumed "format" subcommand so that stdin is detected when no
    // file args remain (e.g., `cat output.txt | skim lint ruff format`).
    // `prepare_format_args` re-injects "format" for binary execution.
    let remaining: Vec<String> = args.iter().skip(1).cloned().collect();
    super::run_linter(
        CONFIG,
        &remaining,
        show_stats,
        json_output,
        analytics_enabled,
        prepare_format_args,
        parse_format_impl,
    )
}

/// Re-inject the `format` subcommand stripped by `run_format`.
///
/// When `ruff format` is executed as a binary, `format` must be the first
/// argument. We strip it before `run_linter` to allow stdin detection, then
/// restore it here.
fn prepare_format_args(cmd_args: &mut Vec<String>) {
    if cmd_args.first().is_none_or(|a| a != "format") {
        cmd_args.insert(0, "format".to_string());
    }
}

/// Three-tier parse for `ruff format [--check]` output.
///
/// # AD-20 (2026-04-15) — ruff format output parsing
///
/// `ruff format --check` prints:
/// ```text
/// Would reformat: src/main.py
/// 2 files would be reformatted, 3 files left unchanged
/// ```
///
/// `ruff format` (apply mode) may print the same lines on a dry-run check but
/// in apply mode it just reformats silently. Either way we parse the output.
///
/// Exit 0 with empty stdout = all files already formatted → `LINT OK`.
fn parse_format_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    let combined = combine_stdout_stderr(output);

    // Tier 1: look for "Would reformat:" lines or well-known summary patterns
    if let Some(result) = try_parse_format_structured(&combined) {
        return ParseResult::Full(result);
    }

    // Empty output on exit 0 = already formatted, nothing to do
    if output.exit_code == Some(0) && combined.trim().is_empty() {
        return ParseResult::Full(LintResult::formatted("ruff".to_string(), 0));
    }

    // Tier 2: regex on the combined text
    if let Some(result) = try_parse_format_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["ruff: format structured parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

/// Tier 1: structured parse of `ruff format [--check]` output.
fn try_parse_format_structured(text: &str) -> Option<LintResult> {
    // If there's no recognisable ruff format output, bail
    let has_would = text.contains("Would reformat:");
    let has_summary = text.contains("already formatted")
        || text.contains("reformatted")
        || text.contains("left unchanged");

    if !has_would && !has_summary {
        return None;
    }

    // Collect files that would be reformatted
    let mut issues: Vec<LintIssue> = Vec::new();
    for line in text.lines() {
        if let Some(caps) = RE_RUFF_FORMAT_WOULD.captures(line) {
            issues.push(LintIssue {
                file: caps[1].trim().to_string(),
                line: 0,
                rule: "formatting".to_string(),
                message: "would be reformatted".to_string(),
                severity: LintSeverity::Warning,
            });
        }
    }

    // Check if this is a pure "all already formatted" pass
    if issues.is_empty() {
        // Try to extract file count from summary
        for line in text.lines() {
            if let Some(caps) = RE_RUFF_FORMAT_UNCHANGED.captures(line) {
                let n: usize = caps[1].parse().unwrap_or(0);
                return Some(LintResult::formatted("ruff".to_string(), n));
            }
        }
        // Summary but no count found — still a pass
        if has_summary {
            return Some(LintResult::formatted("ruff".to_string(), 0));
        }
        return None;
    }

    Some(group_issues("ruff", issues))
}

/// Tier 2: regex fallback for `ruff format` output.
fn try_parse_format_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();

    for line in text.lines() {
        if let Some(caps) = RE_RUFF_FORMAT_WOULD.captures(line) {
            issues.push(LintIssue {
                file: caps[1].trim().to_string(),
                line: 0,
                rule: "formatting".to_string(),
                message: "would be reformatted".to_string(),
                severity: LintSeverity::Warning,
            });
        }
    }

    if issues.is_empty() {
        // Check for standalone unchanged summary
        for line in text.lines() {
            if RE_RUFF_FORMAT_UNCHANGED.is_match(line) {
                return Some(LintResult::formatted("ruff".to_string(), 0));
            }
        }
        return None;
    }

    Some(group_issues("ruff", issues))
}

// ============================================================================
// Tier 1: JSON parsing (check mode)
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
// Tier 2: regex fallback (check mode)
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
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cmd/lint")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    // -------------------------------------------------------------------------
    // Check mode (existing tests, unchanged)
    // -------------------------------------------------------------------------

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
        let result = parse_check_impl(&output);
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
        let result = parse_check_impl(&output);
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
        let result = parse_check_impl(&output);
        assert!(result.is_passthrough());
    }

    // -------------------------------------------------------------------------
    // Format mode (AD-20)
    // -------------------------------------------------------------------------

    /// AD-20: `ruff format --check` failure — files would be reformatted.
    #[test]
    fn test_ruff_format_check_fail_structured() {
        let input = load_fixture("ruff_format_check_fail.txt");
        let result = try_parse_format_structured(&input);
        assert!(
            result.is_some(),
            "Expected structured parse to succeed on format --check failure"
        );
        let result = result.unwrap();
        assert_eq!(
            result.warnings, 2,
            "Expected 2 warnings for 2 files to reformat"
        );
        assert_eq!(result.errors, 0);
        assert!(result.groups.iter().any(|g| g.rule == "formatting"));
    }

    /// AD-20: `ruff format` pass — all files already formatted.
    #[test]
    fn test_ruff_format_pass_structured() {
        let input = load_fixture("ruff_format_pass.txt");
        let result = try_parse_format_structured(&input);
        assert!(
            result.is_some(),
            "Expected structured parse to succeed on format pass"
        );
        let result = result.unwrap();
        assert_eq!(result.errors, 0);
        assert_eq!(result.warnings, 0);
        assert!(result.as_ref().contains("LINT OK"));
        assert!(
            result.as_ref().contains("files formatted"),
            "Expected format-mode render, got: {}",
            result.as_ref()
        );
    }

    /// AD-20: empty output on exit 0 = no files reformatted.
    #[test]
    fn test_ruff_format_empty_output_is_pass() {
        let output = CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_format_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full for empty output on exit 0, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(r) = result {
            assert!(r.as_ref().contains("LINT OK"));
        }
    }

    /// AD-20: is_format_mode dispatches correctly.
    #[test]
    fn test_is_format_mode_true() {
        let args: Vec<String> = vec!["format".to_string(), "--check".to_string()];
        assert!(is_format_mode(&args));
    }

    #[test]
    fn test_is_format_mode_false_for_check() {
        let args: Vec<String> = vec!["check".to_string(), ".".to_string()];
        assert!(!is_format_mode(&args));
    }

    #[test]
    fn test_is_format_mode_false_for_empty() {
        let args: Vec<String> = vec![];
        assert!(!is_format_mode(&args));
    }

    /// AD-20: parse_format_impl on fixture produces Full tier.
    #[test]
    fn test_parse_format_impl_fail_fixture_is_full() {
        let input = load_fixture("ruff_format_check_fail.txt");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_format_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full parse, got {}",
            result.tier_name()
        );
    }

    /// AD-20: parse_format_impl on pass fixture produces Full tier.
    #[test]
    fn test_parse_format_impl_pass_fixture_is_full() {
        let input = load_fixture("ruff_format_pass.txt");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_format_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full parse, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(r) = result {
            assert!(r.as_ref().contains("LINT OK"));
            assert!(r.as_ref().contains("files formatted"));
        }
    }

    // -------------------------------------------------------------------------
    // AD-26: stdin detection — subcommand arg stripping
    // -------------------------------------------------------------------------

    /// AD-26: `prepare_format_args` re-injects "format" when absent.
    #[test]
    fn test_prepare_format_args_injects_format() {
        let mut cmd_args: Vec<String> = vec![];
        prepare_format_args(&mut cmd_args);
        assert_eq!(cmd_args, vec!["format".to_string()]);
    }

    /// AD-26: `prepare_format_args` does not duplicate "format" when already present.
    #[test]
    fn test_prepare_format_args_no_duplicate_format() {
        let mut cmd_args: Vec<String> = vec!["format".to_string(), "--check".to_string()];
        prepare_format_args(&mut cmd_args);
        assert_eq!(cmd_args[0], "format");
        assert_eq!(cmd_args.iter().filter(|a| *a == "format").count(), 1);
    }

    /// AD-26: `prepare_format_args` re-injects "format" when only file args remain.
    #[test]
    fn test_prepare_format_args_with_file_arg() {
        let mut cmd_args: Vec<String> = vec!["src/".to_string()];
        prepare_format_args(&mut cmd_args);
        assert_eq!(cmd_args[0], "format");
        assert_eq!(cmd_args[1], "src/");
    }

    /// AD-26: `prepare_check_args` re-injects "check" when absent.
    #[test]
    fn test_prepare_check_args_injects_check() {
        let mut cmd_args: Vec<String> = vec![];
        prepare_check_args(&mut cmd_args);
        assert!(cmd_args.contains(&"check".to_string()));
        assert!(cmd_args.contains(&"--output-format".to_string()));
    }

    /// AD-26: `prepare_check_args` does not duplicate "check" when already present.
    #[test]
    fn test_prepare_check_args_no_duplicate_check() {
        let mut cmd_args: Vec<String> = vec!["check".to_string()];
        prepare_check_args(&mut cmd_args);
        assert_eq!(cmd_args.iter().filter(|a| *a == "check").count(), 1);
    }
}
