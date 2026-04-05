//! Lint subcommand dispatcher (#104, #116)
//!
//! Routes `skim lint <linter> [args...]` to the appropriate linter parser.
//! Currently supported linters: `eslint`, `golangci`, `mypy`, `prettier`, `ruff`, `rustfmt`.

pub(crate) mod eslint;
pub(crate) mod golangci;
pub(crate) mod mypy;
pub(crate) mod prettier;
pub(crate) mod ruff;
pub(crate) mod rustfmt;

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::process::ExitCode;

use super::{extract_show_stats, run_parsed_command_with_mode, OutputFormat, ParsedCommandConfig};
use crate::output::canonical::{LintGroup, LintIssue, LintResult, LintSeverity};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

/// Known linters that `skim lint` can dispatch to.
const KNOWN_LINTERS: &[&str] = &["eslint", "golangci", "mypy", "prettier", "ruff", "rustfmt"];

/// Entry point for `skim lint <linter> [args...]`.
///
/// If no linter is specified or `--help` / `-h` is passed, prints usage
/// and exits. Otherwise dispatches to the linter-specific handler.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let (filtered_args, show_stats) = extract_show_stats(args);

    // Extract --json flag
    let (filtered_args, json_output) = super::extract_json_flag(&filtered_args);

    let Some((linter_name, linter_args)) = filtered_args.split_first() else {
        print_help();
        return Ok(ExitCode::SUCCESS);
    };

    match linter_name.as_str() {
        "eslint" => eslint::run(linter_args, show_stats, json_output),
        "golangci" => golangci::run(linter_args, show_stats, json_output),
        "mypy" => mypy::run(linter_args, show_stats, json_output),
        "prettier" => prettier::run(linter_args, show_stats, json_output),
        "ruff" => ruff::run(linter_args, show_stats, json_output),
        "rustfmt" => rustfmt::run(linter_args, show_stats, json_output),
        linter => {
            let safe_linter = crate::cmd::infra::sanitize_for_display(linter);
            eprintln!(
                "skim lint: unknown linter '{safe_linter}'\n\
                 Available linters: {}\n\
                 Run 'skim lint --help' for usage information",
                KNOWN_LINTERS.join(", ")
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("skim lint <linter> [args...]");
    println!();
    println!("  Run linters and parse the output for AI context windows.");
    println!();
    println!("Available linters:");
    for linter in KNOWN_LINTERS {
        println!("  {linter}");
    }
    println!();
    println!("Flags:");
    println!("  --json          Emit structured JSON output");
    println!("  --show-stats    Show token statistics");
    println!();
    println!("Examples:");
    println!("  skim lint eslint .             Run eslint");
    println!("  skim lint golangci run ./...   Run golangci-lint");
    println!("  skim lint mypy src/            Run mypy");
    println!("  skim lint prettier .           Run prettier --check");
    println!("  skim lint ruff check .         Run ruff check");
    println!("  skim lint rustfmt src/         Run rustfmt --check");
    println!("  eslint . 2>&1 | skim lint eslint  Pipe eslint output");
}

// ============================================================================
// Shared linter execution helper
// ============================================================================

/// Static configuration for a linter binary.
///
/// Each linter module exposes a `CONFIG` constant with this type.
pub(crate) struct LinterConfig<'a> {
    /// Binary name of the linter (e.g., "eslint", "ruff").
    pub program: &'a str,
    /// Environment variable overrides for the child process.
    pub env_overrides: &'a [(&'a str, &'a str)],
    /// Hint printed when the linter binary is not found.
    pub install_hint: &'a str,
}

/// Execute a linter, parse its output, and emit the result.
///
/// This is the single implementation shared by all lint parsers, handling both
/// text and JSON output modes. It eliminates per-linter `run()` boilerplate by
/// delegating to [`super::run_parsed_command_with_mode`].
///
/// - `config`: static linter metadata (program name, env vars, install hint)
/// - `args`: raw user args (before prepare_args)
/// - `show_stats`: whether to report token statistics
/// - `json_output`: whether to emit JSON instead of text
/// - `prepare_args`: closure to inject linter-specific flags (e.g., `--format json`)
/// - `parse_fn`: linter-specific three-tier parse function
pub(crate) fn run_linter(
    config: LinterConfig<'_>,
    args: &[String],
    show_stats: bool,
    json_output: bool,
    prepare_args: impl FnOnce(&mut Vec<String>),
    parse_fn: impl FnOnce(&CommandOutput) -> ParseResult<LintResult>,
) -> anyhow::Result<ExitCode> {
    let mut cmd_args = args.to_vec();
    prepare_args(&mut cmd_args);

    let use_stdin = !std::io::stdin().is_terminal() && args.is_empty();
    let output_format = if json_output {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    };

    run_parsed_command_with_mode(
        ParsedCommandConfig {
            program: config.program,
            args: &cmd_args,
            env_overrides: config.env_overrides,
            install_hint: config.install_hint,
            use_stdin,
            show_stats,
            command_type: crate::analytics::CommandType::Lint,
            output_format,
        },
        |output, _args| parse_fn(output),
    )
}

/// Re-export the shared `combine_output` under the name callers expect.
pub(crate) use super::combine_output as combine_stdout_stderr;

/// Group individual lint issues by rule into a `LintResult`.
///
/// Uses `BTreeMap` for deterministic ordering of rule groups.
pub(crate) fn group_issues(tool: &str, issues: Vec<LintIssue>) -> LintResult {
    let mut groups: BTreeMap<String, LintGroup> = BTreeMap::new();
    let mut errors = 0usize;
    let mut warnings = 0usize;
    for issue in issues {
        match issue.severity {
            LintSeverity::Error => errors += 1,
            LintSeverity::Warning => warnings += 1,
            LintSeverity::Info => {}
        }
        let location = format!("{}:{}", issue.file, issue.line);
        let group = groups
            .entry(issue.rule.clone())
            .or_insert_with(|| LintGroup {
                rule: issue.rule,
                count: 0,
                severity: issue.severity,
                locations: Vec::new(),
            });
        group.count += 1;
        group.locations.push(location);
    }
    LintResult::new(
        tool.to_string(),
        errors,
        warnings,
        groups.into_values().collect(),
    )
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use crate::output::canonical::{LintIssue, LintSeverity};

    #[test]
    fn test_group_issues_info_severity_not_counted() {
        let issues = vec![
            LintIssue {
                file: "a.ts".to_string(),
                line: 1,
                rule: "info-rule".to_string(),
                message: "informational".to_string(),
                severity: LintSeverity::Info,
            },
            LintIssue {
                file: "a.ts".to_string(),
                line: 2,
                rule: "err-rule".to_string(),
                message: "real error".to_string(),
                severity: LintSeverity::Error,
            },
        ];
        let result = group_issues("test", issues);
        assert_eq!(result.errors, 1);
        assert_eq!(result.warnings, 0);
        // Info issue is grouped but not counted as error or warning
        assert_eq!(result.groups.len(), 2);
    }

    #[test]
    fn test_group_issues_empty() {
        let result = group_issues("test", vec![]);
        assert_eq!(result.errors, 0);
        assert_eq!(result.warnings, 0);
        assert!(result.groups.is_empty());
        assert!(result.as_ref().contains("LINT OK"));
    }

    #[test]
    fn test_combine_stdout_stderr_empty_stderr() {
        let output = CommandOutput {
            stdout: "hello world".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let combined = combine_stdout_stderr(&output);
        assert_eq!(&*combined, "hello world");
        // When stderr is empty, should borrow (Cow::Borrowed)
        assert!(matches!(combined, Cow::Borrowed(_)));
    }

    #[test]
    fn test_combine_stdout_stderr_with_stderr() {
        let output = CommandOutput {
            stdout: "out".to_string(),
            stderr: "err".to_string(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let combined = combine_stdout_stderr(&output);
        assert_eq!(&*combined, "out\nerr");
        // When stderr is non-empty, should own (Cow::Owned)
        assert!(matches!(combined, Cow::Owned(_)));
    }

    #[test]
    fn test_combine_stdout_stderr_both_empty() {
        let output = CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let combined = combine_stdout_stderr(&output);
        assert_eq!(&*combined, "");
        assert!(matches!(combined, Cow::Borrowed(_)));
    }
}
