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
        ParseResult::Degraded(v, markers) => ParseResult::Degraded(v.as_ref().to_string(), markers),
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
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        if crate::debug::is_debug_enabled() {
            eprintln!("skim: npm run: current_dir() failed ({e}), using '.' as cwd");
        }
        PathBuf::from(".")
    });
    let tool = resolve_script(&cwd, script_name)
        .map(|script| extract_tool(&script))
        .unwrap_or(ScriptTool::Unknown);

    // Build the npm command args: [subcmd, ...user_args]
    let mut cmd_args: Vec<String> = Vec::with_capacity(args.len() + 1);
    cmd_args.push(subcmd.to_string());
    cmd_args.extend_from_slice(args);

    let use_stdin = crate::cmd::should_read_stdin(args);
    let env_overrides: &[(&str, &str)] = &[("NO_COLOR", "1")];

    // Cannot use run_pkg_subcommand here: (1) json_output needs conditional
    // OutputFormat (Json vs Text), and (2) the parse function is dynamically
    // selected by the detected tool rather than fixed at compile time.
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
///
/// # Design: direct function imports vs trait dispatch
///
/// This function calls six parser functions by direct import (`parse_impl`,
/// `parse_check_impl`, `parse_tsc`, `parse`). Those functions are `pub(crate)`
/// rather than exposed through a trait registry.
///
/// This is intentional: `rskim` is a single-binary crate, so the "blast radius"
/// of these cross-module imports is bounded at the crate boundary. A trait-based
/// registry (e.g. `ToolParser` in `cmd/mod.rs`) would add an indirection layer
/// without changing visibility or safety properties — `pub(crate)` already
/// enforces internal-only access. The set of supported tools is closed and
/// enumerated by `ScriptTool`; adding a new tool requires touching this match
/// regardless of whether a registry exists, so the trait would not reduce
/// coupling in practice.
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
            ParseResult::Passthrough(super::combine_output(output).into_owned())
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_support::{load_fixture, make_output_full};

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
        let output = make_output_full("some output\n", "", Some(0));
        let result = parse_npm_output(&output, ScriptTool::Unknown);
        assert!(
            result.is_passthrough(),
            "Unknown tool should return Passthrough"
        );
        assert_eq!(result.content(), "some output\n");
    }

    #[test]
    fn test_parse_npm_output_unknown_empty_passthrough() {
        let output = make_output_full("", "", Some(0));
        let result = parse_npm_output(&output, ScriptTool::Unknown);
        assert!(result.is_passthrough());
    }

    /// When stdout is empty and stderr is non-empty, `combine_output` produces
    /// `"\n<stderr>"` (a leading newline before the stderr content).
    ///
    /// This is intentional pre-existing behaviour of `combine_output`: the fast
    /// path (empty stderr) borrows stdout directly; the slow path concatenates
    /// `"{stdout}\n{stderr}"`, which produces a leading newline when stdout is
    /// empty.  Callers that need a clean string (e.g. `parse_fmt`) trim
    /// explicitly.  The Unknown passthrough path does NOT trim — it preserves
    /// the raw combined output so nothing is silently dropped.
    #[test]
    fn test_parse_npm_output_unknown_stderr_only_has_leading_newline() {
        let output = make_output_full("", "error: something went wrong\n", Some(1));
        let result = parse_npm_output(&output, ScriptTool::Unknown);
        assert!(
            result.is_passthrough(),
            "Unknown tool should return Passthrough"
        );
        // The leading newline is present because combine_output formats as
        // "{stdout}\n{stderr}" and stdout is empty.  This test documents and
        // locks in the current behaviour so any future change to combine_output
        // is visible.
        assert_eq!(
            result.content(),
            "\nerror: something went wrong\n",
            "combine_output with empty stdout produces a leading newline"
        );
    }

    // -----------------------------------------------------------------------
    // Known-tool branches: verify each arm calls the correct parser and does
    // not panic.  The assertion checks that the result is not None (i.e. the
    // parser ran and produced *some* tier), without prescribing which tier is
    // chosen — that belongs to the individual parser's own test suite.
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_npm_output_vitest_uses_vitest_parser() {
        // vitest_regex_fail.txt is a plain-text vitest summary (tier 2 input).
        let fixture = load_fixture("test", "vitest_regex_fail.txt");
        let output = make_output_full(&fixture, "", Some(1));
        let result = parse_npm_output(&output, ScriptTool::Vitest);
        // The vitest regex parser recognises this fixture — expect Degraded.
        assert!(
            result.is_degraded(),
            "Expected Degraded (regex tier) for vitest regex fixture, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_npm_output_jest_uses_vitest_parser() {
        // Jest delegates to the same vitest parser; the regex tier handles plain text.
        let fixture = load_fixture("test", "vitest_regex_fail.txt");
        let output = make_output_full(&fixture, "", Some(1));
        let result = parse_npm_output(&output, ScriptTool::Jest);
        assert!(
            result.is_degraded(),
            "Expected Degraded (regex tier) for jest with vitest-format fixture, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_npm_output_eslint_does_not_crash() {
        let fixture = load_fixture("lint", "eslint_fail.json");
        let output = make_output_full(&fixture, "", Some(1));
        let result = parse_npm_output(&output, ScriptTool::Eslint);
        // Eslint JSON fixture — expect Full parse.
        assert!(
            result.is_full(),
            "Expected Full parse for eslint JSON fixture, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_npm_output_biome_does_not_crash() {
        let fixture = load_fixture("lint", "biome_check_fail.json");
        let output = make_output_full(&fixture, "", Some(1));
        let result = parse_npm_output(&output, ScriptTool::Biome);
        // Biome JSON fixture — expect Full parse.
        assert!(
            result.is_full(),
            "Expected Full parse for biome JSON fixture, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_npm_output_prettier_produces_full() {
        // prettier_check_fail.txt uses `[warn]` format — tier 1 (Full) for prettier parser.
        let fixture = load_fixture("lint", "prettier_check_fail.txt");
        let output = make_output_full(&fixture, "", Some(1));
        let result = parse_npm_output(&output, ScriptTool::Prettier);
        assert!(
            result.is_full(),
            "Expected Full parse for prettier [warn] fixture (tier 1), got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_npm_output_oxlint_does_not_crash() {
        let fixture = load_fixture("lint", "oxlint_fail.json");
        let output = make_output_full(&fixture, "", Some(1));
        let result = parse_npm_output(&output, ScriptTool::Oxlint);
        // Oxlint JSON fixture — expect Full parse.
        assert!(
            result.is_full(),
            "Expected Full parse for oxlint JSON fixture, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_npm_output_tsc_produces_full() {
        // tsc_errors.txt contains `file(line,col): error TSxxxx` on stderr — tier 1 (Full).
        let fixture = load_fixture("build", "tsc_errors.txt");
        // tsc writes errors to stderr.
        let output = make_output_full("", &fixture, Some(2));
        let result = parse_npm_output(&output, ScriptTool::Tsc);
        assert!(
            result.is_full(),
            "Expected Full parse for tsc error fixture (tier 1 regex on stderr), got {}",
            result.tier_name()
        );
    }
}
