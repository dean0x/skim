//! Black Python formatter parser with three-tier degradation (#133).
//!
//! Executes `black --check` and parses the output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Parse `would reformat <path>` / `reformatted <path>` lines
//! - **Tier 2 (Degraded)**: Regex fallback on same patterns
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation
//!
//! # AD-LINT-20 (2026-04-15) — check/format split for black
//!
//! `black --check` lists files that would be reformatted.
//! Bare `black` reformats files in place and emits `reformatted <path>` lines.
//! These are dispatched via `is_format_mode`.

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{LintIssue, LintResult, LintSeverity};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, group_issues, LinterConfig};

const CONFIG: LinterConfig<'static> = LinterConfig {
    program: "black",
    env_overrides: &[("NO_COLOR", "1")],
    install_hint: "Install black: pip install black",
};

/// AD-LINT-21 (2026-04-15) — `.+` captures paths with spaces.
static RE_BLACK_WOULD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^would reformat (.+)$").unwrap());

/// AD-LINT-21 (2026-04-15) — `.+` captures paths with spaces.
static RE_BLACK_REFORMATTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^reformatted (.+)$").unwrap());

static RE_BLACK_SUMMARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+) files? (?:would be )?reformatted").unwrap());

/// Returns true when args do NOT contain `--check` or `--diff`, and args are non-empty.
///
/// When args are empty (stdin piping), we default to check mode because stdin
/// is most commonly piped from `black --check` output. Format mode requires
/// explicit args without `--check` / `--diff`.
fn is_format_mode(args: &[String]) -> bool {
    !args.is_empty() && !user_has_flag(args, &["--check", "--diff"])
}

/// Run `skim lint black [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    if is_format_mode(args) {
        run_format(args, ctx)
    } else {
        run_check(args, ctx)
    }
}

fn run_check(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    super::run_linter(CONFIG, args, ctx, prepare_check_args, parse_check_impl)
}

/// Inject `--check` if no mode flag is present.
fn prepare_check_args(cmd_args: &mut Vec<String>) {
    if !user_has_flag(cmd_args, &["--check", "--diff", "--quiet", "-q"]) {
        cmd_args.push("--check".to_string());
    }
}

