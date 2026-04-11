//! Rustfmt parser with three-tier degradation (#116).
//!
//! Executes `rustfmt` and parses check output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Parse `Diff in <path> at line <N>:` headers
//! - **Tier 2 (Degraded)**: Regex on unified diff `--- <path>` headers
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{LintIssue, LintResult, LintSeverity};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, group_issues, LinterConfig};

const CONFIG: LinterConfig<'static> = LinterConfig {
    program: "rustfmt",
    env_overrides: &[],
    install_hint: "rustup component add rustfmt",
};

static RE_RUSTFMT_DIFF_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Diff in (.+) at line (\d+):").unwrap());

static RE_RUSTFMT_UNIFIED_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^--- (.+)").unwrap());

/// Run `skim lint rustfmt [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<std::process::ExitCode> {
    super::run_linter(
        CONFIG,
        args,
        show_stats,
        json_output,
        prepare_args,
        parse_impl,
    )
}

/// Inject `--check` if not already present.
fn prepare_args(cmd_args: &mut Vec<String>) {
    if !user_has_flag(cmd_args, &["--check", "-c"]) {
        cmd_args.insert(0, "--check".to_string());
    }
}

/// Three-tier parse function for rustfmt output.
fn parse_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_structured(&combined) {
        return ParseResult::Full(result);
    }

    // Empty output on exit 0 = all files formatted correctly
    if output.exit_code == Some(0) && combined.trim().is_empty() {
        return ParseResult::Full(group_issues("rustfmt", vec![]));
    }

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["rustfmt: diff parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// Tier 1: diff-header parsing
// ============================================================================

/// Parse rustfmt `--check` output by scanning `Diff in <path> at line <N>:` headers.
///
/// Rustfmt check output format:
/// ```text
/// Diff in /path/to/src/main.rs at line 15:
///  fn main() {
/// -    let x=1;
/// +    let x = 1;
///  }
/// ```
fn try_parse_structured(text: &str) -> Option<LintResult> {
    if !text.contains("Diff in ") {
        return None;
    }

    let mut issues: Vec<LintIssue> = Vec::new();

    for line in text.lines() {
        if let Some(caps) = RE_RUSTFMT_DIFF_HEADER.captures(line) {
            let file_path = caps[1].trim().to_string();
            let line_num: u32 = caps[2].parse().unwrap_or(0);
            issues.push(LintIssue {
                file: file_path,
                line: line_num,
                rule: "formatting".to_string(),
                // Include line number in message so agents can navigate directly
                // to the first formatting difference without re-running rustfmt.
                message: format!("formatting difference at line {line_num}"),
                severity: LintSeverity::Warning,
            });
        }
    }

    Some(group_issues("rustfmt", issues))
}

// ============================================================================
// Tier 2: unified diff header fallback
// ============================================================================

/// Parse unified diff `--- <path>` headers as a fallback.
fn try_parse_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    for line in text.lines() {
        if let Some(caps) = RE_RUSTFMT_UNIFIED_HEADER.captures(line) {
            let raw_path = caps[1].trim();
            // Skip "a/..." or "b/..." git diff prefixes, and /dev/null
            let path = raw_path.trim_start_matches("a/").trim_start_matches("b/");
            if path == "/dev/null" || path.is_empty() {
                continue;
            }
            if seen_paths.insert(path.to_string()) {
                issues.push(LintIssue {
                    file: path.to_string(),
                    line: 0,
                    rule: "formatting".to_string(),
                    message: "formatting difference detected".to_string(),
                    severity: LintSeverity::Warning,
                });
            }
        }
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("rustfmt", issues))
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
    fn test_tier1_rustfmt_pass() {
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
    fn test_tier1_rustfmt_fail() {
        let input = load_fixture("rustfmt_check_fail.txt");
        let result = try_parse_structured(&input);
        assert!(
            result.is_some(),
            "Expected Tier 1 structured parse to succeed"
        );
        let result = result.unwrap();
        assert_eq!(result.warnings, 2);
        assert_eq!(result.errors, 0);
        assert!(result.groups.iter().any(|g| g.rule == "formatting"));
    }

    /// Tier 1 locations must include the exact line number from the diff header.
    ///
    /// `group_issues` formats locations as `"file:line"` — both file path and
    /// line number must be present so agents can navigate directly to the diff
    /// without re-running rustfmt.
    #[test]
    fn test_tier1_rustfmt_location_includes_file_and_line() {
        let input = "Diff in /path/to/src/main.rs at line 15:\n-old\n+new\n";
        let result = try_parse_structured(input).expect("must parse");
        let group = &result.groups[0];
        assert_eq!(
            group.locations.len(),
            1,
            "Must have exactly one location: {:?}",
            group.locations
        );
        let loc = &group.locations[0];
        // Location is formatted as "file:line" by group_issues.
        assert!(
            loc.contains("main.rs"),
            "Location must contain file name: {loc}"
        );
        assert!(
            loc.contains("15"),
            "Location must contain line number: {loc}"
        );
    }

    #[test]
    fn test_tier2_rustfmt_regex() {
        let input = "--- src/main.rs\n+++ src/main.rs\n-old line\n+new line\n";
        let result = try_parse_regex(input);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.warnings, 1);
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("rustfmt_check_fail.txt");
        let output = CommandOutput {
            stdout: String::new(),
            stderr: input,
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
            stdout: "unexpected output\nno diff headers here".to_string(),
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
        // Tier 2 input: unified diff headers (`--- <path>`) that pass Tier 2
        // but NOT Tier 1 (`Diff in <path> at line <N>:`).
        let output = CommandOutput {
            stdout: "--- src/main.rs\n+++ src/main.rs\n-old line\n+new line\n".to_string(),
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
