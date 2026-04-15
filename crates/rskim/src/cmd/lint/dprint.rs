//! dprint multi-language formatter parser with three-tier degradation (#133).
//!
//! Executes `dprint check` and parses the output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Parse newline-separated file paths from `--list-different` output
//! - **Tier 2 (Degraded)**: Regex on `from <file>:` diff headers
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation
//!
//! # AD-20 (2026-04-15) — check/format split for dprint
//!
//! `dprint check` lists unformatted files (we inject `--list-different` for
//! clean output). `dprint fmt` reformats files and emits a summary.

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{LintIssue, LintResult, LintSeverity};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, group_issues, LinterConfig};

const CONFIG: LinterConfig<'static> = LinterConfig {
    program: "dprint",
    env_overrides: &[],
    install_hint: "Install dprint: https://dprint.dev/install/",
};

/// AD-21 (2026-04-15) — `.+` captures paths with spaces.
static RE_DPRINT_FROM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^from (.+):$").unwrap());

static RE_DPRINT_FORMATTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Formatted (\d+) files?").unwrap());

/// Returns true when the first user arg is `"fmt"`.
fn is_format_mode(args: &[String]) -> bool {
    args.first().is_some_and(|a| a == "fmt")
}

/// Run `skim lint dprint [args...]`.
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

fn run_check(
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
        prepare_check_args,
        parse_check_impl,
    )
}

/// Inject `check` subcommand and `--list-different` if not already present.
fn prepare_check_args(cmd_args: &mut Vec<String>) {
    // Ensure "check" subcommand is present
    if cmd_args.first().is_none_or(|a| a != "check") {
        cmd_args.insert(0, "check".to_string());
    }

    // Inject --list-different for clean parseable output
    if !user_has_flag(cmd_args, &["--list-different"]) {
        cmd_args.push("--list-different".to_string());
    }
}

/// Three-tier parse for `dprint check --list-different` output.
fn parse_check_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    let combined = combine_stdout_stderr(output);

    // Empty output = all files formatted
    if combined.trim().is_empty() {
        return ParseResult::Full(group_issues("dprint", vec![]));
    }

    if let Some(result) = try_parse_list(&combined) {
        return ParseResult::Full(result);
    }

    if let Some(result) = try_parse_diff_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["dprint: list parse failed, using diff regex".to_string()],
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

/// Pass args through unchanged for format mode.
fn prepare_format_args(_cmd_args: &mut Vec<String>) {}

/// Three-tier parse for `dprint fmt` output.
fn parse_format_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_fmt_output(&combined) {
        return ParseResult::Full(result);
    }

    // Empty output on exit 0 = nothing formatted
    if output.exit_code == Some(0) && combined.trim().is_empty() {
        return ParseResult::Full(LintResult::formatted("dprint".to_string(), 0));
    }

    ParseResult::Passthrough(combined.into_owned())
}

/// Tier 1: parse `dprint check --list-different` output.
///
/// Each non-empty line is a file path that needs formatting.
/// File paths must contain `.` (extension) or `/` (path separator) to distinguish
/// them from prose error messages or garbage input.
fn try_parse_list(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        // If a line starts with "from " it's diff output — bail to Tier 2
        if trimmed.starts_with("from ") {
            return None;
        }
        // Skip summary/diagnostic lines
        if trimmed.starts_with("Found ")
            || trimmed.starts_with("Error")
            || trimmed.starts_with("Formatted")
        {
            continue;
        }
        // Must look like a file path (contains `.` for extension or `/` for directory)
        if !trimmed.contains('.') && !trimmed.contains('/') {
            return None;
        }
        issues.push(LintIssue {
            file: trimmed.to_string(),
            line: 0,
            rule: "formatting".to_string(),
            message: "file is not dprint-formatted".to_string(),
            severity: LintSeverity::Warning,
        });
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("dprint", issues))
}

