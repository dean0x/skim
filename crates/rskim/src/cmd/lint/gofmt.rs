//! gofmt parser with three-tier degradation (#133).
//!
//! Executes `gofmt -l` and parses unformatted file paths into a `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Parse newline-separated `.go` file paths from `-l` output
//! - **Tier 2 (Degraded)**: Regex on `-d` diff output (`--- <path>` headers)
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation
//!
//! `gofmt` has no check/format mode split — all modes are controlled by flags
//! (`-l` lists unformatted files, `-d` shows diffs, `-w` rewrites in place).
//! We inject `-l` by default for clean parseable output.

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{LintIssue, LintResult, LintSeverity};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, group_issues, LinterConfig};

const CONFIG: LinterConfig<'static> = LinterConfig {
    program: "gofmt",
    env_overrides: &[],
    install_hint: "gofmt ships with Go: https://go.dev/dl/",
};

/// AD-21 (2026-04-15) — `.+` captures paths with spaces. Strips `.orig` suffix.
static RE_GOFMT_DIFF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^--- (.+?)(?:\.orig)?$").unwrap());

/// Validate that a line is a `.go` file path (Tier 1 `-l` output).
static RE_GOFMT_FILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(.+\.go)$").unwrap());

/// Run `skim lint gofmt [args...]`.
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

/// Inject `-l` if no mode flag is present.
fn prepare_args(cmd_args: &mut Vec<String>) {
    if !user_has_flag(cmd_args, &["-l", "-d", "-w", "-e"]) {
        cmd_args.insert(0, "-l".to_string());
    }
}

/// Three-tier parse function for gofmt output.
fn parse_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    let combined = combine_stdout_stderr(output);

    // Empty output = all files correctly formatted
    if combined.trim().is_empty() {
        return ParseResult::Full(group_issues("gofmt", vec![]));
    }

    if let Some(result) = try_parse_list(&combined) {
        return ParseResult::Full(result);
    }

    if let Some(result) = try_parse_diff_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["gofmt: list parse failed, using diff regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

/// Tier 1: parse `gofmt -l` output (newline-separated `.go` file paths).
///
/// Each line is a path to an unformatted file. Empty output = all formatted.
fn try_parse_list(text: &str) -> Option<LintResult> {
    // All lines must look like .go file paths; if any line doesn't match, bail out
    // (this could be diff output or something else)
    let mut issues: Vec<LintIssue> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Skip comment lines in fixtures
        if trimmed.starts_with("//") {
            continue;
        }
        if let Some(caps) = RE_GOFMT_FILE.captures(trimmed) {
            issues.push(LintIssue {
                file: caps[1].to_string(),
                line: 0,
                rule: "formatting".to_string(),
                message: "file is not gofmt-formatted".to_string(),
                severity: LintSeverity::Warning,
            });
        } else {
            // Line doesn't look like a .go path — this isn't -l output
            return None;
        }
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("gofmt", issues))
}

/// Tier 2: parse `gofmt -d` diff output via regex.
///
/// Looks for `--- <path>` headers, stripping the `.orig` suffix gofmt adds.
fn try_parse_diff_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    for line in text.lines() {
        if let Some(caps) = RE_GOFMT_DIFF.captures(line) {
            let path = caps[1].trim().to_string();
            // Skip /dev/null or empty
            if path == "/dev/null" || path.is_empty() {
                continue;
            }
            if seen_paths.insert(path.clone()) {
                issues.push(LintIssue {
                    file: path,
                    line: 0,
                    rule: "formatting".to_string(),
                    message: "file is not gofmt-formatted".to_string(),
                    severity: LintSeverity::Warning,
                });
            }
        }
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("gofmt", issues))
}

#[cfg(test)]
mod tests {
    //! # AD-25 (2026-04-15) — fixture sourcing
    //!
    //! Fixtures are loaded from `tests/fixtures/cmd/lint/` relative to the
    //! crate manifest directory. Each fixture file is prefixed with a version
    //! comment documenting the tool version it was generated from.
    use super::*;

    use crate::cmd::lint::load_lint_fixture as load_fixture;

    #[test]
    fn test_tier1_list_fail() {
        let input = load_fixture("gofmt_list_fail.txt");
        let result = try_parse_list(&input);
        assert!(result.is_some(), "Expected Tier 1 list parse to succeed");
        let result = result.unwrap();
        assert_eq!(
            result.warnings, 3,
            "Expected 3 warnings for 3 unformatted files"
        );
        assert_eq!(result.errors, 0);
        assert!(result.groups.iter().any(|g| g.rule == "formatting"));
    }

    #[test]
    fn test_tier1_empty_output_pass() {
        let output = CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(result.is_full(), "Expected Full for empty output");
        if let ParseResult::Full(r) = result {
            assert!(r.as_ref().contains("LINT OK"));
        }
    }

    #[test]
    fn test_tier2_diff_regex() {
        let input = load_fixture("gofmt_diff_fail.txt");
        let result = try_parse_diff_regex(&input);
        assert!(result.is_some(), "Expected Tier 2 diff regex to succeed");
        let result = result.unwrap();
        // 2 unique files from the diff fixture
        assert_eq!(result.warnings, 2, "Expected 2 warnings from diff output");
    }

    #[test]
    fn test_parse_impl_list_produces_full() {
        let input = load_fixture("gofmt_list_fail.txt");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full tier from list output, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_diff_produces_degraded() {
        let input = load_fixture("gofmt_diff_fail.txt");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded from diff output, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_garbage_passthrough() {
        let output = CommandOutput {
            stdout: "random garbage not gofmt output".to_string(),
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
    fn test_diff_deduplication() {
        // Same file appearing in two diff hunks — should only create one issue
        let input = "--- cmd/server.go.orig\n+++ cmd/server.go\n--- cmd/server.go.orig\n+++ cmd/server.go\n";
        let result = try_parse_diff_regex(input);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.warnings, 1, "Duplicate paths must be deduplicated");
    }

    #[test]
    fn test_path_with_spaces() {
        // AD-21: paths with spaces must be captured correctly
        let input = "my files/server.go\n";
        let result = try_parse_list(input);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.groups[0].locations[0], "my files/server.go:0");
    }

    #[test]
    fn test_orig_suffix_stripped_from_diff() {
        let input = "--- cmd/server.go.orig\n+++ cmd/server.go\n";
        let result = try_parse_diff_regex(input);
        assert!(result.is_some());
        let result = result.unwrap();
        // File path should NOT contain .orig
        assert_eq!(result.groups[0].locations[0], "cmd/server.go:0");
    }
}
