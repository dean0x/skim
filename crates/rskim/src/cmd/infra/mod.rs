//! Infrastructure tool subcommand dispatcher (#116, #131)
//!
//! Routes `skim infra <tool> [args...]` to the appropriate infra tool parser.
//! Currently supported tools: `aws`, `curl`, `gh`, `wget`.
//!
//! The `gh` handler supports list commands (`pr list`, `issue list`, `run list`)
//! and view commands (`issue view`, `pr view`, `pr checks`, `run view`).

pub(crate) mod aws;
pub(crate) mod curl;
pub(crate) mod gh;
pub(crate) mod wget;

use std::io::IsTerminal;
use std::process::ExitCode;

use super::{extract_show_stats, run_parsed_command_with_mode, OutputFormat, ParsedCommandConfig};
use crate::output::canonical::InfraResult;
use crate::output::ParseResult;
use crate::runner::CommandOutput;

/// Known infra tools that `skim infra` can dispatch to.
const KNOWN_TOOLS: &[&str] = &["aws", "curl", "gh", "wget"];

/// Entry point for `skim infra <tool> [args...]`.
///
/// If no tool is specified or `--help` / `-h` is passed, prints usage
/// and exits. Otherwise dispatches to the tool-specific handler.
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let (filtered_args, show_stats) = extract_show_stats(args);
    let (filtered_args, json_output) = super::extract_json_flag(&filtered_args);

    let Some((tool_name, tool_args)) = filtered_args.split_first() else {
        print_help();
        return Ok(ExitCode::SUCCESS);
    };

    let ctx = super::RunContext {
        show_stats,
        json_output,
        analytics_enabled: analytics.enabled,
    };

    match tool_name.as_str() {
        "aws" => aws::run(tool_args, &ctx),
        "curl" => curl::run(tool_args, &ctx),
        "gh" => gh::run(tool_args, &ctx),
        "wget" => wget::run(tool_args, &ctx),
        _ => {
            let safe_tool = super::sanitize_for_display(tool_name);
            eprintln!(
                "skim infra: unknown tool '{safe_tool}'\n\
                 Available tools: {}\n\
                 Run 'skim infra --help' for usage information",
                KNOWN_TOOLS.join(", ")
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("skim infra <tool> [args...]");
    println!();
    println!("  Run infrastructure tools and parse the output for AI context windows.");
    println!();
    println!("Available tools:");
    for tool in KNOWN_TOOLS {
        println!("  {tool}");
    }
    println!();
    println!("Flags:");
    println!("  --json          Emit structured JSON output");
    println!("  --show-stats    Show token statistics");
    println!();
    println!("Examples:");
    println!("  skim infra gh pr list              List GitHub pull requests");
    println!("  skim infra gh issue list           List GitHub issues");
    println!("  skim infra gh run list             List workflow runs");
    println!("  skim infra gh issue view 42        View GitHub issue details");
    println!("  skim infra gh pr view 15           View PR details");
    println!("  skim infra gh pr checks 15         View PR check status");
    println!("  skim infra gh run view 12345       View workflow run details");
    println!("  skim infra aws s3 ls               List S3 buckets");
    println!("  skim infra curl https://api.example.com/data  Make HTTP request");
    println!("  skim infra wget https://example.com/file.txt  Download a file");
}

// ============================================================================
// Shared infra tool execution helper
// ============================================================================

/// Static configuration for an infra tool binary.
pub(crate) struct InfraToolConfig<'a> {
    /// Binary name of the tool (e.g., "gh", "aws").
    pub program: &'a str,
    /// Environment variable overrides for the child process.
    pub env_overrides: &'a [(&'a str, &'a str)],
    /// Hint printed when the tool binary is not found.
    pub install_hint: &'a str,
}

/// Execute an infra tool, parse its output, and emit the result.
///
/// This is the single implementation shared by all infra parsers, handling both
/// text and JSON output modes. It eliminates per-tool `run()` boilerplate by
/// delegating to [`super::run_parsed_command_with_mode`].
pub(crate) fn run_infra_tool(
    config: InfraToolConfig<'_>,
    args: &[String],
    ctx: &super::RunContext,
    prepare_args: impl FnOnce(&mut Vec<String>),
    parse_fn: impl FnOnce(&CommandOutput) -> ParseResult<InfraResult>,
) -> anyhow::Result<ExitCode> {
    let mut cmd_args = args.to_vec();
    prepare_args(&mut cmd_args);

    let use_stdin = !std::io::stdin().is_terminal() && args.is_empty();
    let output_format = if ctx.json_output {
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
            show_stats: ctx.show_stats,
            command_type: crate::analytics::CommandType::Infra,
            output_format,
            analytics_enabled: ctx.analytics_enabled,
            family: "infra",
        },
        |output, _args| parse_fn(output),
    )
}

/// Re-export the shared `combine_output` under the name callers expect.
pub(crate) use super::combine_output as combine_stdout_stderr;

/// Build the clap `Command` definition for shell completions.
///
/// Models `tool` as a positional value with the known tool names so that
/// `skim infra <TAB>` suggests `aws`, `curl`, `gh`, `wget`.
pub(super) fn command() -> clap::Command {
    clap::Command::new("infra")
        .about("Run infrastructure tools and parse output for AI context windows")
        .arg(
            clap::Arg::new("tool")
                .value_name("TOOL")
                .value_parser(["aws", "curl", "gh", "wget"])
                .help("Infrastructure tool to run (aws, curl, gh, wget)"),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Emit structured JSON output"),
        )
        .arg(
            clap::Arg::new("show-stats")
                .long("show-stats")
                .action(clap::ArgAction::SetTrue)
                .help("Show token statistics"),
        )
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    // sanitize_for_display is now in crate::cmd; tests remain here as
    // a usage-site smoke-check to catch regressions at the call site.
    #[test]
    fn test_sanitize_for_display_clean_input() {
        assert_eq!(
            super::super::sanitize_for_display("hello-world"),
            "hello-world"
        );
    }

    #[test]
    fn test_sanitize_for_display_rejects_non_ascii() {
        let input = "tool\x1b[31mred\x1b[0m";
        let sanitized = super::super::sanitize_for_display(input);
        assert!(!sanitized.contains('\x1b'));
    }

    #[test]
    fn test_sanitize_for_display_truncates_at_64() {
        let long_input = "a".repeat(100);
        let sanitized = super::super::sanitize_for_display(&long_input);
        assert_eq!(sanitized.len(), 64);
    }
}
