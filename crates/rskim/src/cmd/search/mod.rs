//! Search subcommand — code search via layered n-gram indexing.
//!
//! # Architecture
//!
//! All I/O lives here (this module). Business logic is split across:
//! - `types` — shared configuration and result types
//! - `walk` — project-root discovery and file traversal
//! - `manifest` — JSONL sidecar for incremental build caching
//! - `index` — full pipeline orchestration (`skim search index`)
//! - `rskim-search` crate — index building, n-gram extraction, BM25F scoring

mod index;
mod manifest;
mod types;
mod walk;

use std::process::ExitCode;

// ============================================================================
// Public entry point
// ============================================================================

/// Run the `skim search` subcommand.
///
/// Dispatches to:
/// - `skim search index [OPTIONS]` — build or update the search index
/// - `skim search [OPTIONS] <QUERY>` — (not yet implemented)
/// - No args / `--help` / `-h` — print help
pub(crate) fn run(
    args: &[String],
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    // `skim search index [OPTIONS]` — build the index (checked before --help so
    // that `skim search index --help` is handled by index::run, not this parent).
    if args.first().is_some_and(|a| a == "index") {
        let rest = &args[1..];
        return index::run(rest);
    }

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
    println!(
        "\
Usage: skim search <SUBCOMMAND|QUERY> [OPTIONS]

Search code using layered n-gram indexing.

Subcommands:
  index    Build or update the search index for the current project

Arguments:
  <QUERY>    Search query string (direct query mode, index must exist)

Options:
  --lang <LANG>    Filter by language (e.g., rust, typescript)
  --ast <PATTERN>  AST pattern to match
  --json           Output results as JSON
  --limit <N>      Maximum number of results (default: 20)
  -h, --help       Print this help message

Examples:
  skim search index              Build the search index
  skim search index --force      Rebuild from scratch
  skim search \"fn parse\"
  skim search --lang rust \"impl Iterator\"
  skim search --ast \"function_declaration\" --json"
    );
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::process::ExitCode;

    /// Stub analytics config for tests — analytics disabled, no cost override.
    const TEST_ANALYTICS: crate::analytics::AnalyticsConfig = crate::analytics::AnalyticsConfig {
        enabled: false,
        input_cost_per_mtok: None,
        session_id: None,
    };

    #[test]
    fn test_search_help_returns_success() {
        let result = run(&[], &TEST_ANALYTICS).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_search_help_flag_returns_success() {
        let result = run(&["--help".to_string()], &TEST_ANALYTICS).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_search_short_help_flag_returns_success() {
        let result = run(&["-h".to_string()], &TEST_ANALYTICS).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_search_unimplemented_returns_failure() {
        let result = run(&["fn parse".to_string()], &TEST_ANALYTICS).unwrap();
        assert_eq!(result, ExitCode::FAILURE);
    }

    /// Regression: `skim search index --help` must dispatch to index help,
    /// not the parent search help. The parent help check must not intercept
    /// flags intended for a known subcommand.
    #[test]
    fn test_index_help_dispatches_to_index_not_parent() {
        let result = run(
            &["index".to_string(), "--help".to_string()],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }
}
