//! Prettier parser with three-tier degradation (#116).
//!
//! Executes `prettier` and parses check output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Parse `[warn]` lines as file paths
//! - **Tier 2 (Degraded)**: Regex fallback on other output formats
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation
//!
//! # AD-20 (2026-04-15) — check/format split for prettier
//!
//! `prettier --check` detects files that need formatting (existing behaviour).
//! `prettier --write` (or `-w`) rewrites files and emits one file path per line
//! on stdout. These are handled by separate `run_check` and `run_format` paths
//! dispatched via `is_format_mode`.

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{LintIssue, LintResult, LintSeverity};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, group_issues, LinterConfig};

const CONFIG: LinterConfig<'static> = LinterConfig {
    program: "prettier",
    env_overrides: &[("NO_COLOR", "1")],
    install_hint: "Install prettier via npm: npm install -g prettier",
};

/// AD-21 (2026-04-15) — Path-aware regex patterns: `.+\S` captures full path including
/// spaces while excluding trailing whitespace. Replaces `\S+` which broke on paths
/// containing spaces (e.g., `[warn] src/My Component.tsx`).
static RE_PRETTIER_WARN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[warn\]\s+(.+\S)").unwrap());

static RE_PRETTIER_SUMMARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[warn\]\s+Code style issues found").unwrap());

/// AD-21 (2026-04-15) — Path-aware regex patterns: `.+` replaces `[^\s]+` so that
/// paths with spaces (e.g., `src/My Component.ts needs formatting`) are captured.
static RE_PRETTIER_FILE_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^(.+\.[a-zA-Z]{1,6})\s+needs? formatting").unwrap());

/// AD-20 (2026-04-15) — format mode: match a file path written by `prettier --write`.
///
/// A written path is a line containing at least one `.` and a short extension,
/// with no surrounding structure. We use a simple `^(.+\.[a-zA-Z]{1,6})$` anchored
/// match so that summary/error lines are not mistaken for paths.
static RE_PRETTIER_WRITTEN_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+\.[a-zA-Z]{1,6})\s*$").unwrap());

/// Returns true when `--write` or `-w` is present in the user arguments.
fn is_format_mode(args: &[String]) -> bool {
    user_has_flag(args, &["--write", "-w"])
}

/// Run `skim lint prettier [args...]`.
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

// ============================================================================
// Check mode (existing behaviour, unchanged)
// ============================================================================

fn run_check(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    super::run_linter(CONFIG, args, ctx, prepare_check_args, parse_check_impl)
}

/// Inject `--check` if not already present.
fn prepare_check_args(cmd_args: &mut Vec<String>) {
    if !user_has_flag(cmd_args, &["--check", "-c", "--list-different", "-l"]) {
        cmd_args.insert(0, "--check".to_string());
    }
}

