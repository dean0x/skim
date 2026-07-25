//! File operations handler — dispatches to file tool parsers (#116)
//!
//! Called via flat dispatch: `skim <tool> [args...]`. Supported tools:
//! `df`, `diff`, `du`, `env`, `find`, `grep`, `ls`, `printenv`, `ps`, `rg`, `tree`, `wc`.
//!
//! ## Shared passthrough helper
//!
//! [`passthrough_parse`] is the single implementation of the byte-faithful native
//! passthrough contract (ADR-009) for pure-passthrough file handlers (grep, rg).
//! Both modules delegate to it so a future fidelity fix lands in one place.

use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

pub(crate) mod df;
pub(crate) mod diff;
pub(crate) mod du;
pub(crate) mod env;
pub(crate) mod find;
pub(crate) mod grep;
pub(crate) mod ls;
pub(crate) mod ps;
pub(crate) mod rg;
pub(crate) mod wc;

use std::process::ExitCode;

use super::extract_show_stats;

/// Known file tools that the file handler can dispatch to.
const KNOWN_TOOLS: &[&str] = &[
    "df", "diff", "du", "env", "find", "grep", "ls", "printenv", "ps", "rg", "tree", "wc",
];

/// Shared pure-passthrough parse helper for grep, rg, and any future
/// byte-faithful pass-through file handlers.
///
/// Returns [`ParseResult::RawPassthrough`] — a payload-less signal that
/// `execution.rs` should serve `CommandOutput::stdout` byte-faithfully without
/// cloning it into the parse result.  The byte-faithful contract (ADR-009) lives
/// in one place; a future fidelity fix (e.g., adjusting how TAB bytes are
/// handled) touches only this function, not N identical copies.
///
/// The `_output` parameter is intentionally ignored: the whole point of
/// `RawPassthrough` is that the parse result carries no payload — the original
/// `CommandOutput::stdout` buffer is served directly by the caller.
pub(super) fn passthrough_parse(_output: &CommandOutput) -> ParseResult<FileResult> {
    ParseResult::RawPassthrough
}

/// Maximum path/match entries shown in output (truncation cap).
pub(crate) const MAX_DISPLAY_ENTRIES: usize = 100;

/// Maximum lines accepted by structured parsers in this family.
///
/// Exceeding this bound never truncates: parsers return `None` so the caller
/// degrades to lossless `Passthrough` (raw output is already bounded at the
/// runner's 64 MiB cap). (#317)
pub(crate) const MAX_INPUT_LINES: usize = 100_000;

/// Entry point for `skim <tool> [args...]` (file handler).
///
/// If no tool is specified or `--help` is passed, prints usage and exits.
/// `-h` is intentionally NOT intercepted here: file tools use `-h` with
/// tool-level semantics (`grep -h` = no-filename, `ls -h`/`du -h`/`df -h` =
/// human-readable sizes), mirroring the `db/mod.rs` hostname-flag precedent.
/// Use `skim <tool> --help` to see this usage text.
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| a == "--help") {
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
        session_id: analytics.session_id.clone(),
    };

    match tool_name.as_str() {
        "df" => df::run(tool_args, &ctx),
        "diff" => diff::run(tool_args, &ctx),
        "du" => du::run(tool_args, &ctx),
        "env" | "printenv" => env::run(tool_args, &ctx),
        "find" => find::run(tool_args, &ctx),
        "grep" => grep::run(tool_args, &ctx),
        "ls" => ls::run(tool_args, &ctx, "ls"),
        "ps" => ps::run(tool_args, &ctx),
        "rg" => rg::run(tool_args, &ctx),
        "tree" => ls::run(tool_args, &ctx, "tree"),
        "wc" => wc::run(tool_args, &ctx),
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

fn print_help() {
    println!("skim <tool> [args...]");
    println!();
    println!("  Run file operation tools and parse the output for AI context windows.");
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
    println!("  skim find . -name '*.rs'       Find Rust files");
    println!("  skim ls -la                    List files with details");
    println!("  skim tree src/                 Directory tree");
    println!("  skim grep -rn 'TODO' src/      Grep recursively");
    println!("  skim rg 'fn main' src/         Ripgrep search");
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    #[test]
    fn test_sanitize_for_display_clean_input() {
        assert_eq!(crate::cmd::sanitize_for_display("find"), "find");
    }

    #[test]
    fn test_sanitize_for_display_rejects_non_ascii() {
        let input = "tool\x1b[31mred\x1b[0m";
        let sanitized = crate::cmd::sanitize_for_display(input);
        assert!(!sanitized.contains('\x1b'));
    }
}
