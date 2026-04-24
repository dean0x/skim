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

use std::process::ExitCode;

use super::{extract_show_stats, run_parsed_command_with_mode, ParsedCommandConfig};
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
    let (filtered_args, json_output) = extract_infra_json_flag(&filtered_args);

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

/// Extract `--json` for skim's output format, preserving `--json <value>`
/// pairs that belong to the underlying tool (e.g., `gh --json fields`).
///
/// Bare `--json` (at end of args or followed by another flag) is skim's
/// boolean flag. `--json <value>` (followed by a non-flag token) is the
/// tool's flag and both tokens are preserved in the output.
///
/// Scoped to infra rather than replacing the global [`super::extract_json_flag`]
/// because lint tests use `["--json", "eslint"]` where the tool name follows
/// `--json` — a value-aware heuristic would misidentify it as a `--json` value.
/// Among infra tools, only `gh` uses `--json <value>` for field selection.
///
/// # Known limitation
///
/// The heuristic classifies the next token as a *value* only when it does not
/// start with `-`. A field-selector string that begins with a dash (e.g., a
/// hypothetical `-fieldname`) would be misidentified as a flag, causing skim
/// to treat the `--json` as its own boolean flag rather than forwarding the
/// pair to the underlying tool. In practice `gh --json` selectors are
/// comma-separated identifiers (e.g., `number,title,state`) and never begin
/// with a dash, so this edge case is not expected to occur.
fn extract_infra_json_flag(args: &[String]) -> (Vec<String>, bool) {
    let mut filtered: Vec<String> = Vec::with_capacity(args.len());
    let mut is_json = false;
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--json" {
            let next_is_value = iter.peek().is_some_and(|next| !next.starts_with('-'));
            if next_is_value {
                filtered.push(arg.clone());
                filtered.push(iter.next().unwrap().clone());
            } else {
                is_json = true;
            }
        } else {
            filtered.push(arg.clone());
        }
    }
    (filtered, is_json)
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

    let use_stdin = super::should_read_stdin(args);

    run_parsed_command_with_mode(
        ParsedCommandConfig {
            program: config.program,
            args: &cmd_args,
            env_overrides: config.env_overrides,
            install_hint: config.install_hint,
            use_stdin,
            show_stats: ctx.show_stats,
            command_type: crate::analytics::CommandType::Infra,
            output_format: ctx.output_format(),
            analytics_enabled: ctx.analytics_enabled,
            family: "infra",
        },
        |output, _args| parse_fn(output),
    )
}

