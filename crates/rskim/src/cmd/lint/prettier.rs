//! Prettier parser with three-tier degradation (#116).
//!
//! Executes `prettier` and parses check output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Parse `[warn]` lines as file paths
//! - **Tier 2 (Degraded)**: Regex fallback on other output formats
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation

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

static RE_PRETTIER_WARN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[warn\]\s+(\S+)").unwrap());

static RE_PRETTIER_SUMMARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[warn\]\s+Code style issues found").unwrap());

static RE_PRETTIER_FILE_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^([^\s]+\.[a-zA-Z]{1,6})\s+needs? formatting").unwrap());

/// Run `skim lint prettier [args...]`.
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

/// Inject `--check` if not already present.
fn prepare_args(cmd_args: &mut Vec<String>) {
    if !user_has_flag(cmd_args, &["--check", "-c", "--list-different", "-l"]) {
        cmd_args.insert(0, "--check".to_string());
    }
}

/// Three-tier parse function for prettier output.
fn parse_impl(output: &CommandOutput) -> ParseResult<LintResult> {
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
// Tier 1: warn-line parsing
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
// Tier 2: regex fallback
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

    fn load_fixture(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/cmd/lint");
        path.push(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    #[test]
    fn test_tier1_prettier_pass() {
        let output = CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
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
        let result = parse_impl(&output);
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
        let result = parse_impl(&output);
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
        let result = parse_impl(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded parse result, got {}",
            result.tier_name()
        );
    }
}
