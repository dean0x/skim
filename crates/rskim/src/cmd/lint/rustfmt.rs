//! Rustfmt parser with three-tier degradation (#116).
//!
//! Executes `rustfmt` and parses check output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Parse `Diff in <path> at line <N>:` headers
//! - **Tier 2 (Degraded)**: Regex on unified diff `--- <path>` headers
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation
//!
//! # AD-20 (2026-04-15) — check/format split for rustfmt
//! # AD-26 (2026-04-15) — safe default: check mode unless --format/-f is explicit
//!
//! `rustfmt --check` (or `cargo fmt --check`) produces diff output for files
//! that need reformatting (existing behaviour, `run_check`).
//!
//! Bare `rustfmt` / `cargo fmt` rewrites files and produces minimal or no output
//! on success. These are handled by `run_format`, which is active ONLY when the
//! user passes `--format` or `-f`. All other invocations (including bare args with
//! no flags) default to check mode to prevent accidental file rewrites.

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

/// Returns true when the user explicitly passed `--format` or `-f` to indicate
/// format (apply) mode.
///
/// # AD-26 (2026-04-15) — safe default for rustfmt dispatch
///
/// The old implementation (`!user_has_flag(args, &["--check"])`) treated bare
/// invocation (no args) as format mode, which causes rustfmt to rewrite files in
/// place — a destructive default.  Format mode must be explicit opt-in, not the
/// fallback.
///
/// Design: require `--format` or `-f` flag for apply mode. `--check` remains
/// optional in check mode (it is injected automatically by `prepare_check_args`
/// when absent).  This matches the positive-signal pattern used by prettier
/// (`--write`) and ruff (`format` subcommand), and the non-empty-args guard used
/// by black.
fn is_format_mode(args: &[String]) -> bool {
    user_has_flag(args, &["--format", "-f"])
}

/// Run `skim lint rustfmt [args...]`.
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

/// Inject `--check` if not already present.
fn prepare_check_args(cmd_args: &mut Vec<String>) {
    if !user_has_flag(cmd_args, &["--check", "-c"]) {
        cmd_args.insert(0, "--check".to_string());
    }
}

/// Three-tier parse function for rustfmt check output.
fn parse_check_impl(output: &CommandOutput) -> ParseResult<LintResult> {
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
// Format mode (AD-20)
// ============================================================================

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

/// Pass args through unchanged for format mode — no `--check` injection.
fn prepare_format_args(_cmd_args: &mut Vec<String>) {}

/// Parse for `rustfmt` (format/apply mode) output.
///
/// # AD-20 (2026-04-15) — rustfmt format mode
///
/// Bare `rustfmt` / `cargo fmt` rewrites files silently on success. Output is
/// typically empty on success. On error (e.g., parse error), rustfmt emits
/// diagnostics on stderr.
///
/// - Exit 0 + empty/minimal output → `LINT OK | rustfmt (0 files formatted)`
/// - Any stderr content → treat as passthrough (unexpected error)
fn parse_format_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    let combined = combine_stdout_stderr(output);

    // Exit 0 = successful format run (rustfmt may emit minor notices on success,
    // but we treat any exit-0 output as formatted).
    if output.exit_code == Some(0) {
        return ParseResult::Full(LintResult::formatted("rustfmt".to_string(), 0));
    }

    // Non-zero exit = error (e.g., syntax error in source) → passthrough
    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// Tier 1: diff-header parsing (check mode)
// ============================================================================

/// Parse rustfmt `--check` output by scanning `Diff in <path> at line <N>:` headers.
///
/// # AD-17 (2026-04-11) — line-number inclusion in rustfmt location messages
///
/// The Tier-1 parse extracts the exact line number from the `Diff in <path> at
/// line <N>:` header and embeds it in the message string as
/// `"formatting difference at line N"` rather than the generic
/// `"formatting difference detected"` used by Tier-2.  This lets agents navigate
/// directly to the affected line without re-running rustfmt.
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
// Tier 2: unified diff header fallback (check mode)
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

    use crate::cmd::lint::load_lint_fixture as load_fixture;

    // -------------------------------------------------------------------------
    // Check mode tests (existing, unchanged)
    // -------------------------------------------------------------------------

    #[test]
    fn test_tier1_rustfmt_pass() {
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
            stdout: "unexpected output\nno diff headers here".to_string(),
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
        // Tier 2 input: unified diff headers (`--- <path>`) that pass Tier 2
        // but NOT Tier 1 (`Diff in <path> at line <N>:`).
        let output = CommandOutput {
            stdout: "--- src/main.rs\n+++ src/main.rs\n-old line\n+new line\n".to_string(),
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

    // -------------------------------------------------------------------------
    // Format mode tests (AD-20, AD-26)
    // -------------------------------------------------------------------------

    /// AD-26: bare invocation (no args) must default to check mode, not format.
    /// Bare `skim lint rustfmt` must never rewrite files without an explicit flag.
    #[test]
    fn test_is_format_mode_bare_args_is_false() {
        let args: Vec<String> = vec![];
        assert!(
            !is_format_mode(&args),
            "No args (bare rustfmt) must be check mode — safe default"
        );
    }

    /// AD-26: file-only args (no format flag) must also default to check mode.
    #[test]
    fn test_is_format_mode_with_files_only_is_false() {
        let args: Vec<String> = vec!["src/main.rs".to_string()];
        assert!(
            !is_format_mode(&args),
            "File-only args without --format/-f must be check mode"
        );
    }

    /// AD-26: --check flag keeps check mode.
    #[test]
    fn test_is_format_mode_false_when_check_present() {
        let args: Vec<String> = vec!["--check".to_string(), "src/main.rs".to_string()];
        assert!(!is_format_mode(&args));
    }

    /// AD-26: --format flag is the explicit opt-in for format mode.
    #[test]
    fn test_is_format_mode_true_with_format_flag() {
        let args: Vec<String> = vec!["--format".to_string(), "src/main.rs".to_string()];
        assert!(
            is_format_mode(&args),
            "--format flag must activate format mode"
        );
    }

    /// AD-26: -f short flag is the explicit opt-in for format mode.
    #[test]
    fn test_is_format_mode_true_with_short_flag() {
        let args: Vec<String> = vec!["-f".to_string(), "src/main.rs".to_string()];
        assert!(
            is_format_mode(&args),
            "-f short flag must activate format mode"
        );
    }

    /// AD-20: empty output on exit 0 = successful format run.
    #[test]
    fn test_rustfmt_format_empty_output_is_pass() {
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
            assert!(
                r.as_ref().contains("files formatted"),
                "Expected format-mode render, got: {}",
                r.as_ref()
            );
            assert_eq!(r.files_formatted, Some(0));
        }
    }

    /// AD-20: non-zero exit = syntax error → passthrough.
    #[test]
    fn test_rustfmt_format_error_is_passthrough() {
        let output = CommandOutput {
            stdout: String::new(),
            stderr: "error[E0001]: unexpected token `}` in format string\n --> src/main.rs:5:1"
                .to_string(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_format_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for error exit, got {}",
            result.tier_name()
        );
    }
}