/// Three-tier parse for `black --check` output.
fn parse_check_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_check_structured(&combined) {
        return ParseResult::Full(result);
    }

    // Empty output on exit 0 = all files already formatted
    if output.exit_code == Some(0) && combined.trim().is_empty() {
        return ParseResult::Full(group_issues("black", vec![]));
    }

    if let Some(result) = try_parse_check_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["black: structured parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

fn run_format(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    super::run_linter(CONFIG, args, ctx, prepare_format_args, parse_format_impl)
}

/// Pass args through unchanged for format mode.
fn prepare_format_args(_cmd_args: &mut Vec<String>) {}

/// Three-tier parse for `black` (format/apply mode) output.
fn parse_format_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_format_structured(&combined) {
        return ParseResult::Full(result);
    }

    // Empty output on exit 0 = nothing to reformat
    if output.exit_code == Some(0) && combined.trim().is_empty() {
        return ParseResult::Full(LintResult::formatted("black".to_string(), 0));
    }

    if let Some(result) = try_parse_format_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["black: format structured parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// Shared helpers
// ============================================================================

/// Collect `would reformat <path>` issues from `black --check` output.
///
/// Shared by both Tier 1 and Tier 2 check parsers. Tier 1 guards entry with
/// sentinel-string checks; Tier 2 calls this directly.
fn collect_would_reformat_issues(text: &str) -> Vec<LintIssue> {
    text.lines()
        .filter_map(|line| RE_BLACK_WOULD.captures(line))
        .map(|caps| LintIssue {
            file: caps[1].trim().to_string(),
            line: 0,
            rule: "formatting".to_string(),
            message: "would be reformatted".to_string(),
            severity: LintSeverity::Warning,
        })
        .collect()
}

/// Count `reformatted <path>` lines in `black` format output.
///
/// Falls back to the summary line (`N files reformatted`) when no individual
/// `reformatted <path>` lines are present.
///
/// Shared by both Tier 1 and Tier 2 format parsers. Tier 1 guards entry with
/// sentinel-string checks; Tier 2 calls this directly.
fn collect_reformatted_count(text: &str) -> usize {
    let line_count = text
        .lines()
        .filter(|line| RE_BLACK_REFORMATTED.is_match(line))
        .count();

    if line_count > 0 {
        return line_count;
    }

    // Fall back to summary line
    text.lines()
        .find_map(|line| RE_BLACK_SUMMARY.captures(line))
        .and_then(|caps| caps[1].parse().ok())
        .unwrap_or(0)
}

// ============================================================================
// Check mode parsers
// ============================================================================

/// Tier 1: parse `black --check` output.
///
/// Looks for `would reformat <path>` lines. `"All done!"` with `"left unchanged"` = pass.
fn try_parse_check_structured(text: &str) -> Option<LintResult> {
    // Must contain recognisable black check output
    let has_would = text.contains("would reformat");
    let has_all_done = text.contains("All done!");
    let has_unchanged = text.contains("left unchanged");
    let has_summary = text.contains("would be reformatted");

    if !has_would && !has_all_done && !has_summary && !has_unchanged {
        return None;
    }

    let issues = collect_would_reformat_issues(text);

    // Check if this is a pure pass ("All done!" with no files to reformat)
    if issues.is_empty() {
        if has_all_done || has_unchanged || has_summary {
            return Some(group_issues("black", vec![]));
        }
        return None;
    }

    Some(group_issues("black", issues))
}

/// Tier 2: regex fallback for `black --check` output.
fn try_parse_check_regex(text: &str) -> Option<LintResult> {
    let issues = collect_would_reformat_issues(text);

    if issues.is_empty() {
        // Return Some only when a summary line confirms this is black output.
        if text.lines().any(|line| RE_BLACK_SUMMARY.is_match(line)) {
            return Some(group_issues("black", vec![]));
        }
        return None;
    }

    Some(group_issues("black", issues))
}

// ============================================================================
// Format mode parsers
// ============================================================================

/// Tier 1: parse `black` (format/apply mode) output.
///
/// Looks for `reformatted <path>` lines and counts them.
fn try_parse_format_structured(text: &str) -> Option<LintResult> {
    let has_reformatted = text.contains("reformatted ");
    let has_all_done = text.contains("All done!");
    let has_summary = text.contains("files reformatted") || text.contains("left unchanged");

    if !has_reformatted && !has_all_done && !has_summary {
        return None;
    }

    Some(LintResult::formatted(
        "black".to_string(),
        collect_reformatted_count(text),
    ))
}

/// Tier 2: regex fallback for `black` format output.
fn try_parse_format_regex(text: &str) -> Option<LintResult> {
    let count = collect_reformatted_count(text);

    if count == 0 {
        // Only return Some if there's a recognisable summary line — without it
        // we cannot distinguish "nothing to reformat" from "garbage input".
        if text.lines().any(|line| RE_BLACK_SUMMARY.is_match(line)) {
            return Some(LintResult::formatted("black".to_string(), 0));
        }
        return None;
    }

    Some(LintResult::formatted("black".to_string(), count))
}

#[cfg(test)]
mod tests {
    //! # AD-LINT-25 (2026-04-15) — fixture sourcing
    //!
    //! Fixtures are loaded from `tests/fixtures/cmd/lint/` relative to the
    //! crate manifest directory. Each fixture file is prefixed with a version
    //! comment documenting the tool version it was generated from.
    use super::*;

    use crate::cmd::lint::load_lint_fixture as load_fixture;

    #[test]
    fn test_tier1_check_fail() {
        let input = load_fixture("black_check_fail.txt");
        let result = try_parse_check_structured(&input);
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.warnings, 2, "Expected 2 warnings for 2 files");
        assert_eq!(result.errors, 0);
        assert!(result.groups.iter().any(|g| g.rule == "formatting"));
    }

    #[test]
    fn test_tier1_check_pass() {
        let input = load_fixture("black_check_pass.txt");
        let result = try_parse_check_structured(&input);
        assert!(
            result.is_some(),
            "Expected Tier 1 to succeed on pass output"
        );
        let result = result.unwrap();
        assert_eq!(result.errors, 0);
        assert_eq!(result.warnings, 0);
        assert!(result.as_ref().contains("LINT OK"));
    }

    #[test]
    fn test_tier1_format_output() {
        let input = load_fixture("black_format_output.txt");
        let result = try_parse_format_structured(&input);
        assert!(result.is_some(), "Expected Tier 1 format parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.files_formatted, Some(2));
        assert!(result.as_ref().contains("LINT OK"));
        assert!(result.as_ref().contains("files formatted"));
    }

    #[test]
    fn test_tier2_check_regex() {
        // Plain `would reformat` line without the "All done!" context
        let input = "would reformat src/main.py\nwould reformat src/utils.py\n";
        let result = try_parse_check_regex(input);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.warnings, 2);
    }

    #[test]
    fn test_parse_check_impl_full() {
        let input = load_fixture("black_check_fail.txt");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_check_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full tier, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_check_impl_pass() {
        let input = load_fixture("black_check_pass.txt");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_check_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full tier, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(r) = result {
            assert!(r.as_ref().contains("LINT OK"));
        }
    }

    #[test]
    fn test_parse_check_impl_degraded() {
        // Input that only has `would reformat` without "All done!" context
        let output = CommandOutput {
            stdout: String::new(),
            stderr: "would reformat src/main.py\n".to_string(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_check_impl(&output);
        // Should succeed as Full (regex pattern matches just fine via Tier 1)
        assert!(
            result.is_full() || result.is_degraded(),
            "Expected Full or Degraded, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_check_impl_passthrough_garbage() {
        let output = CommandOutput {
            stdout: "random garbage not black output".to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_check_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for garbage input"
        );
    }

    #[test]
    fn test_parse_format_impl_full() {
        let input = load_fixture("black_format_output.txt");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_format_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full tier, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(r) = result {
            assert!(r.as_ref().contains("files formatted"));
        }
    }

    #[test]
    fn test_is_format_mode_no_check() {
        let args: Vec<String> = vec!["src/".to_string()];
        assert!(is_format_mode(&args));
    }

    #[test]
    fn test_is_format_mode_with_check() {
        let args: Vec<String> = vec!["--check".to_string(), "src/".to_string()];
        assert!(!is_format_mode(&args));
    }

    #[test]
    fn test_is_format_mode_with_diff() {
        let args: Vec<String> = vec!["--diff".to_string(), "src/".to_string()];
        assert!(!is_format_mode(&args));
    }

    #[test]
    fn test_path_with_spaces() {
        // AD-LINT-21: paths with spaces must be captured correctly
        let input = "would reformat src/my file.py\n";
        let result = try_parse_check_structured(input);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.groups[0].locations[0], "src/my file.py:0");
    }
}
