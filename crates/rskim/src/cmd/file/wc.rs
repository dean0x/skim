//! wc parser.
//!
//! Parses `wc` output (line/word/byte counts per file) into structured `FileResult`.
//!
//! Tiers:
//! - **Tier 1 (Full)**: Parse wc output lines, detect total, format entries
//! - **Tier 3 (Passthrough)**: Empty output on non-zero exit

use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

use super::{MAX_DISPLAY_ENTRIES, MAX_INPUT_LINES};
use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "wc",
    env_overrides: &[],
    install_hint: "wc is typically pre-installed on Unix systems",
    family: "file",
    skip_ansi_strip: false,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[],
    forward_stderr: true,
};

/// Matches full wc output: lines words bytes filename
static RE_WC_FULL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\d+)\s+(\d+)\s+(\d+)\s+(.+)$").unwrap());

/// Matches single-stat wc output: count filename (e.g., `wc -l`)
static RE_WC_SINGLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(\d+)\s+(.+)$").unwrap());

/// Run `skim wc [args...]`.
pub(crate) fn run(args: &[String], ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    run_tool(CONFIG, args, ctx, |_| {}, parse_impl)
}

/// Three-tier parse function for wc output.
fn parse_impl(output: &CommandOutput) -> ParseResult<FileResult> {
    // Non-zero exit with empty stdout is an error condition
    if output.exit_code != Some(0) && output.stdout.trim().is_empty() {
        return ParseResult::Passthrough(output.stdout.clone());
    }

    if let Some(result) = try_parse_wc(&output.stdout) {
        return ParseResult::Full(result);
    }

    ParseResult::Passthrough(output.stdout.clone())
}

// ============================================================================
// Tier 1: wc output parsing
// ============================================================================

fn try_parse_wc(stdout: &str) -> Option<FileResult> {
    if stdout.trim().is_empty() {
        return None;
    }

    let mut entries: Vec<String> = Vec::with_capacity(MAX_DISPLAY_ENTRIES);
    let mut total_count = 0usize;
    let mut footer_entry: Option<String> = None;

    for (i, line) in stdout.lines().enumerate() {
        if i >= MAX_INPUT_LINES {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Try full mode: lines words bytes filename
        if let Some(caps) = RE_WC_FULL.captures(trimmed) {
            let lines_n: u64 = caps[1].parse().unwrap_or(0);
            let words_n: u64 = caps[2].parse().unwrap_or(0);
            let bytes_n: u64 = caps[3].parse().unwrap_or(0);
            let filename = caps[4].trim();

            if filename == "total" {
                footer_entry = Some(format!(
                    "total: {lines_n} lines, {words_n} words, {bytes_n} bytes"
                ));
            } else {
                total_count += 1;
                if entries.len() < MAX_DISPLAY_ENTRIES {
                    entries.push(format!(
                        "{filename}: {lines_n} lines, {words_n} words, {bytes_n} bytes"
                    ));
                }
            }
            continue;
        }

        // Try single-stat mode: count filename (e.g., wc -l)
        if let Some(caps) = RE_WC_SINGLE.captures(trimmed) {
            let count: u64 = caps[1].parse().unwrap_or(0);
            let filename = caps[2].trim();

            if filename == "total" {
                footer_entry = Some(format!("total: {count}"));
            } else {
                total_count += 1;
                if entries.len() < MAX_DISPLAY_ENTRIES {
                    entries.push(format!("{count} {filename}"));
                }
            }
        }
    }

    // Handle stdin-only output (just a bare count with no filename)
    if total_count == 0 && entries.is_empty() && footer_entry.is_none() {
        // Try bare number (stdin mode)
        let trimmed = stdout.trim();
        if !trimmed.is_empty()
            && trimmed
                .chars()
                .all(|c| c.is_ascii_digit() || c.is_whitespace())
        {
            return Some(FileResult::new(
                "wc".to_string(),
                1,
                1,
                vec![trimmed.to_string()],
                None,
            ));
        }
        return None;
    }

    let shown_count = entries.len();
    let footer = crate::output::elision_marker(shown_count, total_count, "lines").or(footer_entry);

    Some(FileResult::new(
        "wc".to_string(),
        total_count,
        shown_count,
        entries,
        footer,
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_utils::{load_fixture, make_output_full};

    #[test]
    fn test_tier1_wc_full_mode() {
        let input = load_fixture("file", "wc_small.txt");
        let result = try_parse_wc(&input);
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.total_count, 5, "5 files, not counting total line");
        assert!(result.footer.is_some(), "total line should become footer");
        let footer = result.footer.as_ref().unwrap();
        assert!(footer.contains("total"), "footer should mention total");
    }

    #[test]
    fn test_tier1_wc_lines_only() {
        let input = load_fixture("file", "wc_lines_only.txt");
        let result = try_parse_wc(&input);
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.total_count, 5, "5 files in single-stat mode");
        // Each entry shows count then filename
        assert!(
            result.entries.iter().any(|e| e.contains("src/main.rs")),
            "entries should contain file names"
        );
    }

    #[test]
    fn test_tier1_wc_stdin_no_filename() {
        // wc with stdin input just produces a bare number
        let input = "      42\n";
        let result = try_parse_wc(input);
        assert!(result.is_some(), "Expected parse to succeed for stdin mode");
        let result = result.unwrap();
        assert_eq!(result.total_count, 1);
    }

    #[test]
    fn test_tier1_wc_total_as_footer() {
        let input = load_fixture("file", "wc_small.txt");
        let result = try_parse_wc(&input).unwrap();
        let footer = result.footer.as_ref().expect("total should become footer");
        assert!(
            footer.contains("total"),
            "Footer should reference the total line"
        );
        // The total line should NOT appear as a regular entry
        assert!(
            !result.entries.iter().any(|e| e == "total"),
            "total should not appear as a regular entry"
        );
    }

    #[test]
    fn test_tier3_empty_on_error() {
        let output = make_output_full("", "", Some(1));
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Empty output on error should be passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("file", "wc_small.txt");
        let output = make_output_full(&input, "", Some(0));
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "parse_impl with exit code 0 and valid wc output should return Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_display_format() {
        let input = load_fixture("file", "wc_small.txt");
        let result = try_parse_wc(&input).unwrap();
        let rendered = format!("{result}");
        assert!(
            rendered.contains("wc "),
            "Header should start with tool name"
        );
        assert!(
            rendered.contains("src/main.rs"),
            "Entries should appear in output"
        );
    }
}
