//! Database tool handler — dispatches to DB parsers (#117)
//!
//! Called via flat dispatch: `skim <tool> [args...]`. Supported tools:
//! `mysql`, `psql`, `sqlite3`.
//!
//! Each tool parses tabular query output into a compact [`DbResult`],
//! reducing verbose table borders and row footers to structured column/row data.

pub(crate) mod mysql;
pub(crate) mod psql;
pub(crate) mod sqlite3;

use std::process::ExitCode;

use super::{
    extract_json_flag, extract_show_stats, run_parsed_command_with_mode, ParsedCommandConfig,
};
use crate::output::canonical::DbResult;
use crate::output::ParseResult;
use crate::runner::CommandOutput;

/// Known DB tools that the db handler can dispatch to.
const KNOWN_TOOLS: &[&str] = &["mysql", "psql", "sqlite3"];

/// Entry point for `skim <tool> [args...]` (db handler).
///
/// If no tool is specified or `--help` is passed, prints usage and exits.
/// `-h` is intentionally NOT intercepted here: DB tools use `-h` as a
/// hostname flag (`psql -h localhost`, `mysql -h host`). Otherwise
/// dispatches to the tool-specific handler.
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| a == "--help") {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let (filtered_args, show_stats) = extract_show_stats(args);
    // DB tools don't use --json <value> semantics (unlike gh), so the simple
    // extract_json_flag is correct here — it treats --json as a boolean flag,
    // not a key=value pair like extract_infra_json_flag does.
    let (filtered_args, json_output) = extract_json_flag(&filtered_args);

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
        "mysql" => mysql::run(tool_args, &ctx),
        "psql" => psql::run(tool_args, &ctx),
        "sqlite3" => sqlite3::run(tool_args, &ctx),
        _ => {
            let safe_tool = super::sanitize_for_display(tool_name);
            eprintln!(
                "skim: unknown db tool '{safe_tool}'\n\
                 Available tools: {}\n\
                 Run 'skim <tool> --help' for usage information",
                KNOWN_TOOLS.join(", ")
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("skim <tool> [args...]");
    println!();
    println!("  Run database tools and compress the output for AI context windows.");
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
    println!("  skim psql -c \"SELECT * FROM users LIMIT 10\"");
    println!("  skim mysql -e \"SELECT * FROM orders LIMIT 10\"");
    println!("  skim sqlite3 app.db \"SELECT * FROM logs LIMIT 20\"");
}

// ============================================================================
// Shared DB tool execution helper
// ============================================================================

/// Static configuration for a DB tool binary.
pub(crate) struct DbToolConfig<'a> {
    /// Binary name of the tool (e.g., "psql", "mysql").
    pub program: &'a str,
    /// Environment variable overrides for the child process (e.g. pager suppression).
    pub env_overrides: &'a [(&'a str, &'a str)],
    /// Hint printed when the tool binary is not found.
    pub install_hint: &'a str,
}

/// Execute a DB tool, parse its output, and emit the result.
///
/// Parallel to [`crate::cmd::infra::run_infra_tool`] but uses [`CommandType::Db`]
/// and `family: "db"` for analytics labelling.
pub(crate) fn run_db_tool(
    config: DbToolConfig<'_>,
    args: &[String],
    ctx: &super::RunContext,
    prepare_args: impl FnOnce(&mut Vec<String>),
    parse_fn: impl FnOnce(&CommandOutput) -> ParseResult<DbResult>,
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
            family: "db",
            // DB tools emit tab-separated (TSV) output; stripping ANSI would
            // drop tab characters and break the TSV parser. See ParsedCommandConfig
            // docs for full explanation.
            skip_ansi_strip: true,
            rec: crate::analytics::RecordingContext {
                enabled: ctx.analytics_enabled,
                command_type: crate::analytics::CommandType::Db,
                parse_tier: None,
                session_id: ctx.session_id.as_deref(),
            },
        },
        |output, _args| parse_fn(output),
    )
}
