//! mypy parser with three-tier degradation (#104).
//!
//! Executes `mypy` and parses the output into a structured `LintResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: NDJSON parsing (`--output json`)
//! - **Tier 2 (Degraded)**: Regex on default text output
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation

use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::{run_parsed_command_with_mode, user_has_flag, ParsedCommandConfig};
use crate::output::canonical::{LintIssue, LintResult, LintSeverity};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::group_issues;

// Static regex pattern compiled once via LazyLock.
static RE_MYPY_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+):(\d+):\s+(error|warning|note):\s+(.+?)(?:\s+\[(\S+)\])?$").unwrap()
});

/// Run `skim lint mypy [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<ExitCode> {
    let mut cmd_args: Vec<String> = args.to_vec();

    // Inject --output json if not already present
    if !user_has_flag(&cmd_args, &["--output"]) {
        cmd_args.insert(0, "json".to_string());
        cmd_args.insert(0, "--output".to_string());
    }

    let use_stdin = !std::io::stdin().is_terminal() && args.is_empty();

    if json_output {
        return run_json_mode(&cmd_args, use_stdin, show_stats);
    }

    run_parsed_command_with_mode(
        ParsedCommandConfig {
            program: "mypy",
            args: &cmd_args,
            env_overrides: &[("NO_COLOR", "1"), ("MYPY_FORCE_COLOR", "0")],
            install_hint: "Install mypy: pip install mypy",
            use_stdin,
            show_stats,
            command_type: crate::analytics::CommandType::Lint,
        },
        |output, _args| parse_impl(output),
    )
}

/// Run in `--json` mode.
fn run_json_mode(
    cmd_args: &[String],
    use_stdin: bool,
    show_stats: bool,
) -> anyhow::Result<ExitCode> {
    use std::io::{self, Read, Write};

    let output = if use_stdin {
        let mut stdin_buf = String::new();
        io::stdin().read_to_string(&mut stdin_buf)?;
        CommandOutput {
            stdout: crate::output::strip_ansi(&stdin_buf),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        }
    } else {
        let runner = crate::runner::CommandRunner::new(Some(std::time::Duration::from_secs(300)));
        let args_str: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
        match runner.run_with_env(
            "mypy",
            &args_str,
            &[("NO_COLOR", "1"), ("MYPY_FORCE_COLOR", "0")],
        ) {
            Ok(out) => CommandOutput {
                stdout: crate::output::strip_ansi(&out.stdout),
                stderr: crate::output::strip_ansi(&out.stderr),
                ..out
            },
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("failed to execute") {
                    eprintln!("error: 'mypy' not found");
                    eprintln!("hint: Install mypy: pip install mypy");
                    return Ok(ExitCode::FAILURE);
                }
                return Err(e);
            }
        }
    };

    let result = parse_impl(&output);
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

    if crate::analytics::is_analytics_enabled() {
        crate::analytics::try_record_command(
            output.stdout,
            json_str,
            format!("skim lint mypy {}", cmd_args.join(" ")),
            crate::analytics::CommandType::Lint,
            output.duration,
            Some(result.tier_name()),
        );
    }

    Ok(ExitCode::SUCCESS)
}

/// Three-tier parse function for mypy output.
fn parse_impl(output: &CommandOutput) -> ParseResult<LintResult> {
    // Tier 1: NDJSON parsing
    if let Some(result) = try_parse_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Tier 2: regex fallback
    let combined = if output.stderr.is_empty() {
        output.stdout.clone()
    } else {
        format!("{}\n{}", output.stdout, output.stderr)
    };

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(result, vec!["regex fallback".to_string()]);
    }

    // Tier 3: passthrough
    ParseResult::Passthrough(combined)
}

// ============================================================================
// Tier 1: NDJSON parsing
// ============================================================================

/// Parse mypy NDJSON output format.
///
/// mypy `--output json` produces one JSON object per line (NDJSON):
/// ```json
/// {"file": "...", "line": 10, "column": 5, "message": "...", "code": "...", "severity": "error"}
/// ```
fn try_parse_json(stdout: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();
    let mut any_parsed = false;

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };

        // Must have at least "file" and "line" fields to be a valid mypy JSON entry
        let Some(file) = value.get("file").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(line_num) = value.get("line").and_then(|v| v.as_u64()) else {
            continue;
        };

        any_parsed = true;

        let severity_str = value
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        let severity = match severity_str {
            "error" => LintSeverity::Error,
            "warning" => LintSeverity::Warning,
            "note" => LintSeverity::Info,
            _ => LintSeverity::Error,
        };

        let code = value
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let message = value.get("message").and_then(|v| v.as_str()).unwrap_or("");

        issues.push(LintIssue {
            file: file.to_string(),
            line: line_num as u32,
            rule: code.to_string(),
            message: message.to_string(),
            severity,
        });
    }

    if !any_parsed {
        return None;
    }

    Some(group_issues("mypy", issues))
}

// ============================================================================
// Tier 2: regex fallback
// ============================================================================

/// Parse mypy default text output via regex.
///
/// Format: `file:line: error: message [code]`
fn try_parse_regex(text: &str) -> Option<LintResult> {
    let mut issues: Vec<LintIssue> = Vec::new();

    for line in text.lines() {
        if let Some(caps) = RE_MYPY_LINE.captures(line) {
            let file = caps[1].to_string();
            let line_num: u32 = caps[2].parse().unwrap_or(0);
            let severity = match &caps[3] {
                "error" => LintSeverity::Error,
                "warning" => LintSeverity::Warning,
                "note" => LintSeverity::Info,
                _ => LintSeverity::Error,
            };
            let message = caps[4].to_string();
            let code = caps
                .get(5)
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| "(unknown)".to_string());

            issues.push(LintIssue {
                file,
                line: line_num,
                rule: code,
                message,
                severity,
            });
        }
    }

    if issues.is_empty() {
        return None;
    }

    Some(group_issues("mypy", issues))
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
    fn test_tier1_mypy_fail() {
        let input = load_fixture("mypy_fail.json");
        let result = try_parse_json(&input);
        assert!(result.is_some(), "Expected Tier 1 NDJSON parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.errors, 3);
        assert_eq!(result.warnings, 0);
    }

    #[test]
    fn test_tier2_mypy_regex() {
        let input = load_fixture("mypy_text.txt");
        let result = try_parse_regex(&input);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.errors, 3);
    }

    #[test]
    fn test_parse_impl_json_produces_full() {
        let input = load_fixture("mypy_fail.json");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(result.is_full());
    }

    #[test]
    fn test_parse_impl_garbage_produces_passthrough() {
        let output = CommandOutput {
            stdout: "random garbage".to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(result.is_passthrough());
    }
}
