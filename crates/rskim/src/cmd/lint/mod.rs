//! Lint subcommand dispatcher (#104)
//!
//! Routes `skim lint <linter> [args...]` to the appropriate linter parser.
//! Currently supported linters: `eslint`, `ruff`, `mypy`, `golangci`.

pub(crate) mod eslint;
pub(crate) mod golangci;
pub(crate) mod mypy;
pub(crate) mod ruff;

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::process::ExitCode;

use crate::output::canonical::{LintGroup, LintIssue, LintResult, LintSeverity};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

/// Known linters that `skim lint` can dispatch to.
const KNOWN_LINTERS: &[&str] = &["eslint", "ruff", "mypy", "golangci"];

/// Entry point for `skim lint <linter> [args...]`.
///
/// If no linter is specified or `--help` / `-h` is passed, prints usage
/// and exits. Otherwise dispatches to the linter-specific handler.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let (filtered_args, show_stats) = crate::cmd::extract_show_stats(args);

    // Extract --json flag
    let (filtered_args, json_output) = extract_json_flag(&filtered_args);

    let Some((linter_name, linter_args)) = filtered_args.split_first() else {
        print_help();
        return Ok(ExitCode::SUCCESS);
    };

    let linter = linter_name.as_str();

    match linter {
        "eslint" => eslint::run(linter_args, show_stats, json_output),
        "ruff" => ruff::run(linter_args, show_stats, json_output),
        "mypy" => mypy::run(linter_args, show_stats, json_output),
        "golangci" => golangci::run(linter_args, show_stats, json_output),
        _ => {
            eprintln!(
                "skim lint: unknown linter '{linter}'\n\
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
    println!("  skim lint ruff check .         Run ruff check");
    println!("  skim lint mypy src/            Run mypy");
    println!("  skim lint golangci run ./...   Run golangci-lint");
    println!("  eslint . 2>&1 | skim lint eslint  Pipe eslint output");
}

/// Extract `--json` flag from args, returning (filtered_args, json_output).
fn extract_json_flag(args: &[String]) -> (Vec<String>, bool) {
    let json_output = args.iter().any(|a| a == "--json");
    let filtered: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--json")
        .cloned()
        .collect();
    (filtered, json_output)
}

// ============================================================================
// Shared JSON mode helper
// ============================================================================

/// Configuration for [`run_lint_json_mode`].
pub(crate) struct LintJsonConfig<'a> {
    /// Binary name of the linter (e.g., "eslint", "ruff").
    pub program: &'a str,
    /// Arguments to pass to the linter.
    pub cmd_args: &'a [String],
    /// Environment variable overrides for the child process.
    pub env_overrides: &'a [(&'a str, &'a str)],
    /// Hint printed when the linter binary is not found.
    pub install_hint: &'a str,
    /// Whether stdin is piped (read stdin instead of running command).
    pub use_stdin: bool,
    /// Whether to report token statistics to stderr.
    pub show_stats: bool,
}

/// Run a linter in `--json` mode: execute (or read stdin), parse output,
/// serialize result as JSON to stdout, and preserve the child process exit code.
///
/// This is the single implementation shared by all lint parsers, eliminating
/// the per-linter `run_json_mode` duplication. The caller supplies a
/// `parse_fn` that implements the linter-specific three-tier parse logic.
pub(crate) fn run_lint_json_mode(
    config: LintJsonConfig<'_>,
    parse_fn: impl FnOnce(&CommandOutput) -> ParseResult<LintResult>,
) -> anyhow::Result<ExitCode> {
    /// Maximum bytes we will read from stdin (64 MiB), consistent with the
    /// runner's `MAX_OUTPUT_BYTES` limit for command output pipes.
    const MAX_STDIN_BYTES: u64 = 64 * 1024 * 1024;

    let LintJsonConfig {
        program,
        cmd_args,
        env_overrides,
        install_hint,
        use_stdin,
        show_stats,
    } = config;

    let output = if use_stdin {
        let mut stdin_buf = String::new();
        let bytes_read = io::stdin()
            .take(MAX_STDIN_BYTES)
            .read_to_string(&mut stdin_buf)?;
        if bytes_read as u64 >= MAX_STDIN_BYTES {
            anyhow::bail!("stdin input exceeded 64 MiB limit");
        }
        CommandOutput {
            stdout: crate::output::strip_ansi(&stdin_buf),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        }
    } else {
        let runner = crate::runner::CommandRunner::new(Some(std::time::Duration::from_secs(300)));
        let args_str: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
        match runner.run_with_env(program, &args_str, env_overrides) {
            Ok(out) => CommandOutput {
                stdout: crate::output::strip_ansi(&out.stdout),
                stderr: crate::output::strip_ansi(&out.stderr),
                ..out
            },
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("failed to execute") {
                    eprintln!("error: '{program}' not found");
                    eprintln!("hint: {install_hint}");
                    return Ok(ExitCode::FAILURE);
                }
                return Err(e);
            }
        }
    };

    let result = parse_fn(&output);
    let json_str = match &result {
        ParseResult::Full(lint_result) => serde_json::to_string(lint_result)?,
        ParseResult::Degraded(lint_result, warnings) => {
            let val = serde_json::json!({
                "tier": "degraded",
                "warnings": warnings,
                "result": lint_result,
            });
            serde_json::to_string(&val)?
        }
        ParseResult::Passthrough(raw) => {
            let val = serde_json::json!({
                "tier": "passthrough",
                "raw": raw,
            });
            serde_json::to_string(&val)?
        }
    };

    let stdout = io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{json_str}")?;
    handle.flush()?;

    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(&output.stdout, &json_str);
        crate::process::report_token_stats(orig, comp, "");
    }

    // Capture exit code before moving stdout into analytics
    let code = output.exit_code.unwrap_or(1);

    if crate::analytics::is_analytics_enabled() {
        crate::analytics::try_record_command(
            output.stdout,
            json_str,
            format!("skim lint {program} {}", cmd_args.join(" ")),
            crate::analytics::CommandType::Lint,
            output.duration,
            Some(result.tier_name()),
        );
    }

    // Preserve child process exit code
    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}

/// Group individual lint issues by rule into a `LintResult`.
///
/// Uses `BTreeMap` for deterministic ordering of rule groups.
pub(crate) fn group_issues(tool: &str, issues: Vec<LintIssue>) -> LintResult {
    let mut groups: BTreeMap<String, LintGroup> = BTreeMap::new();
    let mut errors = 0usize;
    let mut warnings = 0usize;
    for issue in &issues {
        match issue.severity {
            LintSeverity::Error => errors += 1,
            LintSeverity::Warning => warnings += 1,
            LintSeverity::Info => {}
        }
        let group = groups
            .entry(issue.rule.clone())
            .or_insert_with(|| LintGroup {
                rule: issue.rule.clone(),
                count: 0,
                severity: issue.severity.clone(),
                locations: Vec::new(),
            });
        group.count += 1;
        group
            .locations
            .push(format!("{}:{}", issue.file, issue.line));
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
    fn test_extract_json_flag_present() {
        let args = vec!["--json".to_string(), "eslint".to_string(), ".".to_string()];
        let (filtered, json_output) = extract_json_flag(&args);
        assert!(json_output);
        assert_eq!(filtered, vec!["eslint".to_string(), ".".to_string()]);
    }

    #[test]
    fn test_extract_json_flag_absent() {
        let args = vec!["eslint".to_string(), ".".to_string()];
        let (filtered, json_output) = extract_json_flag(&args);
        assert!(!json_output);
        assert_eq!(filtered, vec!["eslint".to_string(), ".".to_string()]);
    }
}
