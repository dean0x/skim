//! `npm test` and `npm run` compression with package.json tool detection.
//!
//! Resolves the underlying tool used by the npm script (via `package.json`),
//! then delegates to the appropriate parser. Falls back to raw passthrough
//! when the tool cannot be identified.
//!
//! # Design note
//!
//! Each known parser returns a different `ParseResult<T>` where `T` is a
//! canonical type (e.g. `TestResult`, `LintResult`, `BuildResult`). These are
//! unified into `ParseResult<String>` via `stringify_result` before being
//! passed to `run_parsed_command_with_mode`.

use std::borrow::Cow;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::script_tool::{ScriptTool, extract_tool, resolve_script};

/// Convert a `ParseResult<T: AsRef<str>>` into a `ParseResult<String>`.
///
/// Preserves tier and degradation markers. This unification step is needed
/// because different parsers return different canonical types (`TestResult`,
/// `LintResult`, etc.) and `run_parsed_command_with_mode` requires a single
/// concrete type.
fn stringify_result<T: AsRef<str>>(result: ParseResult<T>) -> ParseResult<String> {
    match result {
        ParseResult::Full(v) => ParseResult::Full(v.as_ref().to_string()),
        ParseResult::Degraded(v, markers) => {
            ParseResult::Degraded(v.as_ref().to_string(), markers)
        }
        ParseResult::Passthrough(s) => ParseResult::Passthrough(s),
    }
}

/// Run `npm test` or `npm run <script>` with output compression.
///
/// `subcmd` is `"test"` or `"run"`. For `"run"`, `args` must start with the
/// script name. For `"test"`, the script name is implicitly `"test"`.
///
/// The function resolves the script body from `package.json`, identifies the
/// underlying tool via [`extract_tool`], and selects the matching parser.
pub(super) fn run_script(
    subcmd: &str,
    args: &[String],
    show_stats: bool,
    json_output: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    // Determine the script name.
    let script_name = if subcmd == "test" {
        "test"
    } else {
        // "run" — first arg is the script name.
        match args.first() {
            Some(name) => name.as_str(),
            None => {
                eprintln!("skim npm run: missing script name\n\nUsage: skim npm run <script>");
                return Ok(ExitCode::FAILURE);
            }
        }
    };

    // Resolve and extract tool from package.json.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let tool = resolve_script(&cwd, script_name)
        .map(|script| extract_tool(&script))
        .unwrap_or(ScriptTool::Unknown);

    // Build the npm command args: [subcmd, ...user_args]
    let mut cmd_args: Vec<String> = Vec::with_capacity(args.len() + 1);
    cmd_args.push(subcmd.to_string());
    cmd_args.extend_from_slice(args);

    let use_stdin = crate::cmd::should_read_stdin(args);

    // Determine if we should inject NO_COLOR.
    let env_overrides: &[(&str, &str)] = &[("NO_COLOR", "1")];

    crate::cmd::run_parsed_command_with_mode(
        crate::cmd::ParsedCommandConfig {
            program: "npm",
            args: &cmd_args,
            env_overrides,
            install_hint: "Install Node.js from https://nodejs.org",
            use_stdin,
            show_stats,
            output_format: if json_output {
                crate::cmd::OutputFormat::Json
            } else {
                crate::cmd::OutputFormat::Text
            },
            family: "pkg",
            skip_ansi_strip: false,
            rec,
        },
        move |output: &CommandOutput| parse_npm_output(output, tool),
    )
}

/// Select the appropriate parser based on the detected tool.
///
/// Each branch converts the parser's native `ParseResult<T>` to
/// `ParseResult<String>` via `stringify_result`.
fn parse_npm_output(output: &CommandOutput, tool: ScriptTool) -> ParseResult<String> {
    match tool {
        ScriptTool::Vitest | ScriptTool::Jest => {
            let combined = super::combine_output(output);
            let result = crate::cmd::test::vitest::parse(combined.as_ref());
            stringify_result(result)
        }
        ScriptTool::Eslint => {
            let result = crate::cmd::lint::eslint::parse_impl(output);
            stringify_result(result)
        }
        ScriptTool::Biome => {
            let result = crate::cmd::lint::biome::parse_check_impl(output);
            stringify_result(result)
        }
        ScriptTool::Prettier => {
            let result = crate::cmd::lint::prettier::parse_check_impl(output);
            stringify_result(result)
        }
        ScriptTool::Oxlint => {
            let result = crate::cmd::lint::oxlint::parse_impl(output);
            stringify_result(result)
        }
        ScriptTool::Tsc => {
            let result = crate::cmd::build::tsc::parse_tsc(output);
            stringify_result(result)
        }
        ScriptTool::Unknown => {
            // Passthrough: no recognised tool, return raw output unchanged.
            let combined: Cow<str> = super::combine_output(output);
            ParseResult::Passthrough(combined.into_owned())
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::CommandOutput;

    fn make_output(stdout: &str, stderr: &str, exit_code: i32) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code: Some(exit_code),
            duration: std::time::Duration::ZERO,
        }
    }

    #[test]
    fn test_stringify_result_full() {
        let result: ParseResult<&str> = ParseResult::Full("hello");
        let stringified = stringify_result(result);
        assert!(stringified.is_full());
        assert_eq!(stringified.content(), "hello");
    }

    #[test]
    fn test_stringify_result_degraded() {
        let result: ParseResult<&str> =
            ParseResult::Degraded("partial", vec!["marker".to_string()]);
        let stringified = stringify_result(result);
        assert!(stringified.is_degraded());
        assert_eq!(stringified.content(), "partial");
    }

    #[test]
    fn test_stringify_result_passthrough() {
        let result: ParseResult<&str> = ParseResult::Passthrough("raw".to_string());
        let stringified = stringify_result(result);
        assert!(stringified.is_passthrough());
        assert_eq!(stringified.content(), "raw");
    }

    #[test]
    fn test_parse_npm_output_unknown_passthrough() {
        let output = make_output("some output\n", "", 0);
        let result = parse_npm_output(&output, ScriptTool::Unknown);
        assert!(
            result.is_passthrough(),
            "Unknown tool should return Passthrough"
        );
        assert_eq!(result.content(), "some output\n");
    }

    #[test]
    fn test_parse_npm_output_unknown_empty_passthrough() {
        let output = make_output("", "", 0);
        let result = parse_npm_output(&output, ScriptTool::Unknown);
        assert!(result.is_passthrough());
    }
}
