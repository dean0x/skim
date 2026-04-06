//! File operations subcommand dispatcher (#116)
//!
//! Routes `skim file <tool> [args...]` to the appropriate file tool parser.
//! Currently supported tools: `find`, `grep`, `ls`, `rg`, `tree`.

pub(crate) mod find;
pub(crate) mod grep;
pub(crate) mod ls;
pub(crate) mod rg;

use std::io::IsTerminal;
use std::process::ExitCode;

use std::collections::BTreeMap;

use super::{extract_show_stats, run_parsed_command_with_mode, OutputFormat, ParsedCommandConfig};
use crate::output::canonical::FileResult;
use crate::output::ParseResult;
use crate::runner::CommandOutput;

/// Known file tools that `skim file` can dispatch to.
const KNOWN_TOOLS: &[&str] = &["find", "grep", "ls", "rg", "tree"];

/// Maximum path/match entries shown in output (truncation cap).
pub(crate) const MAX_DISPLAY_ENTRIES: usize = 100;

/// Maximum lines accepted from a single tool invocation.
pub(crate) const MAX_INPUT_LINES: usize = 100_000;

/// Entry point for `skim file <tool> [args...]`.
///
/// If no tool is specified or `--help` / `-h` is passed, prints usage
/// and exits. Otherwise dispatches to the tool-specific handler.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
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

    match tool_name.as_str() {
        "find" => find::run(tool_args, show_stats, json_output),
        "grep" => grep::run(tool_args, show_stats, json_output),
        "ls" => ls::run(tool_args, show_stats, json_output, "ls"),
        "rg" => rg::run(tool_args, show_stats, json_output),
        "tree" => ls::run(tool_args, show_stats, json_output, "tree"),
        _ => {
            let safe_tool = super::sanitize_for_display(tool_name);
            eprintln!(
                "skim file: unknown tool '{safe_tool}'\n\
                 Available tools: {}\n\
                 Run 'skim file --help' for usage information",
                KNOWN_TOOLS.join(", ")
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("skim file <tool> [args...]");
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
    println!("  skim file find . -name '*.rs'       Find Rust files");
    println!("  skim file ls -la                    List files with details");
    println!("  skim file tree src/                 Directory tree");
    println!("  skim file grep -rn 'TODO' src/      Grep recursively");
    println!("  skim file rg 'fn main' src/         Ripgrep search");
}

// ============================================================================
// Shared file tool execution helper
// ============================================================================

/// Static configuration for a file tool binary.
pub(crate) struct FileToolConfig<'a> {
    /// Binary name of the tool (e.g., "find", "rg").
    pub program: &'a str,
    /// Environment variable overrides for the child process.
    pub env_overrides: &'a [(&'a str, &'a str)],
    /// Hint printed when the tool binary is not found.
    pub install_hint: &'a str,
}

/// Execute a file tool, parse its output, and emit the result.
///
/// Shared implementation for all file parsers, mirroring `run_infra_tool`.
pub(crate) fn run_file_tool(
    config: FileToolConfig<'_>,
    args: &[String],
    show_stats: bool,
    json_output: bool,
    prepare_args: impl FnOnce(&mut Vec<String>),
    parse_fn: impl FnOnce(&CommandOutput) -> ParseResult<FileResult>,
) -> anyhow::Result<ExitCode> {
    let mut cmd_args = args.to_vec();
    prepare_args(&mut cmd_args);

    let use_stdin = !std::io::stdin().is_terminal() && args.is_empty();
    let output_format = if json_output {
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
            show_stats,
            command_type: crate::analytics::CommandType::FileOps,
            output_format,
        },
        |output, _args| parse_fn(output),
    )
}

// ============================================================================
// Shared result builder for grep/rg parsers
// ============================================================================

/// Build a [`FileResult`] from grouped file matches.
///
/// `tool` — binary name (e.g. `"grep"`, `"rg"`).
/// `total_matches` — total match count across all files.
/// `file_matches` — map from file path to formatted match lines (already capped per-file).
/// `max_files` — maximum number of files to include in entries.
/// `max_per_file` — maximum match lines shown per file (used only for Vec capacity hint).
pub(super) fn build_file_result(
    tool: &str,
    total_matches: usize,
    file_matches: BTreeMap<String, Vec<String>>,
    max_files: usize,
    max_per_file: usize,
) -> Option<FileResult> {
    let file_count = file_matches.len();
    if file_count == 0 {
        return None;
    }
    let shown_files = file_count.min(max_files);

    let mut shown_matches = 0usize;
    let mut entries: Vec<String> = Vec::with_capacity(shown_files * (max_per_file + 1));
    for (file, matches) in file_matches.iter().take(max_files) {
        entries.push(file.clone());
        shown_matches += matches.len();
        entries.extend(matches.iter().cloned());
    }

    let footer = if file_count > max_files {
        Some(format!("... and {} more files", file_count - max_files))
    } else {
        None
    };

    let summary = format!(
        "{}: {total_matches} matches in {file_count} files (showing {shown_files})",
        tool.to_uppercase()
    );
    let mut all_entries = vec![summary];
    all_entries.extend(entries);

    Some(FileResult::new(
        tool.to_string(),
        total_matches,
        shown_matches,
        all_entries,
        footer,
    ))
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
