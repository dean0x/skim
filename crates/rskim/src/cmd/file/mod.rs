//! File operations handler — dispatches to file tool parsers (#116)
//!
//! Called via flat dispatch: `skim <tool> [args...]`. Supported tools:
//! `df`, `diff`, `du`, `env`, `find`, `grep`, `ls`, `printenv`, `ps`, `rg`, `tree`, `wc`.

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

use std::collections::BTreeMap;

use super::extract_show_stats;
use crate::output::canonical::FileResult;

/// Known file tools that the file handler can dispatch to.
const KNOWN_TOOLS: &[&str] = &[
    "df", "diff", "du", "env", "find", "grep", "ls", "printenv", "ps", "rg", "tree", "wc",
];

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
// Shared grep/rg regex constants and parser
// ============================================================================

/// Matches `file:line_number:content` format produced by both `grep -n` and `rg`.
pub(super) static RE_FILE_LINE_CONTENT: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^([^:]+):(\d+):(.*)$").unwrap());

/// Parse `file:line:content` text output shared by grep and rg.
///
/// Groups ALL matches by file path — grouping is re-encoding, never truncation:
/// every match line in the input appears in the output. (#317)
///
/// `tool` — binary name used in the result summary (e.g. `"grep"`, `"rg"`).
/// `text` — raw stdout from the tool.
/// `fallback_label` — when `Some`, lines that do not match the regex are
///   bucketed under this label (e.g. `"<stdin>"` when grep ran with no file
///   operands, `"(no filename)"` under `-h`). When `None`, an unattributable
///   line aborts the structured parse (returns `None`) so the caller degrades
///   to lossless `Passthrough` instead of dropping or mislabeling lines.
pub(super) fn try_parse_file_line_content(
    tool: &str,
    text: &str,
    fallback_label: Option<&str>,
) -> Option<FileResult> {
    if text.lines().nth(MAX_INPUT_LINES).is_some() {
        return None;
    }

    let mut file_matches: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut binary_notices: Vec<String> = Vec::new();
    let mut total_matches = 0usize;

    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line == "--" {
            // Context-group separator (grep -A/-B/-C) — pure formatting noise.
            continue;
        }
        if line.starts_with("Binary file ") {
            binary_notices.push(line.to_string());
            continue;
        }
        if let Some(caps) = RE_FILE_LINE_CONTENT.captures(line) {
            let file = caps[1].to_string();
            let lineno = &caps[2];
            let content = caps[3].trim();
            total_matches += 1;
            file_matches
                .entry(file)
                .or_default()
                .push(format!("  :{lineno}: {content}"));
        } else if let Some(label) = fallback_label {
            total_matches += 1;
            file_matches
                .entry(label.to_string())
                .or_default()
                .push(format!("  {line}"));
        } else {
            return None;
        }
    }

    if total_matches == 0 {
        return None;
    }

    build_file_result(tool, total_matches, file_matches, binary_notices)
}

// ============================================================================
// Shared result builder for grep/rg parsers
// ============================================================================

/// Build a [`FileResult`] from grouped file matches.
///
/// Emits every match in every file — no per-file or file-count caps, no
/// elision footer. `shown_count == total_count` so the canonical header
/// renders as `tool N` (no truncation ratio; Fix F). (#317)
///
/// `tool` — binary name (e.g. `"grep"`, `"rg"`).
/// `total_matches` — total match count across all files.
/// `file_matches` — map from file path to formatted match lines.
/// `extra_entries` — verbatim lines appended after the groups (e.g.
///   `Binary file x matches` notices); not counted as matches.
pub(super) fn build_file_result(
    tool: &str,
    total_matches: usize,
    file_matches: BTreeMap<String, Vec<String>>,
    extra_entries: Vec<String>,
) -> Option<FileResult> {
    let file_count = file_matches.len();
    if file_count == 0 && extra_entries.is_empty() {
        return None;
    }

    let summary = format!(
        "{}: {total_matches} matches in {file_count} files",
        tool.to_uppercase()
    );
    let mut all_entries = vec![summary];
    for (file, matches) in &file_matches {
        all_entries.push(file.clone());
        all_entries.extend(matches.iter().cloned());
    }
    all_entries.extend(extra_entries);

    Some(FileResult::new(
        tool.to_string(),
        total_matches,
        total_matches,
        all_entries,
        None,
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