/// Tier 2: regex on `from <file>:` diff headers in `dprint check` output.
fn try_parse_diff_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    for line in text.lines() {
        if let Some(caps) = RE_DPRINT_FROM.captures(line) {
            let path = caps[1].trim().to_string();
            if seen_paths.insert(path.clone()) {
                issues.push(LintIssue {
                    file: path,
                    line: 0,
                    rule: "formatting".to_string(),
                    message: "file is not dprint-formatted".to_string(),
                    severity: LintSeverity::Warning,
                });
            }
        }
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("dprint", issues))
}

/// Tier 1: parse `dprint fmt` output.
///
/// `dprint fmt` emits `Formatted N files.` on success.
fn try_parse_fmt_output(text: &str) -> Option<LintResult> {
    let mut count = 0usize;
    let mut found = false;

    for line in text.lines() {
        if let Some(caps) = RE_DPRINT_FORMATTED.captures(line) {
            count = caps[1].parse().unwrap_or(0);
            found = true;
            break;
        }
    }

    if !found && !text.trim().is_empty() {
        return None;
    }

    if found {
        Some(LintResult::formatted("dprint".to_string(), count))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    //! # AD-25 (2026-04-15) — fixture sourcing
    //!
    //! Fixtures are loaded from `tests/fixtures/cmd/lint/` relative to the
    //! crate manifest directory. Each fixture file is prefixed with a version
    //! comment documenting the tool version it was generated from.
    use super::*;

    fn load_fixture(name: &str) -> String {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cmd/lint")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    #[test]
    fn test_tier1_list_fail() {
        let input = load_fixture("dprint_check_fail.txt");
        let result = try_parse_list(&input);
        assert!(result.is_some(), "Expected Tier 1 list parse to succeed");
        let result = result.unwrap();
        assert_eq!(
            result.warnings, 3,
            "Expected 3 warnings for 3 unformatted files"
        );
        assert_eq!(result.errors, 0);
    }

    #[test]
    fn test_tier1_fmt_output() {
        let input = load_fixture("dprint_fmt_output.txt");
        let result = try_parse_fmt_output(&input);
        assert!(result.is_some(), "Expected Tier 1 fmt parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.files_formatted, Some(3));
        assert!(result.as_ref().contains("files formatted"));
    }

    #[test]
    fn test_tier2_diff_regex() {
        let input = "from src/main.ts:\n  | some diff\nfrom src/utils.ts:\n  | more diff\n";
        let result = try_parse_diff_regex(input);
        assert!(result.is_some(), "Expected Tier 2 diff regex to succeed");
        let result = result.unwrap();
        assert_eq!(result.warnings, 2, "Expected 2 warnings from diff output");
    }

    #[test]
    fn test_parse_check_impl_full() {
        let input = load_fixture("dprint_check_fail.txt");
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
    fn test_parse_check_impl_empty_pass() {
        let output = CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_check_impl(&output);
        assert!(result.is_full(), "Expected Full for empty output");
        if let ParseResult::Full(r) = result {
            assert!(r.as_ref().contains("LINT OK"));
        }
    }

    #[test]
    fn test_parse_check_impl_diff_produces_degraded() {
        let output = CommandOutput {
            stdout: "from src/main.ts:\n  | diff content\n".to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_check_impl(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded from diff output, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_check_impl_garbage_passthrough() {
        let output = CommandOutput {
            stdout: "random garbage not dprint output".to_string(),
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
        let input = load_fixture("dprint_fmt_output.txt");
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
    fn test_is_format_mode() {
        let args: Vec<String> = vec!["fmt".to_string()];
        assert!(is_format_mode(&args));
    }

    #[test]
    fn test_is_format_mode_false_for_check() {
        let args: Vec<String> = vec!["check".to_string()];
        assert!(!is_format_mode(&args));
    }

    #[test]
    fn test_diff_deduplication() {
        let input = "from src/main.ts:\n  | diff\nfrom src/main.ts:\n  | more diff\n";
        let result = try_parse_diff_regex(input);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.warnings, 1, "Duplicate paths must be deduplicated");
    }
}