/// Three-tier parse function for prettier check output.
fn parse_check_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    if let Some(result) = try_parse_structured(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Empty stdout on exit 0 = all files formatted correctly
    if output.exit_code == Some(0) && output.stdout.trim().is_empty() {
        return ParseResult::Full(group_issues("prettier", vec![]));
    }

    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["prettier: structured parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// Format mode (AD-20)
// ============================================================================

fn run_format(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    // Strip --write / -w so that `args.is_empty()` is true when no file targets
    // remain, enabling stdin detection (e.g., `cat output.txt | skim lint prettier --write`).
    // `prepare_format_args` re-injects --write for binary execution.
    let remaining: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--write" && a.as_str() != "-w")
        .cloned()
        .collect();
    super::run_linter(
        CONFIG,
        &remaining,
        ctx,
        prepare_format_args,
        parse_format_impl,
    )
}

/// Re-inject `--write` stripped by `run_format` so the binary receives it during execution.
fn prepare_format_args(cmd_args: &mut Vec<String>) {
    if !user_has_flag(cmd_args, &["--write", "-w"]) {
        cmd_args.insert(0, "--write".to_string());
    }
}

/// Three-tier parse for `prettier --write` output.
///
/// # AD-20 (2026-04-15) — prettier --write output parsing
///
/// `prettier --write` prints one reformatted file path per line to stdout:
/// ```text
/// src/App.tsx
/// src/components/Header.tsx
/// src/utils/format.ts
/// ```
///
/// Exit 0 with empty stdout = all files already formatted → `LINT OK`.
fn parse_format_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    // Tier 1: parse written file paths from stdout
    if let Some(result) = try_parse_format_structured(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Empty stdout on exit 0 = nothing reformatted
    if output.exit_code == Some(0) && output.stdout.trim().is_empty() {
        return ParseResult::Full(LintResult::formatted("prettier".to_string(), 0));
    }

    let combined = combine_stdout_stderr(output);

    // Tier 2: regex fallback
    if let Some(result) = try_parse_format_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["prettier: format structured parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

/// Tier 1: structured parse of `prettier --write` output (one path per line).
fn try_parse_format_structured(stdout: &str) -> Option<LintResult> {
    if stdout.trim().is_empty() {
        return None;
    }

    let mut count = 0usize;

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if RE_PRETTIER_WRITTEN_PATH.is_match(trimmed) {
            count += 1;
        } else {
            // Non-path line (e.g., error message) — bail out to tier 2
            return None;
        }
    }

    if count == 0 {
        return None;
    }

    Some(LintResult::formatted("prettier".to_string(), count))
}

/// Tier 2: regex fallback for `prettier --write` output.
fn try_parse_format_regex(text: &str) -> Option<LintResult> {
    let mut count = 0usize;

    for line in text.lines() {
        let trimmed = line.trim();
        if RE_PRETTIER_WRITTEN_PATH.is_match(trimmed) {
            count += 1;
        }
    }

    if count == 0 {
        return None;
    }

    Some(LintResult::formatted("prettier".to_string(), count))
}

// ============================================================================
// Tier 1: warn-line parsing (check mode)
// ============================================================================

/// Parse prettier `--check` output by scanning `[warn]` lines.
///
/// Prettier check output format:
/// ```text
/// [warn] src/App.tsx
/// [warn] src/utils/format.ts
/// [warn] Code style issues found in the above file(s). Forgot to run Prettier?
/// ```
fn try_parse_structured(stdout: &str) -> Option<LintResult> {
    if !stdout.contains("[warn]") {
        return None;
    }

    let mut issues: Vec<LintIssue> = Vec::new();

    for line in stdout.lines() {
        // Skip the summary line
        if RE_PRETTIER_SUMMARY.is_match(line) {
            continue;
        }

        if let Some(caps) = RE_PRETTIER_WARN.captures(line) {
            issues.push(LintIssue {
                file: caps[1].to_string(),
                line: 0,
                rule: "formatting".to_string(),
                message: "Code style issues found".to_string(),
                severity: LintSeverity::Warning,
            });
        }
    }

    Some(group_issues("prettier", issues))
}

// ============================================================================
// Tier 2: regex fallback (check mode)
// ============================================================================

/// Regex fallback for other output formats.
fn try_parse_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();

    for caps in RE_PRETTIER_FILE_PATH.captures_iter(text) {
        issues.push(LintIssue {
            file: caps[1].to_string(),
            line: 0,
            rule: "formatting".to_string(),
            message: "needs formatting".to_string(),
            severity: LintSeverity::Warning,
        });
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("prettier", issues))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    use crate::cmd::lint::load_lint_fixture as load_fixture;

    // -------------------------------------------------------------------------
    // Check mode tests (existing, unchanged)
    // -------------------------------------------------------------------------

    #[test]
    fn test_tier1_prettier_pass() {
        let output = CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_check_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full result for clean pass, got {}",
            result.tier_name()
        );
        if let crate::output::ParseResult::Full(r) = result {
            assert_eq!(r.warnings, 0);
            assert!(r.as_ref().contains("LINT OK"));
        }
    }

    #[test]
    fn test_tier1_prettier_fail() {
        let input = load_fixture("prettier_check_fail.txt");
        let result = try_parse_structured(&input);
        assert!(
            result.is_some(),
            "Expected Tier 1 structured parse to succeed"
        );
        let result = result.unwrap();
        assert_eq!(result.warnings, 3);
        assert_eq!(result.errors, 0);
        assert!(result.groups.iter().any(|g| g.rule == "formatting"));
    }

    #[test]
    fn test_tier2_prettier_regex() {
        let input = "src/main.ts needs formatting\nsrc/lib.ts needs formatting\n";
        let result = try_parse_regex(input);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.warnings, 2);
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("prettier_check_fail.txt");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_check_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full parse result, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_garbage_produces_passthrough() {
        let output = CommandOutput {
            stdout: "unexpected output from prettier\nno warn lines at all".to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_check_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_text_produces_degraded() {
        // Tier 2 input: matches the `<path> needs formatting` regex but NOT the
        // `[warn]` Tier 1 format.
        let output = CommandOutput {
            stdout: "src/main.ts needs formatting\nsrc/utils/helper.js needs formatting\n"
                .to_string(),
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

    /// AD-21 (2026-04-15) — Path-aware regex patterns: [warn] lines with spaces in file paths.
    #[test]
    fn test_tier1_prettier_spaces_in_path() {
        let input = load_fixture("prettier_check_fail_spaces.txt");
        let result = try_parse_structured(&input);
        assert!(
            result.is_some(),
            "Expected Tier 1 structured parse to succeed on space-containing paths"
        );
        let result = result.unwrap();
        assert_eq!(
            result.warnings, 2,
            "Expected 2 warnings for 2 space-containing paths"
        );
        assert!(
            result.groups.iter().any(|g| g.rule == "formatting"),
            "Expected formatting rule group"
        );
        // Verify the full path with spaces was captured
        let all_locs: Vec<&str> = result
            .groups
            .iter()
            .flat_map(|g| g.locations.iter().map(|l| l.as_str()))
            .collect();
        assert!(
            all_locs.iter().any(|l| l.contains("My Component.tsx")),
            "Expected location to contain 'My Component.tsx', got: {all_locs:?}"
        );
    }

    /// AD-21 (2026-04-15) — Path-aware regex patterns: Tier 2 regex with spaces.
    #[test]
    fn test_tier2_prettier_spaces_in_path() {
        let input = "src/My Component.ts needs formatting\nsrc/My Other File.js needs formatting\n";
        let result = try_parse_regex(input);
        assert!(
            result.is_some(),
            "Expected Tier 2 regex parse on space-containing paths"
        );
        let result = result.unwrap();
        assert_eq!(result.warnings, 2);
    }

    // -------------------------------------------------------------------------
    // Format mode tests (AD-20)
    // -------------------------------------------------------------------------

    /// AD-20: is_format_mode detects --write flag.
    #[test]
    fn test_is_format_mode_write() {
        let args: Vec<String> = vec!["--write".to_string(), "src/".to_string()];
        assert!(is_format_mode(&args));
    }

    #[test]
    fn test_is_format_mode_w_short() {
        let args: Vec<String> = vec!["-w".to_string(), "src/".to_string()];
        assert!(is_format_mode(&args));
    }

    #[test]
    fn test_is_format_mode_false_for_check() {
        let args: Vec<String> = vec!["--check".to_string(), "src/".to_string()];
        assert!(!is_format_mode(&args));
    }

    /// AD-20: `prettier --write` output — 3 files reformatted.
    #[test]
    fn test_prettier_format_write_output_structured() {
        let input = load_fixture("prettier_write_output.txt");
        let result = try_parse_format_structured(&input);
        assert!(
            result.is_some(),
            "Expected structured parse on --write output"
        );
        let result = result.unwrap();
        assert_eq!(result.errors, 0);
        assert_eq!(result.warnings, 0);
        assert_eq!(result.files_formatted, Some(3));
        assert!(
            result.as_ref().contains("3 files formatted"),
            "Expected format render, got: {}",
            result.as_ref()
        );
    }

    /// AD-20: empty stdout on exit 0 = nothing reformatted.
    #[test]
    fn test_prettier_format_empty_is_pass() {
        let output = CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_format_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full for empty output, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(r) = result {
            assert!(r.as_ref().contains("LINT OK"));
        }
    }

    /// AD-20: parse_format_impl on fixture produces Full tier.
    #[test]
    fn test_parse_format_impl_fixture_is_full() {
        let input = load_fixture("prettier_write_output.txt");
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
            assert_eq!(r.files_formatted, Some(3));
        }
    }
}