/// Build an analytics label for streaming infra commands.
///
/// Delegates to [`super::format_analytics_label`] for a consistent label format
/// across streaming and non-streaming infra commands.  Scrubs credential-bearing
/// URLs from args before joining so that tokens are never written to the analytics
/// database or shown in stats output.  Returns an empty string when analytics is
/// disabled (avoids unnecessary formatting).  SEE: PF-022.
pub(crate) fn build_streaming_label(
    family: &str,
    program: &str,
    subcommand: &str,
    args: &[String],
    show_stats: bool,
    analytics_enabled: bool,
) -> String {
    if !show_stats && !analytics_enabled {
        return String::new();
    }
    let rest = if args.is_empty() {
        subcommand.to_string()
    } else {
        let scrubbed: Vec<std::borrow::Cow<'_, str>> = args
            .iter()
            .map(|a| crate::cmd::git::shared::scrub_credential_url(a))
            .collect();
        format!("{subcommand} {}", scrubbed.join(" "))
    };
    super::format_analytics_label(family, program, &rest)
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

    // ========================================================================
    // build_streaming_label tests (PF-022)
    // ========================================================================

    #[test]
    fn test_build_streaming_label_with_args() {
        let args: Vec<String> = vec!["12345".to_string()];
        let label = super::build_streaming_label("infra", "gh", "run watch", &args, true, true);
        assert_eq!(label, "skim infra gh run watch 12345");
    }

    #[test]
    fn test_build_streaming_label_no_args() {
        let args: Vec<String> = vec![];
        let label = super::build_streaming_label("infra", "gh", "run watch", &args, true, true);
        assert_eq!(label, "skim infra gh run watch");
    }

    #[test]
    fn test_build_streaming_label_disabled_analytics_returns_empty() {
        let args: Vec<String> = vec!["12345".to_string()];
        // Both show_stats and analytics_enabled are false → empty string.
        let label = super::build_streaming_label("infra", "gh", "run watch", &args, false, false);
        assert_eq!(label, "");
    }

    #[test]
    fn test_build_streaming_label_show_stats_enables_label() {
        // show_stats=true with analytics_enabled=false still produces a label
        // (label is used for display output as well as recording).
        let args: Vec<String> = vec![];
        let label = super::build_streaming_label("infra", "gh", "api", &args, true, false);
        assert_eq!(label, "skim infra gh api");
    }

    /// Credential-bearing URLs in args must be scrubbed from the analytics label.
    ///
    /// A streaming command like `skim infra gh api https://token@github.com/repo`
    /// must not write the token to the analytics DB or stats output.
    #[test]
    fn test_build_streaming_label_scrubs_credentials() {
        let args: Vec<String> = vec!["https://ghp_secret@github.com/org/repo".to_string()];
        let label = super::build_streaming_label("infra", "gh", "api", &args, true, true);
        assert!(
            !label.contains("ghp_secret"),
            "credential must be scrubbed from label: {label}"
        );
        assert!(
            label.contains("github.com/org/repo"),
            "host/path must be preserved in label: {label}"
        );
    }

    // ========================================================================
    // extract_infra_json_flag tests
    // ========================================================================

    #[test]
    fn test_extract_infra_json_bare_at_end() {
        let args: Vec<String> = vec!["gh".into(), "run".into(), "--json".into()];
        let (filtered, is_json) = super::extract_infra_json_flag(&args);
        assert!(is_json);
        assert_eq!(filtered, vec!["gh", "run"]);
    }

    #[test]
    fn test_extract_infra_json_with_value_preserved() {
        let args: Vec<String> = vec![
            "gh".into(),
            "run".into(),
            "--json".into(),
            "databaseId,status".into(),
        ];
        let (filtered, is_json) = super::extract_infra_json_flag(&args);
        assert!(!is_json);
        assert_eq!(filtered, vec!["gh", "run", "--json", "databaseId,status"]);
    }

    #[test]
    fn test_extract_infra_json_before_flag_stripped() {
        let args: Vec<String> = vec!["gh".into(), "--json".into(), "--verbose".into()];
        let (filtered, is_json) = super::extract_infra_json_flag(&args);
        assert!(is_json);
        assert_eq!(filtered, vec!["gh", "--verbose"]);
    }

    #[test]
    fn test_extract_infra_json_absent() {
        let args: Vec<String> = vec!["gh".into(), "run".into(), "list".into()];
        let (filtered, is_json) = super::extract_infra_json_flag(&args);
        assert!(!is_json);
        assert_eq!(filtered, vec!["gh", "run", "list"]);
    }

    #[test]
    fn test_extract_infra_json_single_field() {
        let args: Vec<String> = vec!["gh".into(), "run".into(), "--json".into(), "status".into()];
        let (filtered, is_json) = super::extract_infra_json_flag(&args);
        assert!(!is_json);
        assert_eq!(filtered, vec!["gh", "run", "--json", "status"]);
    }

    #[test]
    fn test_extract_infra_json_multiple_json_tokens() {
        // First --json is tool's field selector (value follows), second is skim's bare flag.
        // Expected: value pair preserved, bare flag extracted, is_json = true.
        let args: Vec<String> = vec![
            "gh".into(),
            "run".into(),
            "--json".into(),
            "fields".into(),
            "--json".into(),
        ];
        let (filtered, is_json) = super::extract_infra_json_flag(&args);
        assert!(is_json);
        assert_eq!(filtered, vec!["gh", "run", "--json", "fields"]);
    }
}
