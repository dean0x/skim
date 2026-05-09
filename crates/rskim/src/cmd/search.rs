//! Search subcommand — code search via layered n-gram indexing.
//!
//! This is a CLI stub wiring the `search` subcommand into the dispatch table.
//! The full search implementation lives in `rskim-search` library crate.
//!
//! # Architecture
//!
//! All I/O lives here (this file). Business logic lives in:
//! - `rskim-search` crate: types, traits, indexing layer implementations
//!
//! Search is not yet implemented; this stub allows the subcommand to be
//! registered, help text to be discoverable, and the dispatch sync guard
//! test to pass.

use std::process::ExitCode;

// ============================================================================
// Public entry point
// ============================================================================

/// Run the `skim search` subcommand.
///
/// Currently a stub: prints help when invoked with no args or `--help`, and
/// returns `ExitCode::FAILURE` with an informative message for all other inputs.
pub(crate) fn run(
    args: &[String],
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    // No args or --help/-h → print help
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Has a query arg → not yet implemented
    eprintln!("skim search: not yet implemented");
    Ok(ExitCode::FAILURE)
}

// ============================================================================
// Help text
// ============================================================================

fn print_help() {
    eprintln!(
        "\
Usage: skim search [OPTIONS] <QUERY>

Search code using layered n-gram indexing.

Arguments:
  <QUERY>    Search query string

Options:
  --lang <LANG>    Filter by language (e.g., rust, typescript)
  --ast <PATTERN>  AST pattern to match
  --json           Output results as JSON
  --limit <N>      Maximum number of results (default: 20)
  -h, --help       Print this help message

Examples:
  skim search \"fn parse\"
  skim search --lang rust \"impl Iterator\"
  skim search --ast \"function_declaration\" --json"
    );
}
