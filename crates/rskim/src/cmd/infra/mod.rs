//! Infrastructure tool handler — dispatches to infra parsers (#116, #117, #131, #168)
//!
//! Called via flat dispatch: `skim <tool> [args...]`. Supported tools:
//! `aws`, `curl`, `dig`, `docker`, `gh`, `kubectl`, `nslookup`, `terraform`, `wget`.

pub(crate) mod aws;
pub(crate) mod curl;
pub(crate) mod dns;
pub(crate) mod docker;
pub(crate) mod gh;
pub(crate) mod kubectl;
pub(crate) mod terraform;
pub(crate) mod wget;

use std::process::ExitCode;

use super::{ParsedCommandConfig, extract_show_stats, run_parsed_command_with_mode};
use crate::output::ParseResult;
use crate::output::canonical::InfraResult;
use crate::runner::CommandOutput;

/// Known infra tools that the infra handler can dispatch to.
const KNOWN_TOOLS: &[&str] = &[
    "aws",
    "curl",
    "dig",
    "docker",
    "gh",
    "kubectl",
    "nslookup",
    "terraform",
    "wget",
];

/// Entry point for `skim <tool> [args...]` (infra handler).
///
/// If no tool is specified or `--help` is passed, prints usage and exits.
/// Otherwise dispatches to the tool-specific handler.
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| a == "--help") {
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
        session_id: analytics.session_id.clone(),
    };

    match tool_name.as_str() {
        "aws" => aws::run(tool_args, &ctx),
        "curl" => curl::run(tool_args, &ctx),
        "dig" => dns::run_dig(tool_args, &ctx),
        "docker" => docker::run(tool_args, &ctx),
        "gh" => gh::run(tool_args, &ctx),
        "kubectl" => kubectl::run(tool_args, &ctx),
        "nslookup" => dns::run_nslookup(tool_args, &ctx),
        "terraform" => terraform::run(tool_args, &ctx),
        "wget" => wget::run(tool_args, &ctx),
        _ => {
            let safe_tool = super::sanitize_for_display(tool_name);
            eprintln!(
                "skim: unknown tool '{safe_tool}'\n\
                 Available tools: {}\n\
                 Run 'skim <tool> --help' for usage information",
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
    println!("skim <tool> [args...]");
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
    println!("  skim gh pr list              List GitHub pull requests");
    println!("  skim gh issue list           List GitHub issues");
    println!("  skim gh run list             List workflow runs");
    println!("  skim gh issue view 42        View GitHub issue details");
    println!("  skim gh pr view 15           View PR details");
    println!("  skim gh pr checks 15         View PR check status");
    println!("  skim gh run view 12345       View workflow run details");
    println!("  skim aws s3 ls               List S3 buckets");
    println!("  skim curl https://api.example.com/data  Make HTTP request");
    println!("  skim wget https://example.com/file.txt  Download a file");
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
    /// When `true`, skip ANSI escape stripping on the raw command output.
    ///
    /// `strip_ansi_escapes` treats ASCII control codes — including `\t` (0x09) —
    /// as part of escape sequences and drops them. DNS tools (dig, nslookup) use
    /// TABs as field separators in their structured output; stripping would remove
    /// those separators and cause record-line regex to fail, falling through to
    /// Passthrough. Set `true` for dig and nslookup.
    pub skip_ansi_strip: bool,
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
            output_format: ctx.output_format(),
            family: "infra",
            skip_ansi_strip: config.skip_ansi_strip,
            rec: crate::analytics::RecordingContext {
                enabled: ctx.analytics_enabled,
                command_type: crate::analytics::CommandType::Infra,
                parse_tier: None,
                session_id: ctx.session_id.as_deref(),
            },
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
///
/// # Analytics label format
///
/// Labels are produced as `"skim infra <tool> <subcommand> [args]"` — the
/// `"infra"` family prefix is retained for **backwards-compatible analytics
/// grouping** even though the user-facing CLI no longer exposes the `infra`
/// category (commands are now invoked as `skim gh …`, not `skim infra gh …`).
/// Changing the prefix would silently break historical trend data stored in the
/// analytics database.  The divergence between the label and the CLI surface is
/// intentional and must not be "fixed" to match the current CLI syntax.
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

/// Skip global flags in an args slice to find the first subcommand index.
///
/// Returns the index of the first non-flag token within `args`.
/// `value_flags` lists flags that consume the following token as a value
/// (e.g. `"-n"`, `"--namespace"` for kubectl), so both the flag and its
/// value are skipped.  Tokens matching `--flag=value` form are skipped
/// without consuming a second token.  All other flag tokens are skipped
/// as boolean flags.
///
/// Used by kubectl and docker handlers so that `kubectl -n ns get pods`
/// dispatches correctly to the `get` sub-parser rather than seeing `-n`
/// as the subcommand.
pub(crate) fn find_subcommand_index(args: &[String], value_flags: &[&str]) -> usize {
    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if arg.starts_with("--") && arg.contains('=') {
            idx += 1;
            continue;
        }
        if value_flags.iter().any(|&f| arg == f) {
            idx += 2;
            continue;
        }
        if arg.starts_with('-') {
            idx += 1;
            continue;
        }
        return idx;
    }
    args.len()
}

/// Re-export the shared `combine_output` under the name callers expect.
pub(crate) use super::combine_output as combine_stdout_stderr;

/// Passthrough parser — returns raw combined stdout+stderr unchanged.
///
/// Used as the default arm in docker and kubectl dispatch when no sub-parser
/// matches the subcommand.
pub(crate) fn passthrough_parse(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);
    ParseResult::Passthrough(combined.into_owned())
}

/// Inject `--format json` into args unless a format flag is already present.
///
/// Shared by parsers that support `--format json` (docker ps, docker images).
/// Checks for both `--format` and `--format=<value>` forms.
pub(crate) fn inject_format_json(args: &mut Vec<String>) {
    let has_format = args
        .iter()
        .any(|a| a == "--format" || a.starts_with("--format="));
    if !has_format {
        args.push("--format".to_string());
        args.push("json".to_string());
    }
}

/// Convert a `LogResult` into an `InfraResult` for a given tool and operation.
///
/// Shared by docker/logs, docker/compose, and kubectl/logs so they don't each
/// duplicate this mapping.
pub(crate) fn log_result_to_infra(
    log_result: crate::output::canonical::LogResult,
    tool: &str,
    operation: &str,
) -> InfraResult {
    let summary = format!(
        "{} lines, {} unique",
        log_result.total_lines, log_result.unique_messages
    );
    let items = vec![crate::output::canonical::InfraItem {
        label: "log".to_string(),
        value: log_result.to_string(),
    }];
    InfraResult::new(tool.to_string(), operation.to_string(), summary, items)
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    // ========================================================================
    // build_streaming_label tests (PF-022)
    // ========================================================================

    // NOTE: Analytics labels intentionally retain the "infra" family prefix
    // (e.g. "skim infra gh run watch 12345") for backwards-compatible grouping
    // of historical data in the analytics database, even though the user-facing
    // CLI no longer exposes the "infra" category (commands are now invoked as
    // "skim gh …").  Do not update these expected values to match the current
    // CLI syntax — that would silently corrupt stored analytics trends.
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

    // ========================================================================
    // find_subcommand_index tests (Fix 3)
    // ========================================================================

    const KUBECTL_VALUE_FLAGS: &[&str] =
        &["--context", "-n", "--namespace", "--kubeconfig", "--server"];

    #[test]
    fn test_find_subcommand_index_no_global_flags() {
        let args: Vec<String> = vec!["get".into(), "pods".into()];
        assert_eq!(super::find_subcommand_index(&args, KUBECTL_VALUE_FLAGS), 0);
    }

    #[test]
    fn test_find_subcommand_index_namespace_short() {
        // `kubectl -n mynamespace get pods` → subcmd at index 2
        let args: Vec<String> = vec![
            "-n".into(),
            "mynamespace".into(),
            "get".into(),
            "pods".into(),
        ];
        assert_eq!(super::find_subcommand_index(&args, KUBECTL_VALUE_FLAGS), 2);
    }

    #[test]
    fn test_find_subcommand_index_namespace_long() {
        // `kubectl --namespace production get pods` → subcmd at index 2
        let args: Vec<String> = vec!["--namespace".into(), "production".into(), "get".into()];
        assert_eq!(super::find_subcommand_index(&args, KUBECTL_VALUE_FLAGS), 2);
    }

    #[test]
    fn test_find_subcommand_index_context_and_namespace() {
        // `kubectl --context prod -n ns get pods` → subcmd at index 4
        let args: Vec<String> = vec![
            "--context".into(),
            "prod".into(),
            "-n".into(),
            "ns".into(),
            "get".into(),
            "pods".into(),
        ];
        assert_eq!(super::find_subcommand_index(&args, KUBECTL_VALUE_FLAGS), 4);
    }

    #[test]
    fn test_find_subcommand_index_equals_form_skipped() {
        // `kubectl --context=prod get pods` → subcmd at index 1
        let args: Vec<String> = vec!["--context=prod".into(), "get".into(), "pods".into()];
        assert_eq!(super::find_subcommand_index(&args, KUBECTL_VALUE_FLAGS), 1);
    }

    #[test]
    fn test_find_subcommand_index_empty_args() {
        let args: Vec<String> = vec![];
        assert_eq!(super::find_subcommand_index(&args, KUBECTL_VALUE_FLAGS), 0);
    }
}
